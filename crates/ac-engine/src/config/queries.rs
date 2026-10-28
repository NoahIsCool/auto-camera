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
//! [`QuerySettings::MAX_PRIORITY`] (5), higher meaning more important.
//! There is no built-in ranking of roles (speaker above singer above
//! instrument, say): until it is settled which subjects matter most, the
//! config ranks them, in one of two ways ([`Ranking`]):
//!
//! - **List order**, when no query gives a `priority`: the position in the
//!   list is the ranking. The first query has 5 and each later one a level
//!   less, down to 1, which the fifth and later queries share.
//! - **Given**, once any query gives one: each query has its own, and a
//!   query that gives none has [`QuerySettings::DEFAULT_PRIORITY`] (3),
//!   wherever it is listed. Numbering the unranked queries by position
//!   instead would let one listed second (4) outrank a first one the
//!   operator set to 3.
//!
//! A query added by `AddQuery` joins the end of the list: under list order
//! it stands one level below the query listed last (5 when there is none),
//! under given priorities it has the default. A query's priority is fixed
//! when the query is created; removing another query does not renumber it.
//!
//! Only differences count: the engine core gives the director the number
//! of levels a subject's primary query (the one that found it, not its
//! aliases) stands above the lowest priority among the queries that have
//! subjects, and the director adds a bonus per level
//! (`ac_director::candidate`), so a config that gives every query the same
//! priority directs exactly as the director did before it weighed
//! priorities (every level is 0), and so does a config with only one
//! query. A config that gives no priority at all is not such a config: its
//! list order ranks the queries. A priority outside the range is an error
//! ([`QuerySettings::check_priority`]), not clamped: a typo such as
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
    /// [`Self::MIN_PRIORITY`] to [`Self::MAX_PRIORITY`], higher first. When
    /// absent the [`Ranking`] decides; see the module docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
}

impl QuerySettings {
    /// The lowest priority a query may have.
    pub const MIN_PRIORITY: u8 = 1;
    /// The highest priority a query may have.
    pub const MAX_PRIORITY: u8 = 5;
    /// The priority of a query that gives none while others give one, the
    /// middle of the range ([`Ranking::Given`]).
    pub const DEFAULT_PRIORITY: u8 = 3;

    /// A query named `name` for `prompt`, with no explicit presets and no
    /// priority of its own.
    pub fn new(name: &str, prompt: &str) -> Self {
        Self {
            id: None,
            name: name.into(),
            prompt: prompt.into(),
            presets: None,
            literal: false,
            priority: None,
        }
    }

