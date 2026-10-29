//! The studio configuration: everything needed to start an engine.
//!
//! A [`StudioConfig`] is plain data, loaded from TOML or JSON
//! ([`StudioConfig::load`]) and checked once by [`StudioConfig::validate`]
//! before an engine starts, so the engine never sees an inconsistent setup.
//! Every section has defaults, so a minimal file only says which layout it
//! is written in and lists its sources:
//!
//! ```toml
//! schema = 1
//!
//! [[sources]]
//! id = "stage"
//! kind = "synthetic"
//! width = 1280
//! height = 720
//! fps = 30
//! ```
//!
//! The top-level `schema` key names the file's layout, [`SCHEMA_VERSION`]
//! in this build. A file without it (every file written before the key
//! existed) is read as schema 1 with a warning to add it
//! ([`StudioConfig::load_with_warnings`]); an unknown or newer version is
//! an error, and an older one is upgraded by the migration steps in
//! `schema` before it is deserialized.
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
//! ([`DetectorSettings::sees_actions`]); see [`crate::queries`]. A query's
//! `priority`, 1 to 5, ranks its subjects for the director; when no query
//! gives one, the list order ranks them, first highest ([`Ranking`]).
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
//! file; the output's two, `output.ffmpeg` and `output.record_path`, are
//! taken as given, from the working folder (see [`OutputSettings`]).
//!
//! # Layout
//!
//! Each table has a private module holding its type, its defaults and its
//! own checks (`lexicon`, whose type is `ac_core`'s, is applied here), and
//! every type is re-exported here, so callers name
//! `crate::config::OutputSettings` whatever file it lives in:
//!
//! | Table | Type | Module |
//! |---|---|---|
//! | `schema` (a key) | `u32`, [`SCHEMA_VERSION`], and its migrations | `schema` |
//! | `sources` | [`SourceEntry`] | `sources` |
//! | `queries` | [`QuerySettings`], [`PresetSpec`] | `queries` |
//! | `lexicon` | [`Lexicon`], applied by [`StudioConfig::prompt_lexicon`] | here |
//! | `detector` | [`DetectorSettings`], [`SidecarSettings`] | `detector`, `sidecar` |
//! | `discovery` | [`DiscoverySettings`] | `discovery` |
//! | `tracker`, `framing` | partial tables over `ac_vision` and `ac_framing` | `partial` |
//! | `subjects` | [`SubjectSettings`] | `subjects` |
//! | `director` | [`DirectorSettings`] | `director` |
//! | `switcher` | [`SwitcherSettings`] | `switcher` |
//! | `output` | [`OutputSettings`], [`EncoderSetting`] | `output` |
//! | `monitors` | [`MonitorSettings`] | `monitors` |
//!
//! This module keeps the root table, [`StudioConfig`]: loading, path
//! resolution and [`StudioConfig::problems`], which runs every table's check
//! and the checks that span tables (a synthetic detector needs synthetic
//! sources, the sidecar's request deadline must be below
//! `discovery.timeout_s`, `output.audio_source` must name a source) and
//! lists every problem found, for the readiness check
//! ([`crate::readiness`]); [`StudioConfig::validate`] fails on the first. It
//! also keeps the helpers used beyond one table: [`valid_id`] for every id
//! and [`preset_for`] for bare shot sizes and the engine's default presets.
//!
//! The flat tables (`discovery`, `subjects`, `director`, `switcher`,
//! `output` and `monitors`) have a default for every key
//! (`#[serde(default)]`), and every table rejects unknown keys, so a typo in
//! a config file is an error rather than a silently ignored setting.
//! Durations are written as seconds (`*_s` keys) and converted, with range
//! checks, when the engine builds its components.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ac_core::{Lexicon, Preset, Query, ShotSize};
use ac_director::DirectorConfig;
use ac_framing::FramerConfig;
use ac_media::MediaError;
use ac_sources::segment::Segment;
use ac_sources::{FrameRate, SourceConfig, SyntheticPattern};
use ac_subjects::StoreConfig;
use ac_vision::TemplateTrackerConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::queries::PromptReader;

mod detector;
mod director;
mod discovery;
mod monitors;
mod output;
mod partial;
mod queries;
mod schema;
mod sidecar;
mod sources;
mod subjects;
mod switcher;

pub use detector::DetectorSettings;
pub use director::DirectorSettings;
pub use discovery::DiscoverySettings;
pub use monitors::MonitorSettings;
pub use output::{EncoderSetting, OutputSettings};
pub use queries::{PresetSpec, QuerySettings, Ranking, slug, validate_query};
pub use schema::SCHEMA_VERSION;
pub use sidecar::SidecarSettings;
pub use sources::SourceEntry;
pub use subjects::SubjectSettings;
pub use switcher::SwitcherSettings;

