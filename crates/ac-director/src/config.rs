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
//! [`ScoreWeights`] defaults.
//!
//! The operator's side has its own parameters, the same for both profiles:
//! a shot the operator skips is left out of the suggestions for 30 s, and
//! what the operator's own switching does while the director runs in Auto
//! mode is the [`Takeover`] setting: by default the director drops to
//! Suggest, so the operator's key press is never fought; the pause setting
//! holds the director's takes for 10 s after each manual action instead.
//!
//! Invariant: a [`DirectorConfig`] accepted by [`DirectorConfig::validate`]
//! has finite, non-negative weights, thresholds inside `0.0..=1.0`, a
//! minimum dwell no longer than either maximum dwell, and a skip window and
//! a pause longer than zero. The director relies on this to keep every
//! score finite and every skip and pause able to end.

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

/// What the operator's own switching (staging, Cut, Cut now, Auto, the
/// T-bar) does while the director runs in [`DirectorMode::Auto`]; see
/// [`Director::take_over`](crate::Director::take_over). In the other modes
/// the operator switches anyway and the director takes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Takeover {
    /// The director drops to Suggest: it goes on proposing shots and the
    /// operator takes them.
    #[default]
    Suggest,
    /// The director stays in Auto but takes nothing for
    /// [`DirectorConfig::pause`] after each manual action; meanwhile it
    /// proposes as in Suggest mode.
    Pause,
    /// Nothing changes: the director goes on staging and taking, and stages
    /// its own suggestion over a shot the operator staged.
    Director,
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
    /// What the operator's own switching does in Auto mode.
    pub takeover: Takeover,
    /// How long each manual action holds auto-takes with
    /// [`Takeover::Pause`].
    pub pause: Duration,
    /// How long a shot the operator skipped is left out of the suggestions.
    pub skip: Duration,
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
            takeover: Takeover::default(),
            pause: Duration::from_secs(10),
            skip: Duration::from_secs(30),
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
        non_negative("variety_penalty", f64::from(self.scoring.variety_penalty))?;
        positive("pause", self.pause)?;
        positive("skip", self.skip)
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

/// Rejects a zero duration.
fn positive(name: &'static str, value: Duration) -> Result<(), ConfigError> {
    if value.is_zero() {
        Err(ConfigError::OutOfRange {
            name,
            value: 0.0,
            range: "> 0 s",
        })
    } else {
        Ok(())
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
    fn the_operator_side_defaults_to_dropping_to_suggest() {
        for profile in [ContentProfile::Talk, ContentProfile::Music] {
            let c = DirectorConfig::new(DirectorMode::Auto, profile);
            assert_eq!(c.takeover, Takeover::Suggest);
            assert_eq!(c.pause, Duration::from_secs(10));
            assert_eq!(c.skip, Duration::from_secs(30));
        }
    }

    #[test]
    fn a_zero_pause_or_skip_window_is_rejected() {
        let pause = DirectorConfig {
            pause: Duration::ZERO,
            ..DirectorConfig::default()
        };
        assert!(matches!(
            pause.validate(),
            Err(ConfigError::OutOfRange { name: "pause", .. })
        ));
        let skip = DirectorConfig {
            skip: Duration::ZERO,
            ..DirectorConfig::default()
        };
        assert!(matches!(
            skip.validate(),
            Err(ConfigError::OutOfRange { name: "skip", .. })
        ));
        let tiny = DirectorConfig {
            pause: Duration::from_millis(1),
            skip: Duration::from_millis(1),
            ..DirectorConfig::default()
        };
        assert!(tiny.validate().is_ok());
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
}
