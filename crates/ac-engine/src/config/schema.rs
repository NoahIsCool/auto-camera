//! The `schema` key: which layout a studio file is written in, and the
//! migrations that bring an older layout up to this build's.
//!
//! # Theory
//!
//! A studio file outlives the build that wrote it, so every file says which
//! layout ("schema") its keys follow with a top-level `schema = N` (in JSON,
//! `"schema": N`), and this build reads [`SCHEMA_VERSION`]. Reading a file
//! ([`read`]) goes:
//!
//! 1. Read only the `schema` key, ignoring every other key, so that a file
//!    in an older layout, which this build's types would reject, still says
//!    what it is.
//! 2. A version this build does not know is an error naming both versions:
//!    0 or less, not a whole number, or newer than [`SCHEMA_VERSION`] (a
//!    file from a newer build must not be half-read by an older one).
//! 3. A file at this build's version is deserialized straight from its
//!    text, so a parse error keeps its line and column.
//! 4. An older file is parsed into an untyped document, upgraded one
//!    version at a time by the migration steps (step `i` takes version
//!    `i + 1` to `i + 2`), stamped with [`SCHEMA_VERSION`] and deserialized
//!    from the document, with a warning that the file itself is unchanged
//!    (the program never rewrites a studio file; Save show writes a show
//!    file, in the show's own current layout, [`crate::show`]).
//!
//! A file without the key predates it: every such file was written in the
//! layout schema 1 describes, so it is read as schema 1 with a warning to
//! add `schema = 1`. A show must not fail to start over a missing version
//! line; an unknown version, which may mean anything, is an error.
//!
//! # Invariants
//!
//! * There is one migration step per version below [`SCHEMA_VERSION`], so
//!   every version from 1 up has a path to the current one (checked at
//!   compile time). Schema 1 is the only version so far, so there are none.
//! * A step only rewrites the document: migrating reads no file and is
//!   deterministic.
//! * A read that succeeds yields a value whose `schema` is
//!   [`SCHEMA_VERSION`] (when the key is missing, the field's default,
//!   [`UNVERSIONED`], which is 1, the only version a file without it can
//!   be read at directly).
//!
//! # Other layouts
//!
//! The steps above do not depend on the studio file: a [`Layout`] names
//! the key that holds the version, the words the messages use, the
//! current version, its migration steps and whether a document without
//! the key is read at a version or refused. [`STUDIO`] is the studio
//! file's; a show file ([`crate::show`]) has its own, keyed `show`, which
//! it requires, and is read by the same [`read_layout`], so both kinds of
//! file are versioned and migrated the same way.

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// The layout of studio files this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// The version a file without a `schema` key is read at.
const UNVERSIONED: u32 = 1;

/// One migration step: rewrites the top-level table of a document of one
/// version into the next version's layout, or says why it cannot.
pub(crate) type Migration = fn(&mut Map<String, Value>) -> Result<(), String>;

/// The steps from each older version to the next: `MIGRATIONS[0]` takes
/// schema 1 to 2, and so on. Empty while schema 1 is the only version.
const MIGRATIONS: [Migration; SCHEMA_VERSION as usize - 1] = [];

/// A versioned document layout; see the module docs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Layout<'a> {
    /// The top-level key holding the version (`schema`).
    pub(crate) key: &'static str,
    /// What a version is called in messages: "schema", as in "this file
    /// is schema 2".
    pub(crate) label: &'static str,
    /// What a document of the layout is called in messages: "config", as
    /// in "schema 0 is not a config version".
    pub(crate) noun: &'static str,
    /// The version this build reads and writes.
    pub(crate) current: u32,
    /// `steps[i]` takes version `i + 1` to `i + 2`.
    pub(crate) steps: &'a [Migration],
    /// The version a document without the key is read at, with a warning
    /// to add it; `None` when the key is required.
    pub(crate) unversioned: Option<u32>,
}

/// The studio file's layout.
const STUDIO: Layout<'static> = Layout {
    key: "schema",
    label: "schema",
    noun: "config",
    current: SCHEMA_VERSION,
    steps: &MIGRATIONS,
    unversioned: Some(UNVERSIONED),
};

/// The default of the config's `schema` field: what a file without the key
/// is read as.
pub(super) fn unversioned() -> u32 {
    UNVERSIONED
}

/// Reads `text` (JSON when `json`, else TOML) written at any schema this
/// build knows, returning the value and the warnings for the operator; see
/// the module docs. The error is a sentence for the operator.
pub(super) fn read<T: DeserializeOwned>(
    text: &str,
    json: bool,
) -> Result<(T, Vec<String>), String> {
    read_layout(text, json, &STUDIO)
}

