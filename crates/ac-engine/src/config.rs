//! The studio configuration: everything needed to start an engine.
//!
//! A [`StudioConfig`] is plain data, loaded from TOML or JSON
//! ([`StudioConfig::load`]) and checked once by [`StudioConfig::validate`]
//! before an engine starts, so the engine never sees an inconsistent setup.
//! Every section has defaults, so a minimal file only lists its sources:
//!
//! ```toml
//! [[sources]]
//! id = "stage"
//! kind = "synthetic"
//! width = 1280
//! height = 720
//! fps = 30
//! ```
//!
//! Sections mirror the pipeline: `sources` (any [`SourceConfig`] plus an id
//! and a name), `queries` (prompts with their framing presets), `detector`
//! and `discovery` (the slow open-vocabulary tier), `tracker` (the fast
//! tier), `subjects` (identity management), `framing`, `director`,
//! `switcher`, `output` (program size, rate and destinations) and `monitors`
//! (what the operator UI is sent).
//!
//! The `tracker` and `framing` tables are partial: keys given override the
//! crate defaults and unknown keys are rejected. Relative file paths
//! (`sources[].path` and detector model paths) are resolved against the
//! directory of the config file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ac_core::{Preset, QueryId, ShotSize, SourceId};
use ac_director::DirectorConfig;
use ac_framing::FramerConfig;
use ac_sources::SourceConfig;
use ac_subjects::StoreConfig;
use ac_vision::TemplateTrackerConfig;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

mod settings;

pub use settings::{
    DetectorSettings, DirectorSettings, DiscoverySettings, EncoderSetting, MonitorSettings,
    OutputSettings, SubjectSettings, SwitcherSettings,
};

/// Errors from loading or validating a [`StudioConfig`].
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The file is not valid TOML/JSON for a studio config.
    #[error("cannot parse {path}: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// The parser's message.
        message: String,
    },
    /// A value is out of range or inconsistent; the message names it.
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// One video source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceEntry {
    /// Stable identifier, used in shot ids; letters, digits, `-` and `_`.
    pub id: String,
    /// Operator-facing name (defaults to the id).
    #[serde(default)]
    pub name: Option<String>,
    /// How to open it (`kind = "file"` or `"synthetic"`).
    #[serde(flatten)]
    pub input: SourceConfig,
}

impl SourceEntry {
    /// The source id.
    pub fn source_id(&self) -> SourceId {
        SourceId(self.id.clone())
    }

    /// The operator-facing name.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// A preset given either as a bare shot size or in full.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PresetSpec {
    /// A shot size with the standard headroom and zoom limit.
    Size(ShotSize),
    /// A fully specified preset.
    Full(Preset),
}

impl PresetSpec {
    /// The preset this spec describes.
    pub fn to_preset(&self) -> Preset {
        match self {
            PresetSpec::Size(size) => preset_for(*size),
            PresetSpec::Full(p) => p.clone(),
        }
    }
}

/// The standard preset for `size`, with a snake-case id and a title-case name.
pub fn preset_for(size: ShotSize) -> Preset {
    let (id, name) = match size {
        ShotSize::Wide => ("wide", "Wide"),
        ShotSize::Full => ("full", "Full"),
        ShotSize::Medium => ("medium", "Medium"),
        ShotSize::MediumClose => ("medium_close", "Medium close"),
        ShotSize::Close => ("close", "Close"),
    };
    Preset::new(id, name, size)
}

/// Default presets of a query: medium and close.
fn default_presets() -> Vec<PresetSpec> {
    vec![
        PresetSpec::Size(ShotSize::Medium),
        PresetSpec::Size(ShotSize::Close),
    ]
}

/// A text prompt and the framings applied to every subject it matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySettings {
    /// Stable identifier (defaults to a slug of the name).
    #[serde(default)]
    pub id: Option<String>,
    /// Operator-facing name, such as "Pastor".
    pub name: String,
    /// Prompt given to the detector, such as "person's upper body".
    pub prompt: String,
    /// Framing presets (default: medium and close).
    #[serde(default = "default_presets")]
    pub presets: Vec<PresetSpec>,
}

impl QuerySettings {
    /// The query id: the explicit id or a slug of the name.
    pub fn query_id(&self) -> QueryId {
        QueryId(self.id.clone().unwrap_or_else(|| slug(&self.name)))
    }
}

