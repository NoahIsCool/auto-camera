//! The `[switcher]` table: the transition the switcher starts with.
//!
//! The defaults are those of [`Transition::default`]. A cut ignores its
//! duration; a mix or a dip must last more than 0 and at most 10 seconds
//! ([`SwitcherSettings::validate`]).

mod settle;

use ac_switcher::{Transition, TransitionKind};
use serde::{Deserialize, Serialize};
pub use settle::SettleSettings;

/// The switcher's initial transition.
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
}

impl Default for SwitcherSettings {
    fn default() -> Self {
        let t = Transition::default();
        Self {
            settle: SettleSettings::default(),
            transition: t.kind,
            duration: t.duration,
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
