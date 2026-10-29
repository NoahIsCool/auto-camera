//! The `[[sources]]` table: the studio's video inputs.
//!
//! An entry is a [`SourceConfig`] (`kind = "file"`, `"synthetic"` or
//! `"capture"`, its keys flattened into the entry) plus a stable id and an
//! optional operator-facing name. The id is part of every shot id of the
//! source, so [`StudioConfig::validate`](super::StudioConfig::validate)
//! requires each to pass [`valid_id`](super::valid_id) and to be unique; a
//! source without a name is shown by its id. [`input_problems`] lists what
//! the opener of an entry's input would refuse before looking at any file,
//! so a load and `--check` catch it, not only a start.
//!
//! A source that cannot be opened when the engine starts (a file that is
//! not there yet, a camera that is not plugged in) starts offline and is
//! retried (see [`crate::runtime`]). Until it opens, it is described by
//! [`SourceEntry::placeholder_info`]: what its config says about its
//! picture (a capture source's requested mode), and the program's size for
//! a file, whose picture only the file knows. The engine takes the real size
//! from the source's first frame, and the real rate and audio from the
//! description of the connection that delivered it.

use ac_core::SourceId;
use ac_media::{MediaError, SourceInfo};
use ac_sources::capture::request_problems;
use ac_sources::segment::Segment;
use ac_sources::{FrameRate, SourceConfig, SyntheticPattern};
use serde::{Deserialize, Serialize};

/// One video source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceEntry {
    /// Stable identifier, used in shot ids; letters, digits, `-` and `_`.
    pub id: String,
    /// Operator-facing name (defaults to the id).
    #[serde(default)]
    pub name: Option<String>,
    /// How to open it (`kind = "file"`, `"synthetic"` or `"capture"`).
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
    /// docs): a synthetic source by its configured size and rate, a capture
    /// source by the mode it asks for, a file by `program_size` (width,
    /// height) at an unknown rate (`0.0`). None claims audio, which only an
    /// opened input can describe.
    pub fn placeholder_info(&self, program_size: (u32, u32)) -> SourceInfo {
        let (width, height, fps) = match &self.input {
            SourceConfig::File { .. } => (program_size.0, program_size.1, 0.0),
            SourceConfig::Synthetic {
                width, height, fps, ..
            }
            | SourceConfig::Capture {
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

/// What the opener of `input` would refuse before looking at any file: a
/// synthetic pattern's size, rate and subject count, against the limits
/// [`SyntheticPattern::new`] applies, a file's `max_width`, by
/// [`ac_sources::output_size`] itself, and its segment, by
/// [`Segment::from_seconds`], and a capture request ([`capture_problems`]). So a load
/// and `--check` catch them, not only a start. Whether the file is there is
/// the readiness check's question.
pub(super) fn input_problems(input: &SourceConfig) -> Vec<String> {
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
        SourceConfig::Capture { .. } => out.extend(capture_problems(input)),
    }
    out
}

/// Every problem with a capture source's request, as
/// [`ac_sources::capture::request_problems`] lists them (the unique ID,
/// the size, the rate, the pixel format); none for another kind, whose
/// checks are the config's own.
pub(super) fn capture_problems(input: &SourceConfig) -> Vec<String> {
    let SourceConfig::Capture {
        unique_id,
        width,
        height,
        fps,
        pixel_format,
    } = input
    else {
        return Vec::new();
    };
    request_problems(unique_id, *width, *height, *fps, pixel_format.as_deref())
}

#[cfg(test)]
mod capture_tests;

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