use partial::{partial, validate_framing};

// The tests (`config/tests.rs`) name `QueryId` through `use super::*`; it
// was in scope here before the `queries` table moved to its own module.
#[cfg(test)]
use ac_core::QueryId;

/// A config read from a file, with what the operator should be told about
/// it.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    /// The config, its relative paths resolved.
    pub config: StudioConfig,
    /// Things to fix that did not stop the load (a missing `schema` key),
    /// each a sentence starting with the file's path.
    pub warnings: Vec<String>,
}

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

/// Everything needed to start an engine; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudioConfig {
    /// The layout the config is written in: [`SCHEMA_VERSION`] once read
    /// (a file without the key is read as schema 1; see `schema`). First,
    /// so a TOML file written from a config starts with it.
    #[serde(default = "schema::unversioned")]
    pub schema: u32,
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
    /// Auto-director mode, profile and the operator's side (takeover,
    /// pause and skip windows).
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
            schema: SCHEMA_VERSION,
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
    /// anything else as TOML), resolving relative paths against its
    /// directory. The warnings are dropped; a program the operator runs
    /// uses [`Self::load_with_warnings`] instead.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::load_with_warnings(path).map(|loaded| loaded.config)
    }

    /// [`Self::load`], keeping the warnings for the operator.
    pub fn load_with_warnings(path: &Path) -> Result<LoadedConfig, ConfigError> {
        let loaded = Self::read_file(path)?;
        loaded.config.validate()?;
        Ok(loaded)
    }

    /// Reads and parses the file at `path` and resolves its relative paths,
    /// without validating the values, so that the readiness check can list
    /// every problem ([`Self::problems`]) rather than the first.
    pub fn read_file(path: &Path) -> Result<LoadedConfig, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.into(),
            source,
        })?;
        let json = path.extension().is_some_and(|e| e == "json");
        let (mut config, warnings) =
            Self::parse_with_warnings(&text, json).map_err(|message| ConfigError::Parse {
                path: path.into(),
                message,
            })?;
        if let Some(dir) = path.parent() {
            config.resolve_paths(dir);
        }
        let warnings = warnings
            .into_iter()
            .map(|w| format!("{}: {w}", path.display()))
            .collect();
        Ok(LoadedConfig { config, warnings })
    }

    /// Parses `text` as JSON or TOML without validating it; the warnings
    /// are dropped (see [`Self::parse_with_warnings`]).
    pub fn parse(text: &str, json: bool) -> Result<Self, String> {
        Self::parse_with_warnings(text, json).map(|(config, _)| config)
    }

    /// Parses `text` as JSON or TOML at any schema this build reads,
    /// without validating it, with the warnings for the operator (a missing
    /// `schema` key, an upgraded older schema); an unknown or newer schema
    /// is an error. See the module docs.
    pub fn parse_with_warnings(text: &str, json: bool) -> Result<(Self, Vec<String>), String> {
        schema::read(text, json)
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

    /// Checks the whole config, returning the first problem found (the
    /// first of [`Self::problems`]).
    pub fn validate(&self) -> Result<(), ConfigError> {
        match self.problems().into_iter().next() {
            Some(problem) => Err(ConfigError::Invalid(problem)),
            None => Ok(()),
        }
    }

    /// Every problem in the config, in the order checked: the schema, each
    /// source (its id, and the values its input's opener would refuse
    /// before looking at any file), each query, then each table and each
    /// check that spans tables. Empty when the config is valid. The tables
    /// checked here list every bad key; those whose checks belong to the
    /// crate that uses them (`lexicon`, `tracker`, `subjects`, `director`
    /// and `framing`) give the first problem of each. Pure: it reads no file
    /// (whether the files it names exist is the readiness check's question,
    /// [`crate::readiness`]).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.schema != SCHEMA_VERSION {
            out.push(format!(
                "schema {} is not this build's schema {SCHEMA_VERSION}",
                self.schema
            ));
        }
        if self.sources.is_empty() {
            out.push("at least one source is required".into());
        }
        let mut ids = HashSet::new();
        for s in &self.sources {
            if !valid_id(&s.id) {
                out.push(format!(
                    "source id {:?} must be non-empty letters, digits, '-' or '_'",
                    s.id
                ));
            }
            if !ids.insert(s.id.as_str()) {
                out.push(format!("duplicate source id {:?}", s.id));
            }
            // A source that fails to open starts offline and is retried, so
            // a mistake no retry can mend is caught here instead.
            for p in input_problems(&s.input) {
                out.push(format!("source {:?}: {p}", s.id));
            }
            if matches!(self.detector, DetectorSettings::Synthetic { .. })
                && !matches!(s.input, SourceConfig::Synthetic { .. })
            {
                out.push(format!(
                    "detector kind \"synthetic\" only works with synthetic sources; source {:?} is not one",
                    s.id
                ));
            }
        }
        out.extend(
            self.lexicon
                .validate()
                .err()
                .map(|e| format!("lexicon: {e}")),
        );
        out.extend(self.each_query().into_iter().filter_map(Result::err));
        out.extend(self.detector.problems());
        out.extend(self.discovery.problems());
        if let DetectorSettings::Sidecar(sidecar) = &self.detector
            && sidecar.request_timeout_s >= self.discovery.timeout_s
        {
            out.push(format!(
                "detector.request_timeout_s ({} s) must be below discovery.timeout_s ({} s), \
                 after which the engine abandons a request",
                sidecar.request_timeout_s, self.discovery.timeout_s
            ));
        }
        out.extend(
            self.tracker
                .validate()
                .err()
                .map(|e| format!("tracker: {e}")),
        );
        out.extend(self.store_config().err().map(invalid_message));
        out.extend(self.director_config().err().map(invalid_message));
        out.extend(validate_framing(&self.framing).err());
        out.extend(self.switcher.validate().err());
        out.extend(self.output.problems());
        if let Some(audio) = &self.output.audio_source
            && !ids.contains(audio.as_str())
        {
            out.push(format!("output.audio_source {audio:?} is not a source id"));
        }
        out.extend(self.monitors.problems());
        out
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
        self.each_query()
            .into_iter()
            .collect::<Result<_, _>>()
            .map_err(ConfigError::Invalid)
    }

    /// Each query in config order, built as [`Self::build_queries`] builds
    /// it, or why it cannot be: an invalid id, a prompt or preset it
    /// refuses, or an id an earlier query has.
    fn each_query(&self) -> Vec<Result<Query, String>> {
        let reader = PromptReader::of(self);
        let mut ids = HashSet::new();
        self.queries
            .iter()
            .map(|q| {
                let id = q.query_id();
                if !valid_id(&id.0) {
                    return Err(format!("query id {:?} is not a valid id", id.0));
                }
                let query = q.build(&reader)?;
                if !ids.insert(id.0.clone()) {
                    return Err(format!("duplicate query id {:?}", id.0));
                }
                Ok(query)
            })
            .collect()
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
        let c = self.director.to_director_config()?;
        c.validate()
            .map_err(|e| ConfigError::Invalid(format!("director: {e}")))?;
        Ok(c)
    }

    /// The program frame interval.
    pub fn frame_period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.output.fps.max(1)))
    }
}

