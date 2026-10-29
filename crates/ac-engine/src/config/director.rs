//! The `[director]` table: the auto-director's mode and content profile,
//! and the operator's side of it.
//!
//! `mode`, `profile` and `takeover` are `ac_director` enums with their own
//! defaults (suggest, talk, suggest); the director's parameters are derived
//! from them and checked when the studio builds the director
//! ([`StudioConfig::director_config`](super::StudioConfig::director_config)).
//! `takeover` is what the operator's own switching does while the director
//! runs in Auto mode: `"suggest"` drops it to Suggest, `"pause"` holds its
//! takes for `pause_s` after each manual action, `"director"` changes
//! nothing ([`ac_director::Takeover`]). `skip_s` is how long a skipped
//! suggestion stays out of the suggestions. Both durations are seconds,
//! range-checked by [`DirectorSettings::to_director_config`].

use std::time::Duration;

use ac_director::{ContentProfile, DirectorConfig, DirectorMode, Takeover};
use serde::{Deserialize, Serialize};

use super::ConfigError;

/// Longest pause of auto-takes, in seconds.
const MAX_PAUSE_S: f64 = 600.0;
/// Longest skip window, in seconds.
const MAX_SKIP_S: f64 = 3600.0;

/// Auto-director mode, content profile and the operator's side.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DirectorSettings {
    /// Off, suggest (default) or auto.
    pub mode: DirectorMode,
    /// Talk (default) or music.
    pub profile: ContentProfile,
    /// What a manual action does in Auto mode: suggest (default), pause or
    /// director.
    pub takeover: Takeover,
    /// Seconds each manual action holds auto-takes with `takeover =
    /// "pause"`, in `(0, 600]`.
    pub pause_s: f64,
    /// Seconds a skipped suggestion stays out of the suggestions, in
    /// `(0, 3600]`.
    pub skip_s: f64,
}

impl Default for DirectorSettings {
    fn default() -> Self {
        let d = DirectorConfig::default();
        Self {
            mode: d.mode,
            profile: d.profile,
            takeover: d.takeover,
            pause_s: d.pause.as_secs_f64(),
            skip_s: d.skip.as_secs_f64(),
        }
    }
}

impl DirectorSettings {
    /// The director parameters for the mode and profile, with the
    /// operator's side, the durations range-checked (the rest is checked by
    /// [`DirectorConfig::validate`]).
    pub fn to_director_config(&self) -> Result<DirectorConfig, ConfigError> {
        let secs = |name: &str, v: f64, max: f64| {
            if v.is_finite() && v > 0.0 && v <= max {
                Ok(Duration::from_secs_f64(v))
            } else {
                Err(ConfigError::Invalid(format!(
                    "director.{name} {v} must be in (0, {max}]"
                )))
            }
        };
        Ok(DirectorConfig {
            takeover: self.takeover,
            pause: secs("pause_s", self.pause_s, MAX_PAUSE_S)?,
            skip: secs("skip_s", self.skip_s, MAX_SKIP_S)?,
            ..DirectorConfig::new(self.mode, self.profile)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `[director]` table parsed from `toml`.
    fn parse(toml: &str) -> Result<DirectorSettings, String> {
        toml::from_str(toml).map_err(|e| e.to_string())
    }

    #[test]
    fn defaults_drop_auto_to_suggest_on_a_manual_action() {
        let d = parse("").unwrap();
        assert_eq!(d, DirectorSettings::default());
        let c = d.to_director_config().unwrap();
        assert_eq!(c.mode, DirectorMode::Suggest);
        assert_eq!(c.takeover, Takeover::Suggest);
        assert_eq!(c.pause, Duration::from_secs(10));
        assert_eq!(c.skip, Duration::from_secs(30));
    }

    #[test]
    fn every_takeover_setting_and_window_is_read() {
        for (name, takeover) in [
            ("suggest", Takeover::Suggest),
            ("pause", Takeover::Pause),
            ("director", Takeover::Director),
        ] {
            let d = parse(&format!(
                "mode = \"auto\"\ntakeover = \"{name}\"\npause_s = 4.5\nskip_s = 12"
            ))
            .unwrap();
            let c = d.to_director_config().unwrap();
            assert_eq!(c.takeover, takeover);
            assert_eq!(c.mode, DirectorMode::Auto);
            assert_eq!(c.pause, Duration::from_millis(4500));
            assert_eq!(c.skip, Duration::from_secs(12));
        }
        assert!(parse("takeover = \"operator\"").is_err());
        assert!(parse("pause = 3").is_err(), "unknown keys are errors");
    }

    #[test]
    fn windows_out_of_range_are_rejected() {
        for (key, value) in [
            ("pause_s", "0"),
            ("pause_s", "-1"),
            ("pause_s", "601"),
            ("pause_s", "nan"),
            ("skip_s", "0"),
            ("skip_s", "3601"),
            ("skip_s", "inf"),
        ] {
            let d = parse(&format!("{key} = {value}")).unwrap();
            let err = d.to_director_config().unwrap_err().to_string();
            assert!(err.contains(key), "{key} = {value}: {err}");
        }
        let edges = parse("pause_s = 600\nskip_s = 0.1").unwrap();
        assert!(edges.to_director_config().is_ok());
    }
}
