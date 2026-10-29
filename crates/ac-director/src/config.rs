//! Director configuration: autonomy mode, content profile and the timing,
//! margin and threshold parameters of the decision rules.
//!
//! Defaults come from the rebuild specification and the professional rules it
//! cites (Rui et al.; GAZED):
//!
//! | Parameter | Talk | Music | Why |
//! |---|---|---|---|
//! | Minimum dwell | 4 s | 2.5 s | Shorter shots read as nervous; music cuts faster |
//! | Maximum wide dwell | 10 s | 10 s | Overview shots up to 7–10 s |
//! | Maximum shot dwell | 20 s | 8 s | Keep the programme moving |
//! | Transition | mix | cut | Dissolve for sermon and prayer; cut for music |
//!
//! Shared defaults: jump-cut IoU 0.4, challenger margin δ = 0.1 held for
//! 0.75 s, fallback when program quality is below 0.3 for 0.5 s, and the
//! [`ScoreWeights`] defaults (wide weight 0.75, 0.05 per priority level, a
//! variety penalty of 0.3 decaying over 60 s; see [`crate::candidate`]).
//!
//! Invariant: a [`DirectorConfig`] accepted by [`DirectorConfig::validate`]
//! has finite, non-negative weights, thresholds inside `0.0..=1.0`, and a
//! minimum dwell no longer than either maximum dwell. The director relies on
//! this to keep every score finite.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::candidate::ScoreWeights;
use crate::decision::TransitionHint;

/// How much the director may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectorMode {
    /// Outputs nothing; the operator switches alone.
    Off,
    /// Proposes the next shot for preview; the operator takes it.
    #[default]
    Suggest,
    /// Proposes and also takes, for unattended use (opt-in).
    Auto,
}

/// What kind of content is on air; selects the timing defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentProfile {
    /// Speech: sermon, talk, prayer. Longer shots, dissolves.
    #[default]
    Talk,
    /// Music: band and worship sets. Shorter shots, cuts.
    Music,
}

impl ContentProfile {
    /// The transition used for ordinary takes with this content.
    pub fn transition(self) -> TransitionHint {
        match self {
            ContentProfile::Talk => TransitionHint::Mix,
            ContentProfile::Music => TransitionHint::Cut,
        }
    }
}

/// How long a shot may stay on program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dwell {
    /// No ordinary take before program has been on air this long.
    pub min: Duration,
    /// A wide on program is replaced once on air this long.
    pub max_wide: Duration,
    /// A subject shot on program is replaced once on air this long.
    pub max_shot: Duration,
}

impl Dwell {
    /// The specification's dwell limits for `profile`.
    pub fn for_profile(profile: ContentProfile) -> Self {
        match profile {
            ContentProfile::Talk => Self {
                min: Duration::from_secs(4),
                max_wide: Duration::from_secs(10),
                max_shot: Duration::from_secs(20),
            },
            ContentProfile::Music => Self {
                min: Duration::from_millis(2500),
                max_wide: Duration::from_secs(10),
                max_shot: Duration::from_secs(8),
            },
        }
    }
}

/// All director parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectorConfig {
    /// Autonomy level.
    pub mode: DirectorMode,
    /// Content profile the timing defaults were taken from.
    pub profile: ContentProfile,
    /// Minimum and maximum dwell times.
    pub dwell: Dwell,
    /// Transition hint for ordinary takes. Fallback takes always cut.
    pub transition: TransitionHint,
    /// Largest crop IoU allowed between two shots of the same source.
    pub jump_cut_iou: f64,
    /// Margin δ by which a challenger must lead before it counts.
    pub margin: f32,
    /// How long the lead must hold before the suggestion changes, and before
    /// Auto takes a suggestion that beats program.
    pub hold: Duration,
    /// Weights of the scoring function.
    pub scoring: ScoreWeights,
    /// Program quality below this counts as lost; candidates below it are
    /// never suggested.
    pub fallback_quality: f32,
    /// How long program quality must stay low before falling back to a wide.
    pub fallback_after: Duration,
}

impl DirectorConfig {
    /// The specification's defaults for `profile`, in `mode`.
    pub fn new(mode: DirectorMode, profile: ContentProfile) -> Self {
        Self {
            mode,
            profile,
            dwell: Dwell::for_profile(profile),
            transition: profile.transition(),
            jump_cut_iou: 0.4,
            margin: 0.1,
            hold: Duration::from_millis(750),
            scoring: ScoreWeights::default(),
            fallback_quality: 0.3,
            fallback_after: Duration::from_millis(500),
        }
    }

    /// Checks every parameter; see [`ConfigError`] for what is rejected.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let d = &self.dwell;
        for max in [d.max_wide, d.max_shot] {
            if d.min > max {
                return Err(ConfigError::DwellOrder { min: d.min, max });
            }
        }
        unit("jump_cut_iou", self.jump_cut_iou)?;
        unit("fallback_quality", f64::from(self.fallback_quality))?;
        non_negative("margin", f64::from(self.margin))?;
        non_negative("wide_weight", f64::from(self.scoring.wide_weight))?;
        non_negative("priority_weight", f64::from(self.scoring.priority_weight))?;
        non_negative("variety_penalty", f64::from(self.scoring.variety_penalty))?;
        Ok(())
    }
}

