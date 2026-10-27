//! Reading and writing label files as JSON.
//!
//! [`LabelFile::from_json`] reads in three steps, so each kind of mistake
//! is reported in its own terms:
//!
//! 1. **Version.** Only `schema_version` is read at first. A file that
//!    lacks it or states another version is rejected before its shape is
//!    looked at, because another version's shape is not this one's and its
//!    differences would only show up as misleading field errors. Text that
//!    is not JSON at all fails here, located by line and column.
//! 2. **Shape.** The typed parse rejects missing, misspelt and mistyped
//!    fields, giving the field's path (through `serde_path_to_error`)
//!    besides the line and column.
//! 3. **Rules.** [`LabelFile::validate`] reports every broken rule.
//!
//! The first two steps stop at their first error, the third reports all.
//!
//! [`LabelFile::to_json`] writes the canonical form: two-space indents,
//! fields in declaration order, empty truncations and body lines left out
//! and a final newline. Reading a canonical file and writing it again gives
//! back the same bytes, so label files diff cleanly under review.

use serde::Deserialize;

use super::{FieldError, FieldErrors, LabelFile, SCHEMA_VERSION};

/// The first step of reading: the stated version, everything else
/// skipped unparsed.
#[derive(Deserialize)]
struct VersionProbe {
    /// The stated version; `null` counts as missing.
    #[serde(default)]
    schema_version: Option<serde_json::Value>,
}

impl LabelFile {
    /// Reads a label file from JSON text and validates it: first the
    /// schema version, then the shape, then every rule of
    /// [`LabelFile::validate`]. Every error names the field at fault by its
    /// path.
    pub fn from_json(text: &str) -> Result<LabelFile, FieldErrors> {
        check_version(text)?;
        let mut de = serde_json::Deserializer::from_str(text);
        let labels: LabelFile = serde_path_to_error::deserialize(&mut de)
            .map_err(|e| FieldError::new(e.path().to_string(), e.into_inner().to_string()))?;
        labels.validate()?;
        Ok(labels)
    }

    /// The file in canonical JSON: two-space indents, fields in
    /// declaration order, empty truncations and body lines left out and a
    /// final newline, so a canonical file read and written again is
    /// unchanged.
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self)
            .expect("label files have only string keys and plain values");
        text.push('\n');
        text
    }
}

/// Step 1: the text is a JSON object that states [`SCHEMA_VERSION`].
fn check_version(text: &str) -> Result<(), FieldError> {
    let probe: VersionProbe = serde_json::from_str(text)
        .map_err(|e| FieldError::new(".", format!("not a label file: {e}")))?;
    let at = "schema_version";
    let Some(stated) = probe.schema_version else {
        return Err(FieldError::new(
            at,
            format!(
                "missing; a label file states the schema version it follows, and this build \
                 reads version {SCHEMA_VERSION}"
            ),
        ));
    };
    match stated.as_u64() {
        Some(version) if version == u64::from(SCHEMA_VERSION) => Ok(()),
        Some(version) => Err(FieldError::new(
            at,
            format!(
                "version {version} is not supported; this build reads version {SCHEMA_VERSION}"
            ),
        )),
        None => Err(FieldError::new(
            at,
            format!("{stated} is not a version number"),
        )),
    }
}
