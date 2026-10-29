//! The `[switcher]` table: the transition the switcher starts with,
//! whether a wide is on program from the start, and, in its
//! `[switcher.settle]` subtable, settle-before-take (whether an operator's
//! take waits for the preview crop to settle; off by default;
//! [`SettleSettings`] has the table and [`crate::settle`] the rules).
//!
//! The transition's defaults are those of [`Transition::default`]. A cut
//! ignores its duration; a mix or a dip must last more than 0 and at most
//! 10 seconds. [`SwitcherSettings::problems`] lists every bad value, the
//! subtable's included, and [`SwitcherSettings::validate`] fails on the
//! first.
//!
//! `start_on_wide` (off by default) puts the first source's wide on
//! program when the engine starts, before anything else is taken, instead
//! of a black program that waits for the operator or the director (spec
//! C.3: a crash recovers to a clean wide shot). The core does it when it is
//! built ([`crate::EngineCore::new`]), so the snapshot before the first
//! tick already shows it. The wide goes on air even while its source has
//! not delivered yet: it shows black until the first frame arrives, and
//! the picture without anyone taking a shot once it does.

mod settle;

use ac_switcher::{Transition, TransitionKind};
use serde::{Deserialize, Serialize};
pub use settle::SettleSettings;

/// The switcher's initial transition and program, and settle-before-take.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwitcherSettings {
    /// Settle-before-take, the `[switcher.settle]` subtable: whether, and
    /// how long, an operator's take waits for preview to settle (off by
    /// default).
    pub settle: SettleSettings,
    /// Cut, mix (default) or dip.
    pub transition: TransitionKind,
    /// Duration in seconds for mix and dip.
    pub duration: f64,
    /// Put the first source's wide on program at start (see the module
    /// docs); off by default.
    pub start_on_wide: bool,
}

impl Default for SwitcherSettings {
    fn default() -> Self {
        let t = Transition::default();
        Self {
            settle: SettleSettings::default(),
            transition: t.kind,
            duration: t.duration,
            start_on_wide: false,
        }
    }
}

impl SwitcherSettings {
    /// Checks the duration, returning the first problem of
    /// [`Self::problems`].
    pub fn validate(&self) -> Result<(), String> {
        match self.problems().into_iter().next() {
            Some(problem) => Err(problem),
            None => Ok(()),
        }
    }

    /// Every problem in the table: the duration, then the settle-before-take
    /// values ([`SettleSettings::problems`]).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.transition != TransitionKind::Cut
            && !(self.duration.is_finite() && self.duration > 0.0 && self.duration <= 10.0)
        {
            out.push(format!(
                "switcher.duration {} must be in (0, 10] seconds",
                self.duration
            ));
        }
        out.extend(self.settle.problems());
        out
    }

    /// The transition.
    pub fn to_transition(&self) -> Transition {
        Transition {
            kind: self.transition,
            duration: self.duration,
        }
    }
}

#[cfg(test)]
mod tests;
