//! The `[discovery]` table: how often detection runs and when its results
//! are stale.
//!
//! Every limit is written in seconds and handed to the engine as a
//! [`Duration`]. The ages are media time: a result is measured against the
//! newest frame of its source, a subject against the newest detection. A
//! request is abandoned `timeout_s` after it was handed to the source's
//! worker, which a sidecar's own request deadline must stay below (see
//! [`StudioConfig::validate`](super::StudioConfig::validate)).

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How often discovery runs and when its results are stale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoverySettings {
    /// Detector runs per second per source, at most.
    pub rate_hz: f64,
    /// Results computed on a frame older than this (relative to the newest
    /// frame of the source) are dropped, in seconds.
    pub max_result_age_s: f64,
    /// A request without an answer after this long is abandoned, in seconds.
    pub timeout_s: f64,
    /// A subject the detector has not seen for this long (relative to the
    /// newest detection) is no longer kept alive by its tracker, in seconds.
    pub max_undetected_s: f64,
}

impl Default for DiscoverySettings {
    fn default() -> Self {
        Self {
            rate_hz: 1.0,
            max_result_age_s: 2.0,
            timeout_s: 10.0,
            max_undetected_s: 3.0,
        }
    }
}

impl DiscoverySettings {
    /// Checks the rate and the ages.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.rate_hz > 0.0 && self.rate_hz <= 30.0) {
            return Err(format!(
                "discovery.rate_hz {} must be in (0, 30]",
                self.rate_hz
            ));
        }
        for (name, v) in [
            ("max_result_age_s", self.max_result_age_s),
            ("timeout_s", self.timeout_s),
            ("max_undetected_s", self.max_undetected_s),
        ] {
            if !(v > 0.0 && v <= 3600.0) {
                return Err(format!("discovery.{name} {v} must be in (0, 3600]"));
            }
        }
        Ok(())
    }

    /// Interval between requests.
    pub fn interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.rate_hz)
    }

    /// Largest accepted result age.
    pub fn max_result_age(&self) -> Duration {
        Duration::from_secs_f64(self.max_result_age_s)
    }

    /// Request timeout.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs_f64(self.timeout_s)
    }

    /// Longest a tracker may keep an undetected subject alive.
    pub fn max_undetected(&self) -> Duration {
        Duration::from_secs_f64(self.max_undetected_s)
    }
}
