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
//! and a name), `queries` (prompts with their framing presets), `lexicon`
//! (additions to the prompt parser's shot, role and person words),
//! `detector` and `discovery` (the slow open-vocabulary tier), `tracker`
//! (the fast tier), `subjects` (identity management), `framing`,
//! `director`, `switcher`, `output` (program size, rate and destinations)
//! and `monitors` (what the operator UI is sent).
//!
//! A query's prompt is read once, at load, with the built-in lexicon plus
//! the config's `lexicon` additions ([`StudioConfig::prompt_lexicon`]):
//! shot words pick its presets when it lists none, role words become the
//! detector phrase, and `literal = true` sends the text as typed. Explicit
//! `presets` always win over shot words. A role that names an action draws
//! a notice unless the `detector` sees actions
//! ([`DetectorSettings::sees_actions`]); see [`crate::queries`].
//!
//! The `detector` table picks one detector `kind` for the whole studio:
//! `"none"`, `"synthetic"` (ground truth of synthetic sources), `"owl"`
//! (OWLv2 through ONNX Runtime, one instance per source) or `"sidecar"` (a
//! detector in its own process, SAM 3.1 on MLX first, started once and
//! shared by every source; [`SidecarSettings`]). Every query goes to it.
//!
//! The `tracker` and `framing` tables are partial: keys given override the
//! crate defaults and unknown keys are rejected. Relative file paths
//! (`sources[].path`, detector model paths and the path-like elements of a
//! sidecar's `command`) are resolved against the directory of the config
//! file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ac_core::{Lexicon, Preset, Query, QueryId, ShotSize, SourceId};
use ac_director::DirectorConfig;
use ac_framing::FramerConfig;
use ac_sources::SourceConfig;
use ac_subjects::StoreConfig;
use ac_vision::TemplateTrackerConfig;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::queries::PromptReader;

mod settings;
mod sidecar;

pub use settings::{
    DetectorSettings, DirectorSettings, DiscoverySettings, EncoderSetting, MonitorSettings,
    OutputSettings, SubjectSettings, SwitcherSettings,
};
pub use sidecar::SidecarSettings;

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

/// A text prompt and the framings applied to every subject it matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySettings {
    /// Stable identifier (defaults to a slug of the name).
    #[serde(default)]
    pub id: Option<String>,
    /// Operator-facing name, such as "Pastor".
    pub name: String,
    /// The operator's prompt, such as "speaker upper body": shot words pick
    /// framings, role words are rewritten for the detector (see
    /// [`crate::queries`]).
    pub prompt: String,
    /// Framing presets. When given they always win over the prompt's shot
    /// words; when absent the shot words decide, and without shot words the
    /// default is medium and close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<Vec<PresetSpec>>,
    /// Send the prompt to the detector exactly as typed, without reading
    /// shot or role words (for experiments).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub literal: bool,
}

impl QuerySettings {
    /// A query named `name` for `prompt`, with no explicit presets.
    pub fn new(name: &str, prompt: &str) -> Self {
        Self {
            id: None,
            name: name.into(),
            prompt: prompt.into(),
            presets: None,
            literal: false,
        }
    }

    /// The query id: the explicit id or a slug of the name.
    pub fn query_id(&self) -> QueryId {
        QueryId(self.id.clone().unwrap_or_else(|| slug(&self.name)))
    }

    /// The explicit presets, if any.
    pub fn explicit_presets(&self) -> Option<Vec<Preset>> {
        self.presets
            .as_ref()
            .map(|specs| specs.iter().map(PresetSpec::to_preset).collect())
    }

    /// The query, its prompt read with `reader`; see [`crate::queries`].
    pub fn build(&self, reader: &PromptReader) -> Result<Query, String> {
        crate::queries::build_query(
            self.query_id(),
            &self.name,
            &self.prompt,
            self.explicit_presets(),
            self.literal,
            reader,
        )
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
    /// Shot, role and person words added to the built-in lexicon; an entry
    /// with the key of a built-in one replaces it.
    #[serde(default)]
    pub lexicon: Lexicon,
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
            lexicon: Lexicon::default(),
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

    /// Makes relative source, model and sidecar command paths relative to
    /// `dir`.
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
        match &mut self.detector {
            DetectorSettings::Owl {
                model,
                text_encoder,
                tokenizer,
                ..
            } => {
                fix(model);
                fix(tokenizer);
                if let Some(t) = text_encoder {
                    fix(t);
                }
            }
            DetectorSettings::Sidecar(sidecar) => sidecar.resolve_paths(dir),
            DetectorSettings::None | DetectorSettings::Synthetic { .. } => {}
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
        self.lexicon
            .validate()
            .map_err(|e| ConfigError::Invalid(format!("lexicon: {e}")))?;
        self.build_queries()?;
        self.detector.validate().map_err(ConfigError::Invalid)?;
        self.discovery.validate().map_err(ConfigError::Invalid)?;
        if let DetectorSettings::Sidecar(sidecar) = &self.detector
            && sidecar.request_timeout_s >= self.discovery.timeout_s
        {
            return bad(format!(
                "detector.request_timeout_s ({} s) must be below discovery.timeout_s ({} s), \
                 after which the engine abandons a request",
                sidecar.request_timeout_s, self.discovery.timeout_s
            ));
        }
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

    /// The prompt parser's vocabulary: the built-in lexicon with the
    /// config's `lexicon` additions applied in order (an entry with the key
    /// of an earlier one replaces it).
    pub fn prompt_lexicon(&self) -> Lexicon {
        let mut lexicon = Lexicon::builtin();
        for shot in &self.lexicon.shots {
            lexicon.set_shot(shot.clone());
        }
        for role in &self.lexicon.roles {
            lexicon.set_role(role.clone());
        }
        for word in &self.lexicon.person_words {
            lexicon.add_person_word(word);
        }
        lexicon
    }

    /// The queries in config order, each prompt read once with the
    /// studio's [`PromptReader`] (its lexicon and its detector); fails on
    /// the first invalid or duplicate query.
    pub fn build_queries(&self) -> Result<Vec<Query>, ConfigError> {
        let reader = PromptReader::of(self);
        let mut ids = HashSet::new();
        let mut out = Vec::new();
        for q in &self.queries {
            let id = q.query_id();
            if !valid_id(&id.0) {
                return Err(ConfigError::Invalid(format!(
                    "query id {:?} is not a valid id",
                    id.0
                )));
            }
            let query = q.build(&reader).map_err(ConfigError::Invalid)?;
            if !ids.insert(id.0.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate query id {:?}",
                    id.0
                )));
            }
            out.push(query);
        }
        Ok(out)
    }

    /// The subject store tuning, validated. Without `subjects.min_score` the
    /// floor is the store default, or the detector's
    /// [`DetectorSettings::score_floor`] when that is lower.
    pub fn store_config(&self) -> Result<StoreConfig, ConfigError> {
        let mut c = self.subjects.to_store_config()?;
        if self.subjects.min_score.is_none()
            && let Some(floor) = self.detector.score_floor()
        {
            c.min_score = c.min_score.min(floor);
        }
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
        ("head_half_life", f.head_half_life),
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
