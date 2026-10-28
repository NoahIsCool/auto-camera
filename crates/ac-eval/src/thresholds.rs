//! The scorers' pass/fail limits and the parameters that define their
//! events, read from a TOML file.
//!
//! A scorer turns a trace into numbers ([`crate::scorers`]); a threshold
//! turns a number into a verdict ([`crate::report`]). The two are kept
//! apart so that the same run can be judged against the specification's
//! proposed targets today and against Noah's confirmed ones later, without
//! touching the scorers.
//!
//! # Format
//!
//! TOML, so that every number can carry its specification source in a
//! comment. [`DEFAULT_THRESHOLDS`] is the default file, compiled in; it is
//! the place to read what each number means and where it comes from, and
//! [`Thresholds::default`] is that file parsed. A file has a `version`
//! ([`THRESHOLDS_VERSION`]) and four sections, `framing`, `motion`,
//! `tracking` and `switching`, and holds two kinds of number:
//!
//! * **limits**: the bound a metric must keep to pass, such as
//!   `min_head_torso_inside` or `max_jump_cuts` (the `Option` fields
//!   below). A limit left out leaves its metric unchecked: reported, never
//!   failed. The defaults leave out exactly the limits the specification
//!   gives no number for.
//! * **parameters**: numbers that define what a metric counts, such as the
//!   IoU of a jump cut, the headroom band of a close-up or the minimum
//!   dwell (`min_dwell_talk_s`, which defines a dwell violation and is
//!   bounded by the limit `max_dwell_violations`). They are always
//!   required, because a metric cannot be computed without them.
//!
//! Unknown keys are errors, so a misspelt limit is reported rather than
//! silently unchecked. Reading text is all this module does; finding and
//! opening the file is the runner's job.
//!
//! # Invariants
//!
//! A [`Thresholds`] that passed [`Thresholds::validate`] (as every one
//! [`Thresholds::from_toml`] returns has) holds only finite numbers:
//! fractions and IoUs in `0..=1`, bands with `low <= high`, positive
//! durations and speeds where a zero would make an event undefinable, and
//! non-negative limits elsewhere. The scorers rely on this to never divide
//! by zero or compare against NaN.

use serde::Deserialize;
use thiserror::Error;

use crate::labels::{FieldError, FieldErrors};

/// The thresholds-file version this build reads.
pub const THRESHOLDS_VERSION: u32 = 1;

/// The default thresholds file: the specification's proposed targets, each
/// with its source in a comment.
pub const DEFAULT_THRESHOLDS: &str = include_str!("thresholds/default.toml");

/// Every limit and parameter of the scorers; see the module docs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    /// The file's version, [`THRESHOLDS_VERSION`].
    pub version: u32,
    /// Where the on-air crop puts the framed person ([`crate::scorers::framing`]).
    pub framing: FramingLimits,
    /// How every subject shot's crop moves ([`crate::scorers::motion`],
    /// [`crate::scorers::rest`]).
    pub motion: MotionLimits,
    /// Whether each query finds and keeps its people
    /// ([`crate::scorers::tracking`]).
    pub tracking: TrackingLimits,
    /// What goes on program and when ([`crate::scorers::switching`]).
    pub switching: SwitchingLimits,
}

/// A closed range `[low, high]`, written as a two-number array.
pub type Band = [f64; 2];

/// Limits on framing; fractions of frames unless said otherwise.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FramingLimits {
    /// Least share of on-air frames that keep the framed person's head and
    /// torso inside the crop.
    pub min_head_torso_inside: Option<f64>,
    /// Least share of on-air frames whose headroom lies in its size's band.
    pub min_headroom_in_band: Option<f64>,
    /// Headroom band of a close-up, as a fraction of frame height.
    pub headroom_close: Band,
    /// Headroom band of a medium close-up.
    pub headroom_medium_close: Band,
    /// Headroom band of a medium shot.
    pub headroom_medium: Band,
    /// Headroom band of a full shot.
    pub headroom_full: Band,
    /// Largest distance, in head heights, between the frame's bottom edge
    /// and its size's cut line.
    pub max_lower_cut_error: Option<f64>,
    /// Least share of on-air frames that keep the framed person's box,
    /// down to the frame's bottom edge, inside the crop.
    pub min_subject_containment: Option<f64>,
    /// Least distance, as a fraction of frame height, between the frame's
    /// bottom edge and a joint.
    pub min_joint_clearance: Option<f64>,
}

