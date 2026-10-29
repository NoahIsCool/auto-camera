//! Audio-only inputs: program audio with no picture (spec M12, "one stereo
//! input (board feed or embedded/file audio)").
//!
//! Every input here implements [`ac_media::AudioInput`]: it captures or
//! decodes on its own thread, tags every block with its id, a sequence
//! number and a PTS on the clock it is paced by, and keeps the blocks in a
//! bounded [`AudioRing`](ac_media::AudioRing), so its reader never waits and
//! never meets a backlog it did not drain.
//!
//! * [`AudioFileInput`] ([`file`](mod@file)) plays an audio-only file, a
//!   WAV or RF64 recording of the board, through the `ffmpeg` command
//!   line, as the rehearsal stand-in for the board's feed: paced on the
//!   clock as a device would deliver it, looping with PTS that keep
//!   counting.
//! * [`DeviceInput`] ([`device`]) reads the board itself: its USB audio
//!   interface, or any other CoreAudio device, named by its UID, with each
//!   block's capture time mapped to the clock the input is paced by, in a
//!   macOS build with the `coreaudio` feature.
//! * [`FakeAudioInput`] ([`fake`]) is an input whose sample clock drifts,
//!   whose blocks stall and which can be unplugged, on a plan, for testing
//!   what reads a live input, and [`write_fake_wav`] ([`wav`]) writes its
//!   signal to a WAV or RF64 file, for testing file inputs bit for bit.
//! * [`SupervisedAudio`] ([`supervised`]) keeps any of them on air as
//!   [`crate::SupervisedSource`] keeps a video source: an input that fails
//!   (ffmpeg killed, a device unplugged, a file not there yet) is opened
//!   again with the same backoff, its blocks' sequence numbers and PTS
//!   carrying on across connections.
//!
//! # Configuration
//!
//! [`AudioInputConfig`] is the serializable description of an input,
//! tagged by `kind`:
//!
//! ```json
//! {"kind": "file", "path": "board-rehearsal.wav", "loop_playback": true}
//! {"kind": "device", "uid": "AppleUSBAudioEngine:Allen&Heath:SQ:1A2B3C:1"}
//! ```
//!
//! [`open_audio_input`] opens one as a running `Box<dyn AudioInput>`, and
//! [`open_supervised_audio`] under a supervisor, with a check of each
//! connection the caller gives (the engine checks that the input has the
//! channels its config picks for program audio). Which channels become
//! program audio is not the input's business: the input delivers all of
//! them, and the engine picks ([`ac_media::ChannelPick`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ac_core::SourceId;
use ac_media::{AudioInput, AudioInputInfo, MediaError};
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::supervisor::Backoff;

pub mod device;
pub mod fake;
pub mod file;
pub mod supervised;
pub mod wav;

pub use device::{AudioDevice, DeviceBackend, DeviceInput, DeviceOptions, SystemDevices};
pub use fake::{FakeAudio, FakeAudioInput, FakeFeed, FeedState, Gap, fake_sample};
pub use file::{AudioFileInput, AudioFileOptions};
pub use supervised::{AudioOpener, SupervisedAudio};
pub use wav::{WavEncoding, write_fake_wav};

/// How to open an audio input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AudioInputConfig {
    /// An audio-only file (a WAV or RF64 recording of the board; any file
    /// ffmpeg reads plays the same way, its first audio stream) played as
    /// a live feed.
    File {
        /// Path to the file.
        path: PathBuf,
        /// Restart at end of file instead of stopping (default `false`).
        #[serde(default)]
        loop_playback: bool,
    },
    /// An audio device on the Mac, such as the soundboard's USB audio
    /// interface, by its CoreAudio UID; it opens only in a macOS build with
    /// the `coreaudio` feature ([`device`]).
    Device {
        /// The device's UID, as `auto-camera --list-audio-devices` prints
        /// it.
        uid: String,
    },
}

impl AudioInputConfig {
    /// The file the input reads, if it reads one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            AudioInputConfig::File { path, .. } => Some(path),
            AudioInputConfig::Device { .. } => None,
        }
    }

    /// The same, to change (to resolve a relative path).
    pub fn path_mut(&mut self) -> Option<&mut PathBuf> {
        match self {
            AudioInputConfig::File { path, .. } => Some(path),
            AudioInputConfig::Device { .. } => None,
        }
    }

    /// The device UID the input opens, if it opens a device.
    pub fn device_uid(&self) -> Option<&str> {
        match self {
            AudioInputConfig::Device { uid } => Some(uid),
            AudioInputConfig::File { .. } => None,
        }
    }

    /// Why the config cannot describe an input, if it cannot: a device
    /// UID that is empty. Whether the file or the device is there is not
    /// asked.
    pub fn problem(&self) -> Option<String> {
        match self {
            AudioInputConfig::Device { uid } if uid.trim().is_empty() => Some(
                "uid must name a device; `auto-camera --list-audio-devices` prints the UIDs".into(),
            ),
            _ => None,
        }
    }
}

/// Opens `config` as input `id`, paced by `clock`.
pub fn open_audio_input(
    config: &AudioInputConfig,
    id: SourceId,
    clock: Arc<dyn Clock>,
) -> Result<Box<dyn AudioInput>, MediaError> {
    match config {
        AudioInputConfig::File {
            path,
            loop_playback,
        } => {
            let options = AudioFileOptions {
                loop_playback: *loop_playback,
                ..AudioFileOptions::default()
            };
            Ok(Box::new(AudioFileInput::open_with_clock(
                id, path, options, clock,
            )?))
        }
        AudioInputConfig::Device { uid } => Ok(Box::new(DeviceInput::open(
            id,
            uid,
            &SystemDevices,
            DeviceOptions::default(),
            clock,
        )?)),
    }
}

/// Opens `config` as a supervised input paced by `clock`; `offline`
/// describes it (and gives its id) while it has not opened. Every
/// connection is first checked with `accept`: one it refuses is stopped
/// and counts as an attempt that failed, its message the reason.
pub fn open_supervised_audio(
    config: &AudioInputConfig,
    offline: AudioInputInfo,
    accept: impl Fn(&AudioInputInfo) -> Result<(), String> + Send + 'static,
    clock: Arc<dyn Clock>,
    backoff: Backoff,
) -> Result<SupervisedAudio, MediaError> {
    let (config, id, pacing) = (config.clone(), offline.id.clone(), clock.clone());
    let opener = move || {
        let input = open_audio_input(&config, id.clone(), pacing.clone())?;
        // A refused input is dropped, which stops it.
        accept(input.info()).map_err(MediaError::Open)?;
        Ok(input)
    };
    SupervisedAudio::start(opener, offline, clock, backoff)
}

#[cfg(test)]
mod tests;
