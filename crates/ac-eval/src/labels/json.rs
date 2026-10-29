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
//! Unknown fields are errors everywhere in the file, inside boxes and
//! polygon vertices too. Those are [`ac_core`]'s [`Rect`] and [`Point`],
//! whose own serde forms ignore unknown keys, so the label types read and
//! write them through the strict mirrors [`StrictRect`] and
//! [`strict_polygon`], which have the same fields.
//!
//! [`LabelFile::to_json`] writes the canonical form: two-space indents,
//! fields in declaration order, empty truncations and body lines left out
//! and a final newline. Reading a canonical file and writing it again gives
//! back the same bytes, so label files diff cleanly under review. That
//! holds for every coordinate because numbers are written with the fewest
//! digits that read back as the same `f64`, and read exactly (serde_json's
//! `float_roundtrip`), not to within one unit in the last place.

use ac_core::{Point, Rect};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

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

/// The serde form of a box in a label file: [`Rect`]'s fields, unknown
/// keys rejected.
#[derive(Serialize, Deserialize)]
#[serde(remote = "Rect", deny_unknown_fields)]
pub(super) struct StrictRect {
    /// Left edge.
    x: f64,
    /// Top edge.
    y: f64,
    /// Width.
    w: f64,
    /// Height.
    h: f64,
}

/// The serde form of a polygon vertex in a label file: [`Point`]'s
/// fields, unknown keys rejected.
#[derive(Serialize, Deserialize)]
#[serde(remote = "Point", deny_unknown_fields)]
struct StrictPoint {
    /// Horizontal position.
    x: f64,
    /// Vertical position.
    y: f64,
}

/// One vertex of a polygon, read and written as a [`StrictPoint`].
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Vertex(#[serde(with = "StrictPoint")] Point);

/// The serde form of a polygon in a label file: a list of
/// [`StrictPoint`]s.
pub(super) mod strict_polygon {
    use super::*;

    /// Writes the vertices in order.
    pub(in crate::labels) fn serialize<S: Serializer>(
        polygon: &[Point],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(polygon.iter().map(|&point| Vertex(point)))
    }

    /// Reads the vertices in order.
    pub(in crate::labels) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Point>, D::Error> {
        let vertices = Vec::<Vertex>::deserialize(deserializer)?;
        Ok(vertices.into_iter().map(|Vertex(point)| point).collect())
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
