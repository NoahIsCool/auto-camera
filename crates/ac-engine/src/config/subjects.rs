//! The `[subjects]` table: subject store tuning.
//!
//! Only the parameters an operator tunes are keys; the rest of the store
//! keeps its [`StoreConfig`] defaults. Durations are range-checked when
//! [`SubjectSettings::to_store_config`] builds the store config. An unset
//! `min_score` leaves the floor to the studio:
//! [`StudioConfig::store_config`](super::StudioConfig::store_config) lowers
//! the store default to the detector's
//! [`score_floor`](super::DetectorSettings::score_floor).

use std::time::Duration;

use ac_subjects::StoreConfig;
use serde::{Deserialize, Serialize};

use super::ConfigError;

/// Subject store tuning (the remaining store parameters keep their defaults).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubjectSettings {
    /// Soft cap on subjects per source, in `1..=16`.
    pub max_subjects: usize,
    /// Detections below this score are ignored. Unset, the store default
    /// applies, lowered to the detector's person threshold when OWL matches
    /// people first (see
    /// [`DetectorSettings::score_floor`](super::DetectorSettings::score_floor)).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_score: Option<f32>,
    /// Seconds without a confident observation before a subject is lost.
    pub lost_after_s: f64,
    /// Seconds lost before an unprotected subject is retired.
    pub retire_after_s: f64,
    /// Track observations below this confidence are ignored.
    pub track_min_confidence: f32,
}

impl Default for SubjectSettings {
    fn default() -> Self {
        let d = StoreConfig::default();
        Self {
            max_subjects: d.max_subjects,
            min_score: None,
            lost_after_s: d.lost_after.as_secs_f64(),
            retire_after_s: d.retire_after.as_secs_f64(),
            track_min_confidence: d.track_min_confidence,
        }
    }
}

impl SubjectSettings {
    /// The store config, with durations range-checked; an unset `min_score`
    /// becomes the store default.
    pub fn to_store_config(&self) -> Result<StoreConfig, ConfigError> {
        let secs = |name: &str, v: f64| {
            if v.is_finite() && v > 0.0 && v <= 86_400.0 {
                Ok(Duration::from_secs_f64(v))
            } else {
                Err(ConfigError::Invalid(format!(
                    "subjects.{name} {v} must be in (0, 86400]"
                )))
            }
        };
        Ok(StoreConfig {
            max_subjects: self.max_subjects,
            min_score: self.min_score.unwrap_or(StoreConfig::default().min_score),
            lost_after: secs("lost_after_s", self.lost_after_s)?,
            retire_after: secs("retire_after_s", self.retire_after_s)?,
            track_min_confidence: self.track_min_confidence,
            ..StoreConfig::default()
        })
    }
}
