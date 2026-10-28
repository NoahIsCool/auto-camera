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
//!
//! # Priority
//!
//! A query's `priority` ranks its prompt for the director (spec C.1, "role
//! priority"): an integer from [`QuerySettings::MIN_PRIORITY`] (1) to
//! [`QuerySettings::MAX_PRIORITY`] (5), higher meaning more important, and
//! [`QuerySettings::DEFAULT_PRIORITY`] (3) when not given, so one query can
//! be raised or lowered without touching the others. Only differences count:
//! the engine core gives the director the number of levels a subject's
//! query stands above the lowest-priority live query, and the director adds
//! a bonus per level (`ac_director::candidate`), so a config that gives
//! every query the same priority, or none, directs exactly as one without
//! priorities. There is no built-in ranking of roles (speaker
//! above singer above instrument, say): until it is settled which subjects
//! matter most, the order is the one the config gives. A query added by
//! `AddQuery` has the default priority. A priority outside the range is an
//! error ([`QuerySettings::check_priority`]), not clamped: a typo such as
//! `priority = 50` would otherwise quietly pin one prompt on air.

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
    /// How much the director prefers this query's subjects, from
    /// [`Self::MIN_PRIORITY`] to [`Self::MAX_PRIORITY`], higher first; see
    /// the module docs.
    #[serde(
        default = "default_priority",
        skip_serializing_if = "is_default_priority"
    )]
    pub priority: u8,
}

impl QuerySettings {
    /// The lowest priority a query may have.
    pub const MIN_PRIORITY: u8 = 1;
    /// The highest priority a query may have.
    pub const MAX_PRIORITY: u8 = 5;
    /// The priority of a query that gives none, the middle of the range.
    pub const DEFAULT_PRIORITY: u8 = 3;

    /// A query named `name` for `prompt`, with no explicit presets and the
    /// default priority.
    pub fn new(name: &str, prompt: &str) -> Self {
        Self {
            id: None,
            name: name.into(),
            prompt: prompt.into(),
            presets: None,
            literal: false,
            priority: Self::DEFAULT_PRIORITY,
        }
    }

    /// Rejects a priority outside `MIN_PRIORITY..=MAX_PRIORITY`.
    pub fn check_priority(&self) -> Result<(), String> {
        if (Self::MIN_PRIORITY..=Self::MAX_PRIORITY).contains(&self.priority) {
            Ok(())
        } else {
            Err(format!(
                "query {:?}: priority {} is outside {}..={}",
                self.name,
                self.priority,
                Self::MIN_PRIORITY,
                Self::MAX_PRIORITY
            ))
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

    /// The query, its prompt read with `reader` (see [`crate::queries`]),
    /// once its priority is in range.
    pub fn build(&self, reader: &PromptReader) -> Result<Query, String> {
        self.check_priority()?;
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

/// Serde default of [`QuerySettings::priority`].
fn default_priority() -> u8 {
    QuerySettings::DEFAULT_PRIORITY
}

/// True for the default priority, which is left out when serializing.
fn is_default_priority(priority: &u8) -> bool {
    *priority == QuerySettings::DEFAULT_PRIORITY
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StudioConfig;

    /// A studio with one synthetic source and the given `[[queries]]` text.
    fn studio(queries: &str) -> Result<StudioConfig, String> {
        StudioConfig::parse(
            &format!(
                "[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\n\
                 width = 640\nheight = 360\nfps = 30\n{queries}"
            ),
            false,
        )
    }

    /// One query named "Pastor" with `priority = {value}`.
    fn pastor(value: &str) -> Result<StudioConfig, String> {
        studio(&format!(
            "[[queries]]\nname = \"Pastor\"\nprompt = \"person\"\npriority = {value}\n"
        ))
    }

    #[test]
    fn priority_defaults_to_the_middle_of_its_range() {
        let c = studio(
            "[[queries]]\nname = \"Pastor\"\nprompt = \"person\"\npriority = 5\n\
             [[queries]]\nname = \"Band\"\nprompt = \"person\"\n",
        )
        .unwrap();
        c.validate().unwrap();
        let priorities: Vec<u8> = c.queries.iter().map(|q| q.priority).collect();
        assert_eq!(priorities, [5, QuerySettings::DEFAULT_PRIORITY]);
        assert_eq!(QuerySettings::new("Band", "person").priority, 3);
        // Only a priority that differs from the default is written back.
        let text = toml::to_string(&c).unwrap();
        assert_eq!(text.matches("priority").count(), 1, "{text}");
        assert!(text.contains("priority = 5"), "{text}");
        assert_eq!(StudioConfig::parse(&text, false).unwrap(), c);
    }

    #[test]
    fn config_rejects_a_priority_outside_its_range() {
        for value in ["1", "2", "4", "5"] {
            pastor(value).unwrap().validate().unwrap();
        }
        for value in ["0", "6", "255"] {
            let err = pastor(value).unwrap().validate().unwrap_err().to_string();
            assert!(
                err.contains(&format!("\"Pastor\": priority {value} is outside 1..=5")),
                "{err}"
            );
        }
        // Not a small whole number: refused when the file is read.
        for value in ["-1", "256", "2.5", "\"high\""] {
            assert!(pastor(value).is_err(), "{value}");
        }
    }

    #[test]
    fn an_out_of_range_priority_is_reported_before_the_prompt_is_read() {
        let reader = PromptReader::of(&studio("").unwrap());
        let query = QuerySettings {
            priority: 0,
            // An empty prompt is an error too; the priority is checked first.
            ..QuerySettings::new("Pastor", "")
        };
        let err = query.build(&reader).unwrap_err();
        assert!(err.contains("priority 0"), "{err}");
        let fine = QuerySettings {
            priority: QuerySettings::MAX_PRIORITY,
            ..QuerySettings::new("Pastor", "person")
        };
        assert_eq!(fine.build(&reader).unwrap().id, QueryId("pastor".into()));
    }
}