/// Limits on crop motion.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionLimits {
    /// Largest crop-centre acceleration, in source pixels per second squared.
    pub max_peak_acceleration: Option<f64>,
    /// Largest RMS crop-centre acceleration, in source pixels per second
    /// squared.
    pub max_rms_acceleration: Option<f64>,
    /// Largest crop-centre jerk, in source pixels per second cubed.
    pub max_peak_jerk: Option<f64>,
    /// Largest RMS crop-centre jerk, in source pixels per second cubed.
    pub max_rms_jerk: Option<f64>,
    /// Largest horizontal crop speed, in crop widths per second.
    pub max_pan_speed: Option<f64>,
    /// Largest zoom rate, in natural-log crop widths per second.
    pub max_zoom_rate: Option<f64>,
    /// Shortest hold, in seconds, of a crop that stopped and moved again.
    pub min_static_hold_s: Option<f64>,
    /// Largest crop-height variation, as a fraction of the crop's height,
    /// while its subject is at rest.
    pub max_jitter_at_rest: Option<f64>,
    /// Longest time, in seconds, a crop takes to settle once its subject
    /// is at rest.
    pub max_settle_s: Option<f64>,
    /// Speed below which a crop (in crop spans per second) or a labelled
    /// person (in source spans per second) counts as still.
    pub rest_speed: f64,
    /// How close to its final size and position, as a fraction of the
    /// crop's size, a crop must stay to have settled.
    pub settle_band: f64,
    /// Shortest time, in seconds, a labelled person must stay still to be
    /// at rest.
    pub min_rest_s: f64,
}

/// Limits on finding and keeping people, per query.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingLimits {
    /// IoU at or above which a subject's box matches a labelled person's.
    pub match_iou: f64,
    /// Least share of labelled people found.
    pub min_recall: Option<f64>,
    /// Most subjects per scored keyframe that match nobody.
    pub max_false_subjects_per_frame: Option<f64>,
    /// Most identity switches (CLEAR MOT).
    pub max_id_switches: Option<u64>,
    /// Least IDF1.
    pub min_idf1: Option<f64>,
    /// Longest time, in seconds, from a person's appearance to their first
    /// match.
    pub max_time_to_acquire_s: Option<f64>,
    /// Longest time, in seconds, from losing a person to matching them
    /// again.
    pub max_time_to_reacquire_s: Option<f64>,
}

/// Limits on switching.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwitchingLimits {
    /// Crop IoU above which a take between two shots of one source is a
    /// jump cut.
    pub jump_cut_iou: f64,
    /// Most jump cuts.
    pub max_jump_cuts: Option<u64>,
    /// Shortest ordinary stint on program, in seconds, for talk.
    pub min_dwell_talk_s: f64,
    /// Shortest ordinary stint on program, in seconds, for music.
    pub min_dwell_music_s: f64,
    /// Most takes before the minimum dwell.
    pub max_dwell_violations: Option<u64>,
    /// Longest a wide may stay on program, in seconds.
    pub max_wide_s: f64,
    /// Most wides held on program longer than `max_wide_s`.
    pub max_long_wides: Option<u64>,
    /// Most on-air identity swaps.
    pub max_on_air_swaps: Option<u64>,
    /// Seconds without a new frame, of record PTS, after which a source's
    /// frame is stale and its shots are unavailable.
    pub source_stale_s: f64,
    /// Most takes onto an unavailable or lost shot.
    pub max_takes_onto_unavailable: Option<u64>,
    /// Most seconds in all with program on an unavailable or lost shot.
    pub max_time_unavailable_s: Option<f64>,
    /// Longest a loss on program may last, in seconds.
    pub max_time_to_fall_back_s: Option<f64>,
}

