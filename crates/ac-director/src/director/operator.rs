//! The operator's side of the director: skipped suggestions and what the
//! operator's own switching does to a director in Auto mode.
//!
//! The operator stays the authority (spec: "manual override at any
//! moment"; V5: "the operator accepts or skips"). Two inputs come from
//! them, and both are timed on the media clock like everything else here:
//!
//! * **Skip.** [`Director::skip`] leaves a shot out of the suggestions for
//!   [`DirectorConfig::skip`](crate::DirectorConfig::skip) from the moment it
//!   is skipped. The shot is simply not eligible (step 3 of the
//!   [procedure](super)), so a skipped incumbent is replaced by the best
//!   remaining shot at once, and nothing about scoring changes. Skipping
//!   the shot again restarts its window; once the window has passed, the
//!   shot competes again under the usual hysteresis.
//! * **Takeover.** [`Director::take_over`] is called by the engine for each
//!   manual switching action. Outside Auto mode it does nothing: the
//!   director takes nothing there anyway. In Auto mode the
//!   [`Takeover`] setting decides: drop to Suggest (the default, so the
//!   operator's key press is never fought), pause the director's takes for
//!   [`DirectorConfig::pause`](crate::DirectorConfig::pause) from the action
//!   (each further action restarts the pause), or let the director carry
//!   on. While paused the director still suggests; it only takes nothing,
//!   fallback included, since the operator is switching by hand
//!   ([`Reason::Paused`](crate::Reason::Paused)).
//!
//! Invariants: a skipped shot is never suggested within its window; a
//! pause exists only in Auto mode and ends after `pause` without further
//! actions, or on any mode change; a clock step backwards restarts both
//! windows from the new `now`, as it restarts every other timer.

use std::time::Duration;

use ac_core::ShotId;

use super::{Director, State};
use crate::config::{DirectorMode, Takeover};
use crate::decision::Handover;

impl Director {
    /// Leaves `shot` out of the suggestions for the skip window from `now`
    /// (see the module docs). Skipping a skipped shot restarts its window.
    pub fn skip(&mut self, shot: ShotId, now: Duration) {
        let skipped = &mut self.state.skipped;
        skipped.retain(|(s, _)| s != &shot);
        skipped.push((shot, now));
    }

    /// The shots skipped and still left out, oldest skip first, each with
    /// the time left of its window at `now`.
    pub fn skipped(&self, now: Duration) -> Vec<(ShotId, Duration)> {
        self.state
            .skipped
            .iter()
            .filter_map(|(shot, since)| {
                let left = self.config.skip.saturating_sub(now.saturating_sub(*since));
                (!left.is_zero()).then(|| (shot.clone(), left))
            })
            .collect()
    }

    /// The operator switched by hand at `now`: in Auto mode, applies the
    /// [`Takeover`] setting and says what it did; `None` when nothing
    /// changed (another mode, or the director wins). See the module docs.
    pub fn take_over(&mut self, now: Duration) -> Option<Handover> {
        if self.config.mode != DirectorMode::Auto {
            return None;
        }
        match self.config.takeover {
            Takeover::Suggest => {
                self.set_mode(DirectorMode::Suggest);
                Some(Handover::ToSuggest)
            }
            Takeover::Pause => {
                self.state.paused_since = Some(now);
                Some(Handover::Paused(self.config.pause))
            }
            Takeover::Director => None,
        }
    }

    /// How much longer the operator's switching holds auto-takes at `now`;
    /// `None` when they are not paused (always outside Auto mode).
    pub fn paused(&self, now: Duration) -> Option<Duration> {
        if self.config.mode != DirectorMode::Auto {
            return None;
        }
        let since = self.state.paused_since?;
        let left = self.config.pause.saturating_sub(now.saturating_sub(since));
        (!left.is_zero()).then_some(left)
    }

    /// True when `shot` is skipped; the windows are expired first, on each
    /// tick ([`Self::expire_operator_windows`]).
    pub(super) fn is_skipped(&self, shot: &ShotId) -> bool {
        self.state.skipped.iter().any(|(s, _)| s == shot)
    }

    /// Forgets the skips and the pause whose windows have passed at `now`.
    pub(super) fn expire_operator_windows(&mut self, now: Duration) {
        let skip = self.config.skip;
        self.state
            .skipped
            .retain(|(_, since)| now.saturating_sub(*since) < skip);
        if self.paused(now).is_none() {
            self.state.paused_since = None;
        }
    }
}

impl State {
    /// Restarts every skip window and the pause from `now`, after a clock
    /// step backwards.
    pub(super) fn restart_operator_windows(&mut self, now: Duration) {
        if let Some(since) = &mut self.paused_since {
            *since = now;
        }
        for (_, since) in &mut self.skipped {
            *since = now;
        }
    }
}
