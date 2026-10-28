//! The `tracker` and `framing` tables: partial tables over another crate's
//! defaults.
//!
//! Their types, [`ac_vision::TemplateTrackerConfig`] and [`FramerConfig`],
//! belong to the crates that use them and have many tuning keys, so a config
//! file gives only the keys it changes. [`partial`] overlays those keys on
//! the serialized default and deserializes the result; a key the default
//! does not have is an error, so a typo cannot pass as a setting. The
//! tracker's type checks itself; [`validate_framing`] checks the framing
//! table, whose type has no check of its own.

use ac_framing::FramerConfig;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};

/// Deserializes a partial table onto `T::default()`, rejecting unknown keys.
pub(super) fn partial<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Serialize + DeserializeOwned + Default,
{
    let patch = serde_json::Value::deserialize(deserializer)?;
    merge_onto_default(patch).map_err(D::Error::custom)
}

/// Overlays the keys of `patch` on the serialized default of `T`.
fn merge_onto_default<T>(patch: serde_json::Value) -> Result<T, String>
where
    T: Serialize + DeserializeOwned + Default,
{
    let mut base = serde_json::to_value(T::default()).map_err(|e| e.to_string())?;
    let (serde_json::Value::Object(fields), serde_json::Value::Object(patch)) = (&mut base, patch)
    else {
        return Err("expected a table".into());
    };
    for (key, value) in patch {
        if !fields.contains_key(&key) {
            return Err(format!("unknown field `{key}`"));
        }
        fields.insert(key, value);
    }
    serde_json::from_value(base).map_err(|e| e.to_string())
}

/// Checks that every framing parameter is a positive finite number.
pub(super) fn validate_framing(f: &FramerConfig) -> Result<(), String> {
    let values = [
        ("pan_half_life", f.pan_half_life),
        ("zoom_half_life", f.zoom_half_life),
        ("max_pan_speed", f.max_pan_speed),
        ("max_zoom_speed", f.max_zoom_speed),
        ("dead_zone_x", f.dead_zone_x),
        ("dead_zone_y", f.dead_zone_y),
        ("dead_zone_zoom", f.dead_zone_zoom),
        ("head_half_life", f.head_half_life),
    ];
    for (name, v) in values {
        if !(v.is_finite() && v > 0.0) {
            return Err(format!("framing.{name} must be a positive number, got {v}"));
        }
    }
    Ok(())
}