/// Lower-case `text` with runs of anything but letters and digits replaced by
/// one `-`; `"query"` when nothing is left.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "query".into() } else { out }
}

/// Deserializes a partial table onto `T::default()`, rejecting unknown keys.
fn partial<'de, D, T>(deserializer: D) -> Result<T, D::Error>
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

/// Everything needed to start an engine; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudioConfig {
    /// Video inputs, at least one.
    pub sources: Vec<SourceEntry>,
    /// Text prompts and their presets.
    #[serde(default)]
    pub queries: Vec<QuerySettings>,
    /// The open-vocabulary detector.
    #[serde(default)]
    pub detector: DetectorSettings,
    /// How often detection runs and when its results are stale.
    #[serde(default)]
    pub discovery: DiscoverySettings,
    /// Fast tracker tuning (partial table over the defaults).
    #[serde(default, deserialize_with = "partial")]
    pub tracker: TemplateTrackerConfig,
    /// Subject store tuning.
    #[serde(default)]
    pub subjects: SubjectSettings,
    /// Framing controller tuning (partial table over the defaults).
    #[serde(default, deserialize_with = "partial")]
    pub framing: FramerConfig,
    /// Auto-director mode and profile.
    #[serde(default)]
    pub director: DirectorSettings,
    /// Initial transition.
    #[serde(default)]
    pub switcher: SwitcherSettings,
    /// Program size, rate, encoder and destinations.
    #[serde(default)]
    pub output: OutputSettings,
    /// Monitor and multiview rendering for the operator UI.
    #[serde(default)]
    pub monitors: MonitorSettings,
}

impl StudioConfig {
    /// A config with one source and every other section at its default.
    pub fn with_source(id: &str, input: SourceConfig) -> Self {
        Self {
            sources: vec![SourceEntry {
                id: id.into(),
                name: None,
                input,
            }],
            queries: Vec::new(),
            detector: DetectorSettings::default(),
            discovery: DiscoverySettings::default(),
            tracker: TemplateTrackerConfig::default(),
            subjects: SubjectSettings::default(),
            framing: FramerConfig::default(),
            director: DirectorSettings::default(),
            switcher: SwitcherSettings::default(),
            output: OutputSettings::default(),
            monitors: MonitorSettings::default(),
        }
    }

