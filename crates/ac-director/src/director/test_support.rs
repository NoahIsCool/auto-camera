//! Shared fixtures for the director's scenario tests.
//!
//! Scenario tests drive the director tick by tick at 10 Hz with hand-built
//! inputs. [`Sim`] plays the engine for longer runs: it stages every
//! suggestion on preview, applies takes as instant cuts and records when each
//! shot left program, which is what the engine reports as `recently_used_at`.

use std::collections::HashMap;
use std::time::Duration;

use ac_core::{Rect, ShotId, SourceId};

use super::{Director, DirectorInput};
use crate::{
    CandidateKind, ContentProfile, DirectorConfig, DirectorDecision, DirectorMode, Reason,
    ShotCandidate,
};

/// Shorthand for a shot id.
pub(super) fn id(s: &str) -> ShotId {
    ShotId(s.into())
}

/// Tick `k` of a 10 Hz clock.
pub(super) fn tick_time(k: u64) -> Duration {
    Duration::from_millis(k * 100)
}

/// A wide of `source` at full quality.
pub(super) fn wide(name: &str, source: &str) -> ShotCandidate {
    ShotCandidate {
        id: id(name),
        source: SourceId(source.into()),
        kind: CandidateKind::Wide,
        crop: Rect::FULL,
        quality: 1.0,
        available: true,
        recently_used_at: None,
    }
}

/// A subject shot of `source` with a centred medium crop.
pub(super) fn subject(name: &str, source: &str, quality: f32) -> ShotCandidate {
    ShotCandidate {
        id: id(name),
        source: SourceId(source.into()),
        kind: CandidateKind::Subject,
        crop: Rect::new(0.3, 0.2, 0.4, 0.4),
        quality,
        available: true,
        recently_used_at: None,
    }
}

/// A director with the specification defaults.
pub(super) fn director(mode: DirectorMode, profile: ContentProfile) -> Director {
    Director::new(DirectorConfig::new(mode, profile)).unwrap()
}

/// An input with no transition running.
pub(super) fn input<'a>(
    now: Duration,
    program: Option<&'a ShotId>,
    preview: Option<&'a ShotId>,
    program_on_air: Duration,
    candidates: &'a [ShotCandidate],
) -> DirectorInput<'a> {
    DirectorInput {
        now,
        program,
        preview,
        program_on_air,
        in_transition: false,
        candidates,
    }
}

/// A minimal engine: stages suggestions, applies takes as instant cuts.
pub(super) struct Sim {
    /// The director under test.
    pub(super) director: Director,
    /// Shot on program.
    pub(super) program: Option<ShotId>,
    /// Shot on preview.
    pub(super) preview: Option<ShotId>,
    /// When program went on air.
    pub(super) on_air_since: Duration,
    /// When each shot last left program.
    pub(super) used: HashMap<ShotId, Duration>,
    /// Every take: time, shot and reason.
    pub(super) takes: Vec<(Duration, ShotId, Reason)>,
    /// Next tick index.
    pub(super) next: u64,
}

impl Sim {
    /// A simulation starting at time zero with `program` on air.
    pub(super) fn new(director: Director, program: Option<&str>) -> Self {
        Self {
            director,
            program: program.map(id),
            preview: None,
            on_air_since: Duration::ZERO,
            used: HashMap::new(),
            takes: Vec::new(),
            next: 0,
        }
    }

    /// Runs ticks up to and including `until`, with candidates from `cands`.
    pub(super) fn run(
        &mut self,
        until: Duration,
        mut cands: impl FnMut(Duration) -> Vec<ShotCandidate>,
    ) -> Vec<(Duration, DirectorDecision)> {
        let mut out = Vec::new();
        while tick_time(self.next) <= until {
            let now = tick_time(self.next);
            self.next += 1;
            let mut list = cands(now);
            for c in &mut list {
                c.recently_used_at = self.used.get(&c.id).copied();
            }
            let decision = self.director.tick(&input(
                now,
                self.program.as_ref(),
                self.preview.as_ref(),
                now - self.on_air_since,
                &list,
            ));
            if let Some(take) = &decision.take {
                let old = self.program.replace(take.shot.clone());
                if let Some(old) = &old {
                    self.used.insert(old.clone(), now);
                }
                self.preview = old;
                self.on_air_since = now;
                self.takes
                    .push((now, take.shot.clone(), decision.reason.clone()));
            } else if let Some(s) = &decision.suggestion {
                self.preview = Some(s.clone());
            }
            out.push((now, decision));
        }
        out
    }

    /// The subject shots taken so far, in order, leaving out wides.
    pub(super) fn subject_takes(&self) -> Vec<ShotId> {
        self.takes
            .iter()
            .map(|(_, s, _)| s.clone())
            .filter(|s| !s.0.starts_with("wide"))
            .collect()
    }
}
