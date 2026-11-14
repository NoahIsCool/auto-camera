//! The source picker's model: which video file or cameras the studio
//! reads, written into the operator's studio file.
//!
//! # Theory
//!
//! The operator should not have to edit TOML to say "play this file" or
//! "use these two cameras". The window's source picker ([`crate::ui`])
//! asks for a [`Choice`], and [`save`] turns it into the `[[sources]]` of
//! the studio file, [`crate::cli::DEFAULT_CONFIG`] (`config/show.toml`),
//! which a bare launch opens. Everything else in that file (queries,
//! detector, director, output, the operator's comments) is kept as it is:
//! the file is edited as a TOML document ([`edit`]), not re-serialised, so a
//! hand-tuned studio survives a change of cameras. When there is no studio
//! file yet, the new one starts from the file-source example, whose
//! comments explain every setting, with its detector turned off when this
//! build or this machine cannot run it (no `onnx` feature, or the model
//! files are not where the example says).
//!
//! The cameras come from macOS's `system_profiler`, the same listing a
//! capture source resolves its unique ID against when it opens
//! ([`ac_sources::capture::devices`]), so a camera picked here is named
//! the way the engine will look for it ([`list_cameras`]).
//!
//! # Invariants
//!
//! * A studio file that would not load is never written: the new text is
//!   parsed, its paths resolved and validated as the engine would before
//!   it replaces the old file, and it replaces it atomically
//!   ([`crate::show::disk::write_atomic`]), so the file is always the old
//!   studio or the new one.
//! * Sources that stay keep their table as it was (id, name, any tuning
//!   and comments); only their capture mode follows the picker. New
//!   sources get the lowest free `cam-N` id.
//! * Program audio never points at a source that is gone: an
//!   `audio_source` naming an audio input stays; one naming a video source
//!   follows the new video file, or is removed for cameras (which carry no
//!   audio).

mod edit;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use ac_engine::{DetectorSettings, StudioConfig};
use ac_sources::capture::devices::parse_camera_report;
use ac_sources::probe::FfmpegTools;
use ac_sources::{DeviceInventory, SystemInventory};

use crate::show::disk::{SystemDisk, write_atomic};

pub use edit::{current, studio_text};

/// The example a new studio file starts from (see the module docs).
pub const TEMPLATE: &str = include_str!("../../../config/file-source.example.toml");

/// What the studio reads; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// One video file, played in a loop with its audio.
    File(PathBuf),
    /// One or more cameras, each asked for `mode`.
    Cameras {
        /// The cameras, in the order of their sources.
        cameras: Vec<Camera>,
        /// The picture every camera is asked for.
        mode: Mode,
    },
}

/// A camera as the picker shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Camera {
    /// Its name, as macOS gives it.
    pub name: String,
    /// Its unique ID, which a capture source names it by.
    pub unique_id: String,
}

/// A capture mode: the picture a camera is asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frames per second.
    pub fps: f64,
}

impl Mode {
    /// How the picker names it, as in "1280×720 at 30 fps".
    pub fn label(&self) -> String {
        format!("{}×{} at {} fps", self.width, self.height, self.fps)
    }
}

/// The modes the picker offers, the default first: 720p30 is what the
/// example studio asks for and what almost every webcam and HDMI capture
/// dongle offers.
pub const MODES: [Mode; 4] = [
    Mode {
        width: 1280,
        height: 720,
        fps: 30.0,
    },
    Mode {
        width: 1920,
        height: 1080,
        fps: 30.0,
    },
    Mode {
        width: 1280,
        height: 720,
        fps: 60.0,
    },
    Mode {
        width: 1920,
        height: 1080,
        fps: 60.0,
    },
];

/// What a studio file reads now, to open the picker on it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Current {
    /// The first video file source's path, as written.
    pub file: Option<PathBuf>,
    /// The capture sources' cameras, in order (named by the source's name,
    /// or its unique ID when it has none).
    pub cameras: Vec<Camera>,
    /// The first capture source's mode, when it is one of [`MODES`].
    pub mode: Option<Mode>,
}

/// Writes `choice` into the studio file at `path` (made from [`TEMPLATE`]
/// when there is none), after checking that the result loads; see the
/// module docs. The error says why nothing was written.
pub fn save(path: &Path, choice: &Choice) -> Result<(), String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let dir = folder(path);
    let text = match &existing {
        Some(text) => studio_text(text, choice, false, &dir)?,
        None => {
            let text = studio_text(TEMPLATE, choice, true, &dir)?;
            if detector_runs(&text, &dir) {
                text
            } else {
                edit::without_detector(&text)?
            }
        }
    };
    check(&text, &dir).map_err(|e| format!("{} was not changed: {e}", path.display()))?;
    write_atomic(path, text.as_bytes(), &mut SystemDisk)
}

/// The folder relative paths in the file at `path` are resolved against.
fn folder(path: &Path) -> PathBuf {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Parses `text` as a studio file in `dir`, as the engine would load it.
fn check(text: &str, dir: &Path) -> Result<StudioConfig, String> {
    let mut config = StudioConfig::parse(text, false)?;
    config.resolve_paths(dir);
    config.validate().map_err(|e| e.to_string())?;
    Ok(config)
}

/// Whether the detector of the studio `text` in `dir` can run here: none
/// is configured, or OWLv2 is and this build has `onnx` and the model
/// files are there. A sidecar or synthetic detector counts as running
/// (its own readiness is the `--check`'s business).
fn detector_runs(text: &str, dir: &Path) -> bool {
    let Ok(config) = check(text, dir) else {
        return false;
    };
    match &config.detector {
        DetectorSettings::Owl {
            model,
            text_encoder,
            tokenizer,
            ..
        } => {
            cfg!(feature = "onnx")
                && [Some(model), text_encoder.as_ref(), Some(tokenizer)]
                    .into_iter()
                    .flatten()
                    .all(|f| f.is_file())
        }
        _ => true,
    }
}

/// The cameras connected now, from macOS's `system_profiler` (see the
/// module docs); a camera without a unique ID is left out, since a source
/// cannot name it. Blocks for up to ten seconds; the window calls it off
/// its thread. Elsewhere than on macOS the error says that capture needs
/// it.
pub fn list_cameras() -> Result<Vec<Camera>, String> {
    let inventory = SystemInventory::new(&FfmpegTools::from_env());
    let report = inventory.camera_report().map_err(|e| e.to_string())?;
    let cameras = parse_camera_report(&report).map_err(|e| e.to_string())?;
    Ok(cameras
        .into_iter()
        .filter_map(|c| {
            let unique_id = c.unique_id?.trim().to_owned();
            (!unique_id.is_empty()).then_some(Camera {
                name: c.name.trim().to_owned(),
                unique_id,
            })
        })
        .collect())
}