    /// Reads, parses and validates the file at `path` (`.json` as JSON,
    /// anything else as TOML), resolving relative paths against its directory.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.into(),
            source,
        })?;
        let json = path.extension().is_some_and(|e| e == "json");
        let mut config = Self::parse(&text, json).map_err(|message| ConfigError::Parse {
            path: path.into(),
            message,
        })?;
        if let Some(dir) = path.parent() {
            config.resolve_paths(dir);
        }
        config.validate()?;
        Ok(config)
    }

    /// Parses `text` as JSON or TOML without validating it.
    pub fn parse(text: &str, json: bool) -> Result<Self, String> {
        if json {
            serde_json::from_str(text).map_err(|e| e.to_string())
        } else {
            toml::from_str(text).map_err(|e| e.to_string())
        }
    }

    /// Makes relative source and model paths relative to `dir`.
    pub fn resolve_paths(&mut self, dir: &Path) {
        let fix = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = dir.join(&*p);
            }
        };
        for s in &mut self.sources {
            if let SourceConfig::File { path, .. } = &mut s.input {
                fix(path);
            }
        }
        if let DetectorSettings::Owl {
            model,
            text_encoder,
            tokenizer,
            ..
        } = &mut self.detector
        {
            fix(model);
            fix(tokenizer);
            if let Some(t) = text_encoder {
                fix(t);
            }
        }
    }

    /// Checks the whole config, returning the first problem found.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let bad = |msg: String| Err(ConfigError::Invalid(msg));
        if self.sources.is_empty() {
            return bad("at least one source is required".into());
        }
        let mut ids = HashSet::new();
        for s in &self.sources {
            if !valid_id(&s.id) {
                return bad(format!(
                    "source id {:?} must be non-empty letters, digits, '-' or '_'",
                    s.id
                ));
            }
            if !ids.insert(s.id.as_str()) {
                return bad(format!("duplicate source id {:?}", s.id));
            }
            if matches!(self.detector, DetectorSettings::Synthetic { .. })
                && !matches!(s.input, SourceConfig::Synthetic { .. })
            {
                return bad(format!(
                    "detector kind \"synthetic\" only works with synthetic sources; source {:?} is not one",
                    s.id
                ));
            }
        }
        let mut queries = HashSet::new();
        for q in &self.queries {
            let presets: Vec<Preset> = q.presets.iter().map(PresetSpec::to_preset).collect();
            validate_query(&q.name, &q.prompt, &presets).map_err(ConfigError::Invalid)?;
            let id = q.query_id();
            if !valid_id(&id.0) {
                return bad(format!("query id {:?} is not a valid id", id.0));
            }
            if !queries.insert(id.0.clone()) {
                return bad(format!("duplicate query id {:?}", id.0));
            }
        }
        self.detector.validate().map_err(ConfigError::Invalid)?;
        self.discovery.validate().map_err(ConfigError::Invalid)?;
        self.tracker
            .validate()
            .map_err(|e| ConfigError::Invalid(format!("tracker: {e}")))?;
        self.store_config()?;
        self.director_config()?;
        validate_framing(&self.framing).map_err(ConfigError::Invalid)?;
        self.switcher.validate().map_err(ConfigError::Invalid)?;
        self.output.validate().map_err(ConfigError::Invalid)?;
        if let Some(audio) = &self.output.audio_source
            && !ids.contains(audio.as_str())
        {
            return bad(format!("output.audio_source {audio:?} is not a source id"));
        }
        self.monitors.validate().map_err(ConfigError::Invalid)
    }

    /// The subject store tuning, validated.
    pub fn store_config(&self) -> Result<StoreConfig, ConfigError> {
        let c = self.subjects.to_store_config()?;
        c.validate()
            .map_err(|e| ConfigError::Invalid(format!("subjects: {e}")))?;
        Ok(c)
    }

    /// The director parameters, validated.
    pub fn director_config(&self) -> Result<DirectorConfig, ConfigError> {
        let c = DirectorConfig::new(self.director.mode, self.director.profile);
        c.validate()
            .map_err(|e| ConfigError::Invalid(format!("director: {e}")))?;
        Ok(c)
    }

    /// The program frame interval.
    pub fn frame_period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.output.fps.max(1)))
    }
}

/// True for non-empty ids of ASCII letters, digits, `-` and `_`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Checks a query's name, prompt and presets (shared with `AddQuery`).
pub fn validate_query(name: &str, prompt: &str, presets: &[Preset]) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("query name must not be empty".into());
    }
    if prompt.trim().is_empty() {
        return Err(format!("query {name:?}: prompt must not be empty"));
    }
    if presets.is_empty() {
        return Err(format!("query {name:?}: at least one preset is required"));
    }
    let mut seen = HashSet::new();
    for p in presets {
        if !valid_id(&p.id) {
            return Err(format!("query {name:?}: preset id {:?} is invalid", p.id));
        }
        if !seen.insert(p.id.as_str()) {
            return Err(format!("query {name:?}: duplicate preset {:?}", p.id));
        }
        if p.size == ShotSize::Wide {
            return Err(format!(
                "query {name:?}: preset {:?} is a wide; every source already has a wide shot",
                p.id
            ));
        }
        if !(0.0..=0.5).contains(&p.headroom) {
            return Err(format!(
                "query {name:?}: preset {:?} headroom must be in 0..=0.5",
                p.id
            ));
        }
        if !(1.0..=20.0).contains(&p.max_zoom) {
            return Err(format!(
                "query {name:?}: preset {:?} max_zoom must be in 1..=20",
                p.id
            ));
        }
    }
    Ok(())
}

/// Checks that every framing parameter is a positive finite number.
fn validate_framing(f: &FramerConfig) -> Result<(), String> {
    let values = [
        ("pan_half_life", f.pan_half_life),
        ("zoom_half_life", f.zoom_half_life),
        ("max_pan_speed", f.max_pan_speed),
        ("max_zoom_speed", f.max_zoom_speed),
        ("dead_zone_x", f.dead_zone_x),
        ("dead_zone_y", f.dead_zone_y),
        ("dead_zone_zoom", f.dead_zone_zoom),
    ];
    for (name, v) in values {
        if !(v.is_finite() && v > 0.0) {
            return Err(format!("framing.{name} must be a positive number, got {v}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
