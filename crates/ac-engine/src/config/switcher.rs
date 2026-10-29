//! The `[switcher]` table: the transition the switcher starts with, and
//! whether a wide is on program from the start.
//!
//! The defaults are those of [`Transition::default`]. A cut ignores its
//! duration; a mix or a dip must last more than 0 and at most 10 seconds
//! ([`SwitcherSettings::validate`]).
//!
//! `start_on_wide` (off by default) puts the first source's wide on
//! program when the engine starts, before anything else is taken, instead
//! of a black program that waits for the operator or the director (spec
//! C.3: a crash recovers to a clean wide shot). The core does it when it is
//! built ([`crate::EngineCore::new`]), so the snapshot before the first
//! tick already shows it. The wide goes on air even while its source has
//! not delivered yet: it shows black until the first frame arrives, and
//! the picture without anyone taking a shot once it does.

use ac_switcher::{Transition, TransitionKind};
use serde::{Deserialize, Serialize};

/// The switcher's initial transition and program.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwitcherSettings {
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
            transition: t.kind,
            duration: t.duration,
            start_on_wide: false,
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

#[cfg(test)]
mod tests;
