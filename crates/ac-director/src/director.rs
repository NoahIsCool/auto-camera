//! The per-tick decision procedure.
//!
//! [`Director::tick`] is a deterministic function of the configuration, the
//! tick's [`DirectorInput`] and a few timers the director owns. Time enters
//! only as `now` on the media clock; nothing reads a wall clock.
//!
//! Each tick:
//!
//! 1. **Candidates.** Duplicate ids keep their first occurrence. The program
//!    shot is looked up among them for its crop, kind and quality; a program
//!    shot that is missing or unavailable counts as quality 0.
//! 2. **Fallback timer.** While program quality is below
//!    `fallback_quality`, a timer runs; once it reaches `fallback_after` the
//!    director is in *fallback*. The timer restarts when program changes.
//! 3. **Eligibility.** A candidate may be suggested only if it is available,
//!    has a valid crop, has quality of at least `fallback_quality` (a weaker
//!    shot would itself trigger fallback), is not the program shot, is not
//!    a jump cut from program, and is not a shot the operator skipped
//!    within the last `skip` ([`operator`]).
//! 4. **Pool.** In fallback, or with nothing on program, the pool is the
//!    eligible wides (all eligible shots if there is no eligible wide).
//!    Otherwise it is every eligible shot.
//! 5. **Hysteresis.** Each pool member is scored with [`score`] (quality,
//!    prompt priority and recency). If the current suggestion is not in the
//!    pool it is replaced by the best-scoring pool member at once. Otherwise
//!    it changes only after the best challenger has led it by at least
//!    `margin` continuously for `hold`; the timer tracks the lead, not the
//!    challenger's identity, so near-equal challengers swapping places
//!    cannot starve a switch away from a clearly worse incumbent. Ties go to
//!    the earlier candidate in the input list.
//! 6. **Take (Auto only, never during a transition or while the operator's
//!    switching pauses it, [`operator`]).** With nothing on
//!    program, or in fallback, the suggestion is taken at once (fallback with
//!    a cut). Otherwise no take before the minimum dwell; after it, a take is
//!    due when program reaches its maximum dwell (wide or shot) or when the
//!    suggestion has beaten program's [`base_score`] by `margin` for `hold`.
//!    A due take waits until the engine has staged the suggestion on preview,
//!    so what is taken is what the operator saw.
//!
//! A step of `now` backwards is treated as a clock discontinuity and restarts
//! every timer, a skip window and a pause included. Off mode clears all
//! state, so re-enabling starts fresh.

use std::collections::HashSet;
use std::time::Duration;

use ac_core::ShotId;

use crate::candidate::{CandidateKind, ShotCandidate, base_score, is_jump_cut, score};
use crate::config::{ConfigError, DirectorConfig, DirectorMode};
use crate::decision::{DirectorDecision, Reason, TakeRequest, TransitionHint};

mod operator;

#[cfg(test)]
mod operator_tests;
#[cfg(test)]
mod priority_tests;
#[cfg(test)]
mod proptests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod timing_tests;

/// Everything the director needs to know on one tick.
#[derive(Debug, Clone, Copy)]
pub struct DirectorInput<'a> {
    /// Current time on the media clock. Should not decrease.
    pub now: Duration,
    /// The shot on program, if any.
    pub program: Option<&'a ShotId>,
    /// The shot on preview, if any.
    pub preview: Option<&'a ShotId>,
    /// How long the program shot has been on air.
    pub program_on_air: Duration,
    /// True while the switcher runs a transition.
    pub in_transition: bool,
    /// Every shot that could go on air, with its current quality.
    pub candidates: &'a [ShotCandidate],
}

/// Timers and the incumbent suggestion: the only state the director owns.
#[derive(Debug, Clone, Default)]
struct State {
    /// `now` of the previous tick, to detect clock steps backwards.
    last_now: Option<Duration>,
    /// Program shot of the previous tick, to restart program timers.
    program: Option<ShotId>,
    /// The incumbent suggestion.
    suggestion: Option<ShotId>,
    /// Since when a challenger has led the suggestion by the margin.
    challenged_since: Option<Duration>,
    /// Since when the suggestion has beaten program by the margin.
    beaten_since: Option<Duration>,
    /// Since when program quality has been below the fallback threshold.
    low_since: Option<Duration>,
    /// Since when the operator's own switching has paused auto-takes
    /// ([`operator`]); kept only in Auto mode.
    paused_since: Option<Duration>,
    /// Shots the operator skipped and since when, each left out of the
    /// suggestions for `skip` ([`operator`]).
    skipped: Vec<(ShotId, Duration)>,
}

/// An eligible candidate and its score on this tick.
#[derive(Debug, Clone, Copy)]
struct Scored<'a> {
    /// The candidate.
    cand: &'a ShotCandidate,
    /// Its [`score`].
    score: f32,
}