/// [`read`] for a build that reads schema `current` and upgrades older
/// files with `steps` (`steps[i]` takes version `i + 1` to `i + 2`), so the
/// tests can try migrations this build does not have yet.
#[cfg(test)]
fn read_with<T: DeserializeOwned>(
    text: &str,
    json: bool,
    current: u32,
    steps: &[Migration],
) -> Result<(T, Vec<String>), String> {
    read_layout(
        text,
        json,
        &Layout {
            current,
            steps,
            ..STUDIO
        },
    )
}

/// Reads `text` (JSON when `json`, else TOML) written at any version of
/// `layout` this build knows, returning the value and the warnings for the
/// operator; see the module docs. The error is a sentence for the operator.
pub(crate) fn read_layout<T: DeserializeOwned>(
    text: &str,
    json: bool,
    layout: &Layout<'_>,
) -> Result<(T, Vec<String>), String> {
    // Only the version key is looked at before the version is known; its
    // value is taken whatever its type, so a wrong one gets this module's
    // message rather than the parser's.
    let header: Value = parse(text, json)?;
    let mut warnings = Vec::new();
    let found = match (header.get(layout.key), layout.unversioned) {
        (Some(value), _) => version(value, layout)?,
        (None, Some(unversioned)) => {
            warnings.push(missing_key(json, layout, unversioned));
            unversioned
        }
        (None, None) => return Err(required_key(layout)),
    };
    if found == layout.current {
        return parse(text, json).map(|value| (value, warnings));
    }
    let Value::Object(mut doc) = header else {
        return Err(format!("a {} file must be a table of keys", layout.noun));
    };
    upgrade(&mut doc, found, layout)?;
    warnings.push(format!(
        "this file is {label} {found}; it was read as {label} {current} (the file itself \
         is unchanged)",
        label = layout.label,
        current = layout.current
    ));
    let value = serde_json::from_value(Value::Object(doc)).map_err(|e| e.to_string())?;
    Ok((value, warnings))
}

/// Parses `text` as JSON or TOML into `T`.
fn parse<T: DeserializeOwned>(text: &str, json: bool) -> Result<T, String> {
    if json {
        serde_json::from_str(text).map_err(|e| e.to_string())
    } else {
        toml::from_str(text).map_err(|e| e.to_string())
    }
}

/// The version the version key's `value` names, if this build reads it.
fn version(value: &Value, layout: &Layout<'_>) -> Result<u32, String> {
    let Layout {
        key,
        label,
        noun,
        current,
        ..
    } = *layout;
    let Some(n) = value.as_i64() else {
        return Err(format!(
            "{key} must be a whole number, as in `{key} = {current}`; this file has {value}"
        ));
    };
    match u32::try_from(n) {
        Ok(v) if (1..=current).contains(&v) => Ok(v),
        Ok(v) if v > current => Err(format!(
            "this file is {label} {v}, newer than this build of auto-camera reads \
             ({label} {current}); run a newer auto-camera, or rewrite the file for {label} \
             {current}"
        )),
        _ => Err(format!(
            "{label} {n} is not a {noun} version; versions start at 1 and this build reads \
             {label} {current}"
        )),
    }
}

/// The warning for a file without the version key, read at `unversioned`,
/// in its own syntax.
fn missing_key(json: bool, layout: &Layout<'_>, unversioned: u32) -> String {
    let Layout { key, label, .. } = *layout;
    let (line, place) = if json {
        (
            format!("\"{key}\": {unversioned}"),
            "at the top level of the file's object",
        )
    } else {
        (
            format!("{key} = {unversioned}"),
            "as the file's first setting, before any [table]",
        )
    };
    format!(
        "no `{key}` key; read as {label} {unversioned}, the layout every file without one \
         has. Add `{line}` {place}, so a later build can tell which layout the file uses"
    )
}

/// The error for a file without the version key of a layout that
/// requires it.
fn required_key(layout: &Layout<'_>) -> String {
    let Layout {
        key,
        label,
        noun,
        current,
        ..
    } = *layout;
    format!("no `{key}` key; a {noun} file gives its {label}, as in `{key} = {current}`")
}

/// Upgrades `doc` from version `from` to the layout's current one, one
/// step at a time, and stamps the result with it.
fn upgrade(doc: &mut Map<String, Value>, from: u32, layout: &Layout<'_>) -> Result<(), String> {
    let label = layout.label;
    for v in from..layout.current {
        let step = layout
            .steps
            .get(v as usize - 1)
            .ok_or_else(|| format!("no migration from {label} {v} to {}", v + 1))?;
        step(doc).map_err(|e| format!("cannot upgrade {label} {v} to {}: {e}", v + 1))?;
    }
    doc.insert(layout.key.into(), Value::from(layout.current));
    Ok(())
}

#[cfg(test)]
mod tests;
