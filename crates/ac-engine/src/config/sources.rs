//! The `[[sources]]` table: the studio's video inputs.
//!
//! An entry is a [`SourceConfig`] (`kind = "file"` or `"synthetic"`, its
//! keys flattened into the entry) plus a stable id and an optional
//! operator-facing name. The id is part of every shot id of the source, so
//! [`StudioConfig::validate`](super::StudioConfig::validate) requires each
//! to pass [`valid_id`](super::valid_id) and to be unique; a source without
//! a name is shown by its id.

use ac_core::SourceId;
use ac_sources::SourceConfig;
use serde::{Deserialize, Serialize};

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
