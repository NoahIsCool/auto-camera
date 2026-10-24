//! What the director tells the engine and the operator each tick.
//!
//! A [`DirectorDecision`] has three parts:
//!
//! * `suggestion`: the shot the engine should stage on preview, or `None`.
//! * `take`: in Auto mode only, a request to put a shot on program now.
//! * `reason`: one [`Reason`] for the operator UI, with a [`Display`] text.
//!
//! Invariants upheld by the director: a `take` names the same shot as
//! `suggestion`; there is never a `take` outside Auto mode or during a
//! transition; in Off mode both are `None` and the reason is [`Reason::Off`];
//! and `suggestion` is `None` only with [`Reason::Off`] or
//! [`Reason::NoCandidate`].
//!
//! [`Display`]: std::fmt::Display

use std::fmt;
use std::time::Duration;

use ac_core::ShotId;
use serde::{Deserialize, Serialize};

/// How the engine should transition for a take.
///
/// This is a hint: the engine maps it onto its switcher's transitions
/// (`Cut` to a cut, `Mix` to the selected dissolve).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionHint {
    /// Instant change.
    Cut,
    /// Cross-dissolve.
    Mix,
}

/// A request to put a shot on program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeRequest {
    /// The shot to take. The engine stages it on preview first if it is not
    /// there already (fallback and establishing takes do not wait for it).
    pub shot: ShotId,
    /// Suggested transition.
    pub transition: TransitionHint,
}

/// Why the director decided what it did, for the operator UI.
///
/// Only one reason is reported per tick. In Auto mode a take or a reason
/// for withholding one takes precedence over the suggestion's own reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The director is off.
    Off,
    /// No shot can be suggested: everything is unavailable, too weak, on
    /// program, or a jump cut from program.
    NoCandidate,
    /// Nothing is on program; the suggestion establishes the scene (a wide
    /// when one is eligible). Auto takes it at once.
    NothingOnAir,
    /// Program quality stayed below the fallback threshold; the suggestion is
    /// a wide when one is eligible. Auto takes it at once with a cut.
    Fallback,
    /// The suggestion is the best-scoring eligible shot; no take is due.
    Best,
    /// The suggestion is kept by hysteresis: `challenger` leads it by the
    /// margin but has not held the lead for the hold time yet.
    Holding {
        /// The shot that may replace the suggestion.
        challenger: ShotId,
    },
    /// Auto: program has not reached its minimum dwell.
    MinDwell {
        /// Time until the minimum dwell is reached.
        remaining: Duration,
    },
    /// Auto: a take is due but the suggestion is not on preview yet.
    AwaitingPreview,
    /// Auto: a transition is running, so no take is requested.
    TransitionInProgress,
    /// Auto take: the suggestion beat program by the margin for the hold time.
    Better,
    /// Auto take: program reached its maximum dwell.
    MaxDwell,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::Off => f.write_str("auto-director off"),
            Reason::NoCandidate => f.write_str("no shot available to suggest"),
            Reason::NothingOnAir => f.write_str("nothing on program: establishing shot"),
            Reason::Fallback => f.write_str("program shot lost: falling back to wide"),
            Reason::Best => f.write_str("best available shot"),
            Reason::Holding { challenger } => {
                write!(
                    f,
                    "holding suggestion while {} keeps its lead",
                    challenger.0
                )
            }
            Reason::MinDwell { remaining } => write!(
                f,
                "holding program for minimum dwell ({:.1} s left)",
                remaining.as_secs_f64()
            ),
            Reason::AwaitingPreview => f.write_str("take due: waiting for suggestion on preview"),
            Reason::TransitionInProgress => f.write_str("transition in progress"),
            Reason::Better => f.write_str("taking: suggestion beats program"),
            Reason::MaxDwell => f.write_str("taking: program reached maximum dwell"),
        }
    }
}

/// The director's output for one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectorDecision {
    /// Shot to stage on preview.
    pub suggestion: Option<ShotId>,
    /// Shot to put on program now (Auto mode only).
    pub take: Option<TakeRequest>,
    /// Explanation for the operator.
    pub reason: Reason,
}

impl DirectorDecision {
    /// The decision of a director that is off: nothing at all.
    pub fn off() -> Self {
        Self {
            suggestion: None,
            take: None,
            reason: Reason::Off,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_decision_is_empty() {
        let d = DirectorDecision::off();
        assert!(d.suggestion.is_none() && d.take.is_none());
        assert_eq!(d.reason, Reason::Off);
    }

    #[test]
    fn reasons_render_operator_text() {
        let holding = Reason::Holding {
            challenger: ShotId("drums".into()),
        };
        assert!(holding.to_string().contains("drums"));
        let dwell = Reason::MinDwell {
            remaining: Duration::from_millis(1500),
        };
        assert!(dwell.to_string().contains("1.5 s"));
        for r in [
            Reason::Off,
            Reason::NoCandidate,
            Reason::NothingOnAir,
            Reason::Fallback,
            Reason::Best,
            Reason::AwaitingPreview,
            Reason::TransitionInProgress,
            Reason::Better,
            Reason::MaxDwell,
        ] {
            assert!(!r.to_string().is_empty());
        }
    }
}