/// Why a thresholds file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ThresholdsError {
    /// The text is not TOML, or does not have the file's shape: a missing
    /// or unknown key, or a value of the wrong type.
    #[error("thresholds file: {0}")]
    Syntax(String),
    /// The file is of another version.
    #[error(
        "thresholds file: version {0} is not supported (this build reads {THRESHOLDS_VERSION})"
    )]
    Version(i64),
    /// The file has no `version`.
    #[error("thresholds file: no version")]
    NoVersion,
    /// A number breaks a rule of the module docs.
    #[error("thresholds file:\n{0}")]
    Invalid(FieldErrors),
}

impl Default for Thresholds {
    /// [`DEFAULT_THRESHOLDS`], parsed.
    fn default() -> Self {
        Self::from_toml(DEFAULT_THRESHOLDS).expect("the default thresholds file is valid")
    }
}

impl Thresholds {
    /// Reads a thresholds file's `text`, checking its version before
    /// anything else and every rule after.
    pub fn from_toml(text: &str) -> Result<Self, ThresholdsError> {
        let table: toml::Table =
            toml::from_str(text).map_err(|e| ThresholdsError::Syntax(e.to_string()))?;
        match table.get("version").map(toml::Value::as_integer) {
            None => return Err(ThresholdsError::NoVersion),
            Some(Some(v)) if v == i64::from(THRESHOLDS_VERSION) => {}
            Some(Some(v)) => return Err(ThresholdsError::Version(v)),
            Some(None) => {
                return Err(ThresholdsError::Syntax("version is not an integer".into()));
            }
        }
        let thresholds: Self =
            toml::from_str(text).map_err(|e| ThresholdsError::Syntax(e.to_string()))?;
        thresholds.validate().map_err(ThresholdsError::Invalid)?;
        Ok(thresholds)
    }

    /// Checks every rule of the module docs, naming each broken one by its
    /// key (`framing.headroom_close`).
    pub fn validate(&self) -> Result<(), FieldErrors> {
        let mut c = Checker::default();
        if self.version != THRESHOLDS_VERSION {
            c.fail("version", format!("must be {THRESHOLDS_VERSION}"));
        }
        let f = &self.framing;
        c.optional(
            "framing.min_head_torso_inside",
            f.min_head_torso_inside,
            Rule::Fraction,
        );
        c.optional(
            "framing.min_headroom_in_band",
            f.min_headroom_in_band,
            Rule::Fraction,
        );
        c.band("framing.headroom_close", f.headroom_close);
        c.band("framing.headroom_medium_close", f.headroom_medium_close);
        c.band("framing.headroom_medium", f.headroom_medium);
        c.band("framing.headroom_full", f.headroom_full);
        c.optional(
            "framing.max_lower_cut_error",
            f.max_lower_cut_error,
            Rule::NonNegative,
        );
        c.optional(
            "framing.min_subject_containment",
            f.min_subject_containment,
            Rule::Fraction,
        );
        c.optional(
            "framing.min_joint_clearance",
            f.min_joint_clearance,
            Rule::Fraction,
        );
        let m = &self.motion;
        c.optional(
            "motion.max_peak_acceleration",
            m.max_peak_acceleration,
            Rule::NonNegative,
        );
        c.optional(
            "motion.max_rms_acceleration",
            m.max_rms_acceleration,
            Rule::NonNegative,
        );
        c.optional("motion.max_peak_jerk", m.max_peak_jerk, Rule::NonNegative);
        c.optional("motion.max_rms_jerk", m.max_rms_jerk, Rule::NonNegative);
        c.optional("motion.max_pan_speed", m.max_pan_speed, Rule::NonNegative);
        c.optional("motion.max_zoom_rate", m.max_zoom_rate, Rule::NonNegative);
        c.optional(
            "motion.min_static_hold_s",
            m.min_static_hold_s,
            Rule::NonNegative,
        );
        c.optional(
            "motion.max_jitter_at_rest",
            m.max_jitter_at_rest,
            Rule::Fraction,
        );
        c.optional("motion.max_settle_s", m.max_settle_s, Rule::NonNegative);
        c.required("motion.rest_speed", m.rest_speed, Rule::Positive);
        c.required("motion.settle_band", m.settle_band, Rule::Fraction);
        c.required("motion.min_rest_s", m.min_rest_s, Rule::Positive);
        let t = &self.tracking;
        c.required("tracking.match_iou", t.match_iou, Rule::Iou);
        c.optional("tracking.min_recall", t.min_recall, Rule::Fraction);
        c.optional(
            "tracking.max_false_subjects_per_frame",
            t.max_false_subjects_per_frame,
            Rule::NonNegative,
        );
        c.optional("tracking.min_idf1", t.min_idf1, Rule::Fraction);
        c.optional(
            "tracking.max_time_to_acquire_s",
            t.max_time_to_acquire_s,
            Rule::NonNegative,
        );
        c.optional(
            "tracking.max_time_to_reacquire_s",
            t.max_time_to_reacquire_s,
            Rule::NonNegative,
        );
        let s = &self.switching;
        c.required("switching.jump_cut_iou", s.jump_cut_iou, Rule::Fraction);
        c.required(
            "switching.min_dwell_talk_s",
            s.min_dwell_talk_s,
            Rule::Positive,
        );
        c.required(
            "switching.min_dwell_music_s",
            s.min_dwell_music_s,
            Rule::Positive,
        );
        c.required("switching.max_wide_s", s.max_wide_s, Rule::Positive);
        c.required("switching.source_stale_s", s.source_stale_s, Rule::Positive);
        c.optional(
            "switching.max_time_unavailable_s",
            s.max_time_unavailable_s,
            Rule::NonNegative,
        );
        c.optional(
            "switching.max_time_to_fall_back_s",
            s.max_time_to_fall_back_s,
            Rule::NonNegative,
        );
        c.finish()
    }
}

