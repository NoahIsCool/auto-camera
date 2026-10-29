//! The `[[queries]]` table: text prompts and the framings applied to every
//! subject they match.
//!
//! A query stays plain data until the engine is built: its prompt is read
//! once, by [`QuerySettings::build`] with the studio's [`PromptReader`] (see
//! [`crate::queries`]). Presets are written either as a bare shot size,
//! which [`preset_for`] completes, or in full ([`PresetSpec`]); explicit
//! presets always win over the prompt's shot words. A query without an `id`
//! is known by a [`slug`] of its name.
//!
//! [`validate_query`] is the one check of a query's name, prompt and
//! presets. A query from the config file and one added by the operator's
//! `AddQuery` command both pass it (through [`crate::queries::build_query`]),
//! so a query added live meets the same rules as a loaded one.

use std::collections::HashSet;

use ac_core::{Preset, Query, QueryId, ShotSize};
use serde::{Deserialize, Serialize};

use super::{preset_for, valid_id};
use crate::queries::PromptReader;

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