/// The message of a [`ConfigError::Invalid`]; any other error (which the
/// value checks do not return) as displayed.
fn invalid_message(e: ConfigError) -> String {
    match e {
        ConfigError::Invalid(message) => message,
        other => other.to_string(),
    }
}

/// What the opener of `input` would refuse before looking at any file: a
/// synthetic pattern's size, rate and subject count, against the limits
/// [`SyntheticPattern::new`] applies, and a file's `max_width`, by
/// [`ac_sources::output_size`] itself, and its segment, by
/// [`Segment::from_seconds`]. So a load and `--check` catch them, not only
/// a start. Whether the file is there is the readiness check's question.
fn input_problems(input: &SourceConfig) -> Vec<String> {
    let mut out = Vec::new();
    let why = |e: MediaError| match e {
        MediaError::Open(why) => why,
        other => other.to_string(),
    };
    match input {
        SourceConfig::File {
            max_width,
            start_s,
            duration_s,
            ..
        } => {
            // Any picture will do: only `max_width` is in question.
            if let Err(e) = ac_sources::output_size((2, 2), *max_width) {
                out.push(why(e));
            }
            if let Err(e) = Segment::from_seconds(*start_s, *duration_s) {
                out.push(why(e));
            }
        }
        SourceConfig::Synthetic {
            width,
            height,
            fps,
            subjects,
        } => {
            let side = 1..=SyntheticPattern::MAX_SIDE;
            if !side.contains(width) || !side.contains(height) {
                out.push(format!(
                    "synthetic size {width}x{height} is outside 1..={}",
                    SyntheticPattern::MAX_SIDE
                ));
            }
            if FrameRate::from_f64(*fps).is_none() {
                out.push(format!(
                    "synthetic fps {fps} is not a usable frame rate (more than 0, at most {})",
                    FrameRate::MAX_FPS
                ));
            }
            if *subjects > SyntheticPattern::MAX_SUBJECTS {
                out.push(format!(
                    "{subjects} synthetic subjects, at most {} allowed",
                    SyntheticPattern::MAX_SUBJECTS
                ));
            }
        }
    }
    out
}

/// True for non-empty ids of ASCII letters, digits, `-` and `_`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests;