/// What a number must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// In `0..=1`.
    Fraction,
    /// In `0..` (a limit; zero allows nothing).
    NonNegative,
    /// Above zero (a parameter an event is measured against).
    Positive,
    /// In `0..=1` and above zero: an IoU of zero would match boxes that do
    /// not overlap.
    Iou,
}

impl Rule {
    /// Whether finite `value` keeps the rule.
    fn admits(self, value: f64) -> bool {
        match self {
            Rule::Fraction => (0.0..=1.0).contains(&value),
            Rule::NonNegative => value >= 0.0,
            Rule::Positive => value > 0.0,
            Rule::Iou => value > 0.0 && value <= 1.0,
        }
    }

    /// The rule in words, for an error message.
    fn words(self) -> &'static str {
        match self {
            Rule::Fraction => "must be from 0 to 1",
            Rule::NonNegative => "must not be negative",
            Rule::Positive => "must be above 0",
            Rule::Iou => "must be above 0 and at most 1",
        }
    }
}

/// The errors found so far, in key order of the file.
#[derive(Debug, Default)]
struct Checker(Vec<FieldError>);

impl Checker {
    /// Records that `path` breaks a rule.
    fn fail(&mut self, path: &str, message: impl Into<String>) {
        self.0.push(FieldError::new(path, message));
    }

    /// Checks a required number.
    fn required(&mut self, path: &str, value: f64, rule: Rule) {
        if !value.is_finite() {
            self.fail(path, format!("{value} is not a finite number"));
        } else if !rule.admits(value) {
            self.fail(path, format!("{value} {}", rule.words()));
        }
    }

    /// Checks a limit, when given.
    fn optional(&mut self, path: &str, value: Option<f64>, rule: Rule) {
        if let Some(value) = value {
            self.required(path, value, rule);
        }
    }

    /// Checks a band of fractions: both ends in `0..=1`, low first.
    fn band(&mut self, path: &str, [low, high]: Band) {
        let before = self.0.len();
        self.required(&format!("{path}[0]"), low, Rule::Fraction);
        self.required(&format!("{path}[1]"), high, Rule::Fraction);
        if self.0.len() == before && low > high {
            self.fail(path, format!("low end {low} is above high end {high}"));
        }
    }

    /// `Ok` when nothing was recorded.
    fn finish(self) -> Result<(), FieldErrors> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(FieldErrors(self.0))
        }
    }
}

#[cfg(test)]
mod tests;
