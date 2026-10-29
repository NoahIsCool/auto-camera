//! The `[monitors]` table: what is rendered for the operator UI.
//!
//! The program and preview monitors and the multiview tiles each have a
//! size and a refresh rate. A rate of 0 turns them off, which is how a
//! headless run, with nobody watching, skips the work
//! ([`MonitorSettings::disabled`]).

use serde::{Deserialize, Serialize};

/// What is rendered for the operator UI.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MonitorSettings {
    /// Program and preview monitor width.
    pub width: u32,
    /// Program and preview monitor height.
    pub height: u32,
    /// Monitor refreshes per second; 0 disables the monitors.
    pub rate_hz: f64,
    /// Multiview tile width.
    pub thumb_width: u32,
    /// Multiview tile height.
    pub thumb_height: u32,
    /// Multiview refreshes per second; 0 disables the tiles.
    pub thumb_rate_hz: f64,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            width: 960,
            height: 540,
            rate_hz: 30.0,
            thumb_width: 320,
            thumb_height: 180,
            thumb_rate_hz: 10.0,
        }
    }
}

impl MonitorSettings {
    /// Settings with monitors and tiles disabled (headless runs).
    pub fn disabled() -> Self {
        Self {
            rate_hz: 0.0,
            thumb_rate_hz: 0.0,
            ..Self::default()
        }
    }

    /// True when program is rendered for a display (the operator window's
    /// program monitor), which must then not sleep while output runs.
    pub fn feeds_display(&self) -> bool {
        self.rate_hz > 0.0
    }

    /// Checks sizes and rates.
    pub fn validate(&self) -> Result<(), String> {
        let side = |v: u32| (16..=3840).contains(&v);
        if !side(self.width) || !side(self.height) || !side(self.thumb_width) {
            return Err("monitor sizes must be within 16..=3840".into());
        }
        if !side(self.thumb_height) {
            return Err("monitor sizes must be within 16..=3840".into());
        }
        for (name, v) in [
            ("rate_hz", self.rate_hz),
            ("thumb_rate_hz", self.thumb_rate_hz),
        ] {
            if !(v.is_finite() && (0.0..=120.0).contains(&v)) {
                return Err(format!("monitors.{name} {v} must be in 0..=120"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_feeds_a_display_while_the_monitors_refresh() {
        assert!(MonitorSettings::default().feeds_display());
        assert!(!MonitorSettings::disabled().feeds_display());
        // The multiview alone does not show program.
        let tiles_only = MonitorSettings {
            rate_hz: 0.0,
            ..MonitorSettings::default()
        };
        assert!(!tiles_only.feeds_display());
    }
}
