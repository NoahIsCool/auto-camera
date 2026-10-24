//! Tuning of the [`TemplateTracker`](super::TemplateTracker).
//!
//! Defaults target a 30 fps source tracked at 384 px working width. Score
//! thresholds are NCC values (`-1..=1`, clamped to `0..=1` for reporting);
//! distances are pixels at the pyramid level a subject is matched on, which
//! keeps them independent of both source resolution and subject size.

use serde::{Deserialize, Serialize};

use crate::error::VisionError;

/// Tuning for a [`TemplateTracker`](super::TemplateTracker).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateTrackerConfig {
    /// Working width in pixels; frames are downscaled to it, never enlarged.
    pub work_width: u32,
    /// Longest side of the stored template and of boxes at the matching level, in pixels.
    pub template_size: u32,
    /// Search radius as a fraction of the box's longer side at the matching level.
    pub search_fraction: f64,
    /// Smallest search radius, in pixels at the matching level.
    pub min_search_radius: u32,
    /// Largest search radius, in pixels at the matching level.
    pub max_search_radius: u32,
    /// Relative scale step: scales `1 - step`, `1` and `1 + step` are tried. `0` disables scale search.
    pub scale_step: f64,
    /// Score handicap of a scale change.
    pub scale_penalty: f32,
    /// Smallest peak NCC that moves the box.
    pub accept_score: f32,
    /// Smallest gap between the peak and the strongest separate peak.
    pub ambiguity_margin: f32,
    /// Highest confidence reported for an ambiguous match.
    pub ambiguous_confidence: f32,
    /// Largest contrast gain, either way, of a match that scores below
    /// `update_score` and still moves the box.
    ///
    /// The gain is the standard deviation of the image window at the match
    /// over the template's, on the same pixels. NCC ignores it, so a faint
    /// template (a shadow on a wall) also scores 0.6 to 0.76 on a nearby
    /// person of the same shape (gain about 5.5) or on a gentle wall gradient
    /// (gain 0.2 to 0.3). Such a match outside `1 / max_contrast_gain ..=
    /// max_contrast_gain` coasts like an ambiguous one unless it reaches
    /// `update_score`, so a performer under a sudden light cue, who still
    /// matches strongly, is followed and the template relearns the new
    /// contrast. Finite and at least `1`.
    pub max_contrast_gain: f64,
    /// Smallest confidence at which the template learns the new appearance.
    pub update_score: f32,
    /// Weight of the new appearance in a template update.
    pub learning_rate: f32,
    /// Weight of the newest motion in the velocity estimate.
    pub velocity_smoothing: f64,
    /// Velocity multiplier per frame while the match is not trusted.
    pub velocity_decay: f64,
    /// Smallest visible fraction of the template for a placement over the frame border.
    pub min_visible: f64,
    /// Frame interval assumed when timestamps do not advance, in seconds.
    pub nominal_dt: f64,
    /// Longest interval used for prediction, in seconds.
    pub max_dt: f64,
    /// Smallest box side accepted by `start`, in working pixels.
    pub min_box_px: f64,
}

impl Default for TemplateTrackerConfig {
    fn default() -> Self {
        Self {
            work_width: 384,
            template_size: 32,
            search_fraction: 0.5,
            min_search_radius: 4,
            max_search_radius: 24,
            scale_step: 0.05,
            scale_penalty: 0.005,
            accept_score: 0.5,
            ambiguity_margin: 0.05,
            ambiguous_confidence: 0.3,
            max_contrast_gain: 2.5,
            update_score: 0.8,
            learning_rate: 0.1,
            velocity_smoothing: 0.5,
            velocity_decay: 0.7,
            min_visible: 0.5,
            nominal_dt: 1.0 / 30.0,
            max_dt: 0.5,
            min_box_px: 4.0,
        }
    }
}

impl TemplateTrackerConfig {
    /// Checks that every value is in range.
    pub fn validate(&self) -> Result<(), VisionError> {
        let unit = |v: f32| (0.0..=1.0).contains(&v);
        let unit64 = |v: f64| (0.0..=1.0).contains(&v);
        let checks: [(bool, &str); 15] = [
            (self.work_width >= 16, "work_width must be at least 16"),
            (
                (4..=128).contains(&self.template_size),
                "template_size must be in 4..=128",
            ),
            (
                self.search_fraction.is_finite() && self.search_fraction >= 0.0,
                "search_fraction must be >= 0",
            ),
            (
                self.min_search_radius >= 1 && self.min_search_radius <= self.max_search_radius,
                "need 1 <= min_search_radius <= max_search_radius",
            ),
            (
                self.max_search_radius <= 128,
                "max_search_radius must be <= 128",
            ),
            (
                (0.0..0.5).contains(&self.scale_step),
                "scale_step must be in 0..0.5",
            ),
            (
                unit(self.scale_penalty) && unit(self.accept_score),
                "scale_penalty and accept_score must be in 0..=1",
            ),
            (
                unit(self.ambiguity_margin) && unit(self.ambiguous_confidence),
                "ambiguity settings must be in 0..=1",
            ),
            (
                self.max_contrast_gain.is_finite() && self.max_contrast_gain >= 1.0,
                "max_contrast_gain must be finite and >= 1",
            ),
            (
                unit(self.update_score) && unit(self.learning_rate),
                "update_score and learning_rate must be in 0..=1",
            ),
            (
                unit64(self.velocity_smoothing),
                "velocity_smoothing must be in 0..=1",
            ),
            (
                unit64(self.velocity_decay),
                "velocity_decay must be in 0..=1",
            ),
            (
                self.min_visible > 0.0 && self.min_visible <= 1.0,
                "min_visible must be in (0, 1]",
            ),
            (
                self.nominal_dt > 0.0 && self.max_dt >= self.nominal_dt && self.max_dt.is_finite(),
                "need 0 < nominal_dt <= max_dt",
            ),
            (
                self.min_box_px.is_finite() && self.min_box_px >= 2.0,
                "min_box_px must be >= 2",
            ),
        ];
        match checks.iter().find(|(ok, _)| !ok) {
            Some((_, msg)) => Err(VisionError::InvalidConfig((*msg).into())),
            None => Ok(()),
        }
    }
}
