//! The `[switcher]` table: the transition the switcher starts with.
//!
//! The defaults are those of [`Transition::default`]. A cut ignores its
//! duration; a mix or a dip must last more than 0 and at most 10 seconds
//! ([`SwitcherSettings::validate`]).

use ac_switcher::{Transition, TransitionKind};
use serde::{Deserialize, Serialize};

/// The switcher's initial transition.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwitcherSettings {
    /// Cut, mix (default) or dip.
    pub transition: TransitionKind,
    /// Duration in seconds for mix and dip.
    pub duration: f64,
}

impl Default for SwitcherSettings {
    fn default() -> Self {
        let t = Transition::default();
        Self {
            transition: t.kind,
            duration: t.duration,
        }
    }
}

impl SwitcherSettings {
    /// Checks the duration.
    pub fn validate(&self) -> Result<(), String> {
        if self.transition != TransitionKind::Cut
            && !(self.duration.is_finite() && self.duration > 0.0 && self.duration <= 10.0)
        {
            return Err(format!(
                "switcher.duration {} must be in (0, 10] seconds",
                self.duration
            ));
        }
        Ok(())
    }

    /// The transition.
    pub fn to_transition(&self) -> Transition {
        Transition {
            kind: self.transition,
            duration: self.duration,
        }
    }
}
