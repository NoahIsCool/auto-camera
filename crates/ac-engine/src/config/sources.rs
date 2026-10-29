//! The `[[sources]]` table: the studio's video inputs.
//!
//! An entry is a [`SourceConfig`] (`kind = "file"` or `"synthetic"`, its
//! keys flattened into the entry) plus a stable id and an optional
//! operator-facing name. The id is part of every shot id of the source, so
//! [`StudioConfig::validate`](super::StudioConfig::validate) requires each
//! to pass [`valid_id`](super::valid_id) and to be unique; a source without
//! a name is shown by its id.
//!
//! A source that cannot be opened when the engine starts (a file that is
//! not there yet) starts offline and is retried (see [`crate::runtime`]).
//! Until it opens, it is described by [`SourceEntry::placeholder_info`]:
//! what its config says about its picture, and the program's size for a
//! file, whose picture only the file knows. The engine takes the real size
//! from the source's first frame, and the real rate and audio from the
//! description of the connection that delivered it.

use ac_core::SourceId;
use ac_media::SourceInfo;
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

    /// How the source is described while it has not opened (see the module
    /// docs): a synthetic source by its configured size and rate, a file by
    /// `program_size` (width, height) at an unknown rate (`0.0`). Neither
    /// claims audio, which only an opened input can describe.
    pub fn placeholder_info(&self, program_size: (u32, u32)) -> SourceInfo {
        let (width, height, fps) = match &self.input {
            SourceConfig::File { .. } => (program_size.0, program_size.1, 0.0),
            SourceConfig::Synthetic {
                width, height, fps, ..
            } => (*width, *height, *fps),
        };
        SourceInfo {
            id: self.source_id(),
            width,
            height,
            fps,
            audio: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry `id` opening `input`.
    fn entry(input: SourceConfig) -> SourceEntry {
        SourceEntry {
            id: "cam".into(),
            name: None,
            input,
        }
    }

    #[test]
    fn a_placeholder_is_what_the_config_knows() {
        let file = entry(SourceConfig::File {
            path: "later.mp4".into(),
            loop_playback: false,
            max_width: Some(640),
            audio: true,
            start_s: None,
            duration_s: None,
        });
        let info = file.placeholder_info((1280, 720));
        assert_eq!(info.id, SourceId("cam".into()));
        assert_eq!((info.width, info.height, info.fps), (1280, 720, 0.0));
        assert_eq!(info.audio, None, "not known before it opens");
        let pattern = entry(SourceConfig::Synthetic {
            width: 64,
            height: 36,
            fps: 25.0,
            subjects: 1,
        });
        let info = pattern.placeholder_info((1280, 720));
        assert_eq!((info.width, info.height, info.fps), (64, 36, 25.0));
    }
}
