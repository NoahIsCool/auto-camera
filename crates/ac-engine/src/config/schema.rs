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
//!    (saving a migrated file is the runtime-edit work, SH1).
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

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// The layout of studio files this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// The version a file without a `schema` key is read at.
const UNVERSIONED: u32 = 1;

/// One migration step: rewrites the top-level table of a document of one
/// version into the next version's layout, or says why it cannot.
type Migration = fn(&mut Map<String, Value>) -> Result<(), String>;

/// The steps from each older version to the next: `MIGRATIONS[0]` takes
/// schema 1 to 2, and so on. Empty while schema 1 is the only version.
const MIGRATIONS: [Migration; SCHEMA_VERSION as usize - 1] = [];

/// The default of the config's `schema` field: what a file without the key
/// is read as.
pub(super) fn unversioned() -> u32 {
    UNVERSIONED
}

/// The only key read before the version is known; every other key is
/// ignored at this stage.
#[derive(Deserialize)]
struct Header {
    /// The raw `schema` value, of any type, so a wrong type gets this
    /// module's message rather than the parser's.
    #[serde(default)]
    schema: Option<Value>,
}

/// Reads `text` (JSON when `json`, else TOML) written at any schema this
/// build knows, returning the value and the warnings for the operator; see
/// the module docs. The error is a sentence for the operator.
pub(super) fn read<T: DeserializeOwned>(
    text: &str,
    json: bool,
) -> Result<(T, Vec<String>), String> {
    read_with(text, json, SCHEMA_VERSION, &MIGRATIONS)
}

/// [`read`] for a build that reads schema `current` and upgrades older
/// files with `steps` (`steps[i]` takes version `i + 1` to `i + 2`), so the
/// tests can try migrations this build does not have yet.
fn read_with<T: DeserializeOwned>(
    text: &str,
    json: bool,
    current: u32,
    steps: &[Migration],
) -> Result<(T, Vec<String>), String> {
    let header: Header = parse(text, json)?;
    let mut warnings = Vec::new();
    let found = match header.schema {
        Some(value) => version(&value, current)?,
        None => {
            warnings.push(missing_key(json));
            UNVERSIONED
        }
    };
    if found == current {
        return parse(text, json).map(|value| (value, warnings));
    }
    let Value::Object(mut doc) = parse::<Value>(text, json)? else {
        return Err("a studio config must be a table of keys".into());
    };
    upgrade(&mut doc, found, current, steps)?;
    warnings.push(format!(
        "this file is schema {found}; it was read as schema {current} (the file itself \
         is unchanged)"
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

/// The version the `schema` value names, if this build reads it.
fn version(value: &Value, current: u32) -> Result<u32, String> {
    let Some(n) = value.as_i64() else {
        return Err(format!(
            "schema must be a whole number, as in `schema = {current}`; this file has {value}"
        ));
    };
    match u32::try_from(n) {
        Ok(v) if (1..=current).contains(&v) => Ok(v),
        Ok(v) if v > current => Err(format!(
            "this file is schema {v}, newer than this build of auto-camera reads \
             (schema {current}); run a newer auto-camera, or rewrite the file for schema \
             {current}"
        )),
        _ => Err(format!(
            "schema {n} is not a config version; versions start at 1 and this build reads \
             schema {current}"
        )),
    }
}

/// The warning for a file without a `schema` key, in its own syntax.
fn missing_key(json: bool) -> String {
    let (key, place) = if json {
        (
            format!("\"schema\": {UNVERSIONED}"),
            "at the top level of the file's object",
        )
    } else {
        (
            format!("schema = {UNVERSIONED}"),
            "as the file's first setting, before any [table]",
        )
    };
    format!(
        "no `schema` key; read as schema {UNVERSIONED}, the layout every file without one \
         has. Add `{key}` {place}, so a later build can tell which layout the file uses"
    )
}

/// Upgrades `doc` from version `from` to `current` one step at a time and
/// stamps the result with `current`.
fn upgrade(
    doc: &mut Map<String, Value>,
    from: u32,
    current: u32,
    steps: &[Migration],
) -> Result<(), String> {
    for v in from..current {
        let step = steps
            .get(v as usize - 1)
            .ok_or_else(|| format!("no migration from schema {v} to {}", v + 1))?;
        step(doc).map_err(|e| format!("cannot upgrade schema {v} to {}: {e}", v + 1))?;
    }
    doc.insert("schema".into(), Value::from(current));
    Ok(())
}

#[cfg(test)]
mod tests;