/// Facts about this tick that the take decision needs.
#[derive(Debug, Clone, Copy)]
struct TickFacts<'a> {
    /// The program shot's candidate entry, if listed.
    program: Option<&'a ShotCandidate>,
    /// True when the fallback timer has expired.
    fallback: bool,
    /// True when the suggestion has beaten program for the hold time.
    beaten: bool,
}

/// The auto-director.
#[derive(Debug, Clone)]
pub struct Director {
    /// Validated parameters.
    config: DirectorConfig,
    /// Timers and incumbent suggestion.
    state: State,
}

impl Director {
    /// A director with `config`, which must pass [`DirectorConfig::validate`].
    pub fn new(config: DirectorConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            state: State::default(),
        })
    }

    /// The current parameters.
    pub fn config(&self) -> &DirectorConfig {
        &self.config
    }

    /// Replaces the parameters, keeping the current suggestion and timers.
    pub fn set_config(&mut self, config: DirectorConfig) -> Result<(), ConfigError> {
        config.validate()?;
        self.config = config;
        Ok(())
    }

    /// Changes the autonomy mode; a pause of auto-takes ends.
    pub fn set_mode(&mut self, mode: DirectorMode) {
        self.config.mode = mode;
        self.state.paused_since = None;
    }

    /// The incumbent suggestion from the last tick.
    pub fn suggestion(&self) -> Option<&ShotId> {
        self.state.suggestion.as_ref()
    }

    /// Forgets the suggestion and every timer.
    pub fn reset(&mut self) {
        self.state = State::default();
    }

    /// Decides this tick's suggestion and, in Auto mode, whether to take.
    pub fn tick(&mut self, input: &DirectorInput<'_>) -> DirectorDecision {
        if self.config.mode == DirectorMode::Off {
            self.reset();
            return DirectorDecision::off();
        }
        self.observe(input);
        self.expire_operator_windows(input.now);
        let candidates = first_occurrences(input.candidates);
        let program = input
            .program
            .and_then(|id| candidates.iter().copied().find(|c| &c.id == id));
        let fallback = self.update_fallback(input, program);
        let eligible: Vec<Scored<'_>> = candidates
            .iter()
            .copied()
            .filter(|c| self.is_eligible(c, input.program, program))
            .map(|cand| Scored {
                cand,
                score: score(cand, input.now, &self.config.scoring),
            })
            .collect();
        let urgent = fallback || input.program.is_none();
        let pool = if urgent {
            prefer_wides(eligible)
        } else {
            eligible
        };
        let challenger = self.choose(&pool, input.now);
        let Some(current) = self
            .state
            .suggestion
            .as_ref()
            .and_then(|id| find(&pool, id))
        else {
            self.state.beaten_since = None;
            return DirectorDecision {
                suggestion: None,
                take: None,
                reason: Reason::NoCandidate,
            };
        };
        let beaten = self.update_beaten(current.score, program, input.now);
        let suggestion = current.cand.id.clone();
        let base_reason = if input.program.is_none() {
            Reason::NothingOnAir
        } else if fallback {
            Reason::Fallback
        } else if let Some(challenger) = challenger {
            Reason::Holding { challenger }
        } else {
            Reason::Best
        };
        let (take, reason) = if self.config.mode == DirectorMode::Auto {
            let facts = TickFacts {
                program,
                fallback,
                beaten,
            };
            self.take_decision(input, &suggestion, facts, base_reason)
        } else {
            (None, base_reason)
        };
        DirectorDecision {
            suggestion: Some(suggestion),
            take,
            reason,
        }
    }

    /// Restarts timers on a clock step backwards or a program change.
    fn observe(&mut self, input: &DirectorInput<'_>) {
        let s = &mut self.state;
        if s.last_now.is_some_and(|last| input.now < last) {
            s.challenged_since = None;
            s.beaten_since = None;
            s.low_since = None;
            s.restart_operator_windows(input.now);
        }
        s.last_now = Some(input.now);
        if s.program.as_ref() != input.program {
            s.program = input.program.cloned();
            s.low_since = None;
            s.beaten_since = None;
        }
    }

    /// Runs the low-quality timer; true once program has been lost long enough.
    fn update_fallback(
        &mut self,
        input: &DirectorInput<'_>,
        program: Option<&ShotCandidate>,
    ) -> bool {
        if input.program.is_none() {
            self.state.low_since = None;
            return false;
        }
        let quality = program.filter(|p| p.available).map_or(0.0, |p| p.quality());
        if quality >= self.config.fallback_quality {
            self.state.low_since = None;
            return false;
        }
        let since = *self.state.low_since.get_or_insert(input.now);
        input.now.saturating_sub(since) >= self.config.fallback_after
    }

    /// True when `c` may be suggested (step 3 of the module docs).
    fn is_eligible(
        &self,
        c: &ShotCandidate,
        program_id: Option<&ShotId>,
        program: Option<&ShotCandidate>,
    ) -> bool {
        c.available
            && c.has_valid_crop()
            && c.quality() >= self.config.fallback_quality
            && program_id != Some(&c.id)
            && !program.is_some_and(|p| is_jump_cut(p, c, self.config.jump_cut_iou))
            && !self.is_skipped(&c.id)
    }

    /// Applies hysteresis over `pool`, updating the incumbent suggestion.
    ///
    /// Returns the challenger while one leads but has not held the lead long
    /// enough.
    fn choose(&mut self, pool: &[Scored<'_>], now: Duration) -> Option<ShotId> {
        let Some(best) = best_of(pool) else {
            self.state.suggestion = None;
            self.state.challenged_since = None;
            return None;
        };
        let incumbent = self.state.suggestion.as_ref().and_then(|id| find(pool, id));
        let Some(incumbent) = incumbent else {
            self.state.suggestion = Some(best.cand.id.clone());
            self.state.challenged_since = None;
            return None;
        };
        if best.cand.id == incumbent.cand.id || best.score - incumbent.score < self.config.margin {
            self.state.challenged_since = None;
            return None;
        }
        let since = *self.state.challenged_since.get_or_insert(now);
        if now.saturating_sub(since) >= self.config.hold {
            self.state.suggestion = Some(best.cand.id.clone());
            self.state.challenged_since = None;
            None
        } else {
            Some(best.cand.id.clone())
        }
    }

    /// Runs the "suggestion beats program" timer; true once it has held.
    fn update_beaten(
        &mut self,
        suggestion_score: f32,
        program: Option<&ShotCandidate>,
        now: Duration,
    ) -> bool {
        let program_score = program
            .filter(|p| p.available)
            .map_or(0.0, |p| base_score(p, &self.config.scoring));
        if suggestion_score - program_score < self.config.margin {
            self.state.beaten_since = None;
            return false;
        }
        let since = *self.state.beaten_since.get_or_insert(now);
        now.saturating_sub(since) >= self.config.hold
    }

    /// Step 6 of the module docs: the Auto-mode take rules.
    fn take_decision(
        &self,
        input: &DirectorInput<'_>,
        suggestion: &ShotId,
        facts: TickFacts<'_>,
        base_reason: Reason,
    ) -> (Option<TakeRequest>, Reason) {
        if let Some(remaining) = self.paused(input.now) {
            return (None, Reason::Paused { remaining });
        }
        if input.in_transition {
            return (None, Reason::TransitionInProgress);
        }
        let take = |transition| {
            Some(TakeRequest {
                shot: suggestion.clone(),
                transition,
            })
        };
        if input.program.is_none() {
            return (take(self.config.transition), Reason::NothingOnAir);
        }
        if facts.fallback {
            return (take(TransitionHint::Cut), Reason::Fallback);
        }
        let dwell = &self.config.dwell;
        let on_air = input.program_on_air;
        if on_air < dwell.min {
            let remaining = dwell.min - on_air;
            return (None, Reason::MinDwell { remaining });
        }
        let max = match facts.program.map(|p| p.kind) {
            Some(CandidateKind::Wide) => dwell.max_wide,
            _ => dwell.max_shot,
        };
        let due = if on_air >= max {
            Reason::MaxDwell
        } else if facts.beaten {
            Reason::Better
        } else {
            return (None, base_reason);
        };
        if input.preview == Some(suggestion) {
            (take(self.config.transition), due)
        } else {
            (None, Reason::AwaitingPreview)
        }
    }
}

/// The candidates in input order, keeping only the first entry of each id.
fn first_occurrences(candidates: &[ShotCandidate]) -> Vec<&ShotCandidate> {
    let mut seen = HashSet::new();
    candidates.iter().filter(|c| seen.insert(&c.id)).collect()
}

/// The eligible wides, or every eligible shot when no wide is eligible.
fn prefer_wides(eligible: Vec<Scored<'_>>) -> Vec<Scored<'_>> {
    let wides: Vec<Scored<'_>> = eligible
        .iter()
        .copied()
        .filter(|s| s.cand.kind == CandidateKind::Wide)
        .collect();
    if wides.is_empty() { eligible } else { wides }
}

/// The highest score; the earliest entry wins ties.
fn best_of<'p, 'a>(pool: &'p [Scored<'a>]) -> Option<&'p Scored<'a>> {
    pool.iter().fold(None, |best, s| match best {
        Some(b) if b.score >= s.score => Some(b),
        _ => Some(s),
    })
}

/// The pool entry with `id`.
fn find<'p, 'a>(pool: &'p [Scored<'a>], id: &ShotId) -> Option<&'p Scored<'a>> {
    pool.iter().find(|s| &s.cand.id == id)
}