impl Default for DirectorConfig {
    fn default() -> Self {
        Self::new(DirectorMode::default(), ContentProfile::default())
    }
}

/// Rejects a value outside `0.0..=1.0` (or NaN).
fn unit(name: &'static str, value: f64) -> Result<(), ConfigError> {
    if (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(ConfigError::OutOfRange {
            name,
            value,
            range: "0..=1",
        })
    }
}

/// Rejects a negative, infinite or NaN value.
fn non_negative(name: &'static str, value: f64) -> Result<(), ConfigError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(ConfigError::OutOfRange {
            name,
            value,
            range: "finite and >= 0",
        })
    }
}

/// Why a configuration was rejected.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ConfigError {
    /// The minimum dwell is longer than a maximum dwell.
    #[error("minimum dwell {min:?} exceeds maximum dwell {max:?}")]
    DwellOrder {
        /// Minimum dwell.
        min: Duration,
        /// The shorter maximum dwell.
        max: Duration,
    },
    /// A numeric parameter is outside its allowed range.
    #[error("{name} = {value} is outside {range}")]
    OutOfRange {
        /// Parameter name.
        name: &'static str,
        /// Rejected value.
        value: f64,
        /// Allowed range, for the message.
        range: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_carry_specified_defaults() {
        let talk = DirectorConfig::new(DirectorMode::Suggest, ContentProfile::Talk);
        assert_eq!(talk.dwell.min, Duration::from_secs(4));
        assert_eq!(talk.dwell.max_wide, Duration::from_secs(10));
        assert_eq!(talk.dwell.max_shot, Duration::from_secs(20));
        assert_eq!(talk.transition, TransitionHint::Mix);
        let music = DirectorConfig::new(DirectorMode::Auto, ContentProfile::Music);
        assert_eq!(music.dwell.min, Duration::from_millis(2500));
        assert_eq!(music.dwell.max_shot, Duration::from_secs(8));
        assert_eq!(music.transition, TransitionHint::Cut);
        assert_eq!(music.hold, Duration::from_millis(750));
        assert!((music.margin - 0.1).abs() < 1e-6);
        assert!(talk.validate().is_ok() && music.validate().is_ok());
    }

    #[test]
    fn default_is_suggest_mode_for_talk() {
        let c = DirectorConfig::default();
        assert_eq!(c.mode, DirectorMode::Suggest);
        assert_eq!(c.profile, ContentProfile::Talk);
    }

    #[test]
    fn min_dwell_longer_than_max_is_rejected() {
        let mut c = DirectorConfig::default();
        c.dwell.min = Duration::from_secs(30);
        assert_eq!(
            c.validate(),
            Err(ConfigError::DwellOrder {
                min: Duration::from_secs(30),
                max: Duration::from_secs(10),
            })
        );
    }

    #[test]
    fn out_of_range_numbers_are_rejected() {
        let base = DirectorConfig::default;
        let iou = DirectorConfig {
            jump_cut_iou: 1.5,
            ..base()
        };
        assert!(matches!(
            iou.validate(),
            Err(ConfigError::OutOfRange {
                name: "jump_cut_iou",
                ..
            })
        ));
        let margin = DirectorConfig {
            margin: f32::NAN,
            ..base()
        };
        assert!(margin.validate().is_err());
        let quality = DirectorConfig {
            fallback_quality: -0.1,
            ..base()
        };
        assert!(quality.validate().is_err());
        let scoring = ScoreWeights {
            variety_penalty: f32::INFINITY,
            ..ScoreWeights::default()
        };
        assert!(DirectorConfig { scoring, ..base() }.validate().is_err());
    }

    #[test]
    fn edge_values_are_accepted() {
        let dwell = Dwell::for_profile(ContentProfile::Talk);
        let c = DirectorConfig {
            margin: 0.0,
            hold: Duration::ZERO,
            jump_cut_iou: 1.0,
            fallback_quality: 0.0,
            dwell: Dwell {
                min: dwell.max_wide,
                ..dwell
            },
            ..DirectorConfig::default()
        };
        assert!(c.validate().is_ok());
    }

    #[test]
    fn priority_weight_must_be_finite_and_non_negative() {
        let with = |priority_weight| DirectorConfig {
            scoring: ScoreWeights {
                priority_weight,
                ..ScoreWeights::default()
            },
            ..DirectorConfig::default()
        };
        assert!((ScoreWeights::default().priority_weight - 0.05).abs() < 1e-6);
        for bad in [-0.05, f32::NAN, f32::INFINITY] {
            assert!(matches!(
                with(bad).validate(),
                Err(ConfigError::OutOfRange {
                    name: "priority_weight",
                    ..
                })
            ));
        }
        // Zero turns priorities off; a large weight is the operator's call.
        assert!(with(0.0).validate().is_ok());
        assert!(with(2.0).validate().is_ok());
    }
}