    /// Rejects a given priority outside `MIN_PRIORITY..=MAX_PRIORITY`.
    pub fn check_priority(&self) -> Result<(), String> {
        match self.priority {
            Some(p) if !(Self::MIN_PRIORITY..=Self::MAX_PRIORITY).contains(&p) => Err(format!(
                "query {:?}: priority {p} is outside {}..={}",
                self.name,
                Self::MIN_PRIORITY,
                Self::MAX_PRIORITY
            )),
            _ => Ok(()),
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

/// How the director priority of a query that gives none is found; see the
/// module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ranking {
    /// No query of the config gives a priority: the list order ranks them.
    ListOrder,
    /// Some query gives one: a query that gives none has the default.
    Given,
}

impl Ranking {
    /// How `queries` are ranked: by list order unless one gives a priority.
    pub fn of(queries: &[QuerySettings]) -> Self {
        if queries.iter().any(|q| q.priority.is_some()) {
            Self::Given
        } else {
            Self::ListOrder
        }
    }

    /// The priority of each of `queries`, in order: its own, or, when it
    /// gives none, that of a query listed after the ones before it
    /// ([`Self::appended`]). Out-of-range values are returned as given;
    /// [`QuerySettings::check_priority`] rejects them.
    pub fn priorities(self, queries: &[QuerySettings]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(queries.len());
        for q in queries {
            let p = q
                .priority
                .unwrap_or_else(|| self.appended(out.last().copied()));
            out.push(p);
        }
        out
    }

    /// The priority of a query that gives none, listed after a query of
    /// priority `last` (`None` when it is listed first): under list order
    /// one level below `last`, down to the minimum, or the maximum when it
    /// is first; under given priorities the default.
    pub fn appended(self, last: Option<u8>) -> u8 {
        match self {
            Self::ListOrder => last.map_or(QuerySettings::MAX_PRIORITY, |p| {
                p.saturating_sub(1).max(QuerySettings::MIN_PRIORITY)
            }),
            Self::Given => QuerySettings::DEFAULT_PRIORITY,
        }
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

    /// A `[[queries]]` entry named `name`, with `priority` when given.
    fn entry(name: &str, priority: Option<u8>) -> String {
        let priority = priority.map_or(String::new(), |p| format!("priority = {p}\n"));
        format!("[[queries]]\nname = \"{name}\"\nprompt = \"person\"\n{priority}")
    }

    /// The studio of `entries`, validated, and the priority of each query.
    fn ranked(entries: &[(&str, Option<u8>)]) -> (StudioConfig, Vec<u8>) {
        let text: String = entries.iter().map(|(n, p)| entry(n, *p)).collect();
        let c = studio(&text).unwrap();
        c.validate().unwrap();
        let priorities = Ranking::of(&c.queries).priorities(&c.queries);
        (c, priorities)
    }

    #[test]
    fn without_priorities_the_list_order_ranks_the_queries() {
        let names = ["Pastor", "Leader", "Band", "Choir", "Kids", "Table"];
        let entries: Vec<(&str, Option<u8>)> = names.iter().map(|n| (*n, None)).collect();
        let (c, priorities) = ranked(&entries);
        assert_eq!(Ranking::of(&c.queries), Ranking::ListOrder);
        // First highest, a level less each, the fifth and later share 1.
        assert_eq!(priorities, [5, 4, 3, 2, 1, 1]);
        assert_eq!(QuerySettings::new("Band", "person").priority, None);
        // Nothing is written back that the file did not say.
        let text = toml::to_string(&c).unwrap();
        assert!(!text.contains("priority"), "{text}");
        assert_eq!(StudioConfig::parse(&text, false).unwrap(), c);
        // One query, or none, is ranked too.
        assert_eq!(ranked(&[("Pastor", None)]).1, [5]);
        assert_eq!(Ranking::of(&[]), Ranking::ListOrder);
        assert!(Ranking::ListOrder.priorities(&[]).is_empty());
    }

    #[test]
    fn once_any_query_gives_a_priority_the_others_have_the_default() {
        let (c, priorities) = ranked(&[
            ("Pastor", Some(5)),
            ("Band", None),
            ("Choir", Some(2)),
            ("Kids", None),
        ]);
        assert_eq!(Ranking::of(&c.queries), Ranking::Given);
        assert_eq!(priorities, [5, 3, 2, 3]);
        // An unranked query never climbs above a ranked one by its place.
        assert_eq!(ranked(&[("Pastor", Some(3)), ("Band", None)]).1, [3, 3]);
        assert_eq!(ranked(&[("Band", None), ("Pastor", Some(1))]).1, [3, 1]);
        // Only the priorities the file gave are written back.
        let text = toml::to_string(&c).unwrap();
        assert_eq!(text.matches("priority").count(), 2, "{text}");
        assert!(text.contains("priority = 5") && text.contains("priority = 2"));
        assert_eq!(StudioConfig::parse(&text, false).unwrap(), c);
    }

    #[test]
    fn a_query_appended_ranks_as_if_listed_last() {
        let list = Ranking::ListOrder;
        assert_eq!(list.appended(None), QuerySettings::MAX_PRIORITY);
        assert_eq!(list.appended(Some(4)), 3);
        assert_eq!(list.appended(Some(1)), QuerySettings::MIN_PRIORITY);
        // Out-of-range input (refused by the config) still lands in range.
        assert_eq!(list.appended(Some(0)), QuerySettings::MIN_PRIORITY);
        assert_eq!(list.appended(Some(9)), 8);
        for last in [None, Some(1), Some(5)] {
            assert_eq!(
                Ranking::Given.appended(last),
                QuerySettings::DEFAULT_PRIORITY
            );
        }
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
            priority: Some(0),
            // An empty prompt is an error too; the priority is checked first.
            ..QuerySettings::new("Pastor", "")
        };
        let err = query.build(&reader).unwrap_err();
        assert!(err.contains("priority 0"), "{err}");
        let fine = QuerySettings {
            priority: Some(QuerySettings::MAX_PRIORITY),
            ..QuerySettings::new("Pastor", "person")
        };
        assert_eq!(fine.build(&reader).unwrap().id, QueryId("pastor".into()));
    }
}
