//! The `[output]` table: program picture, encoder and default destinations.
//!
//! The program rate is also the engine's: `fps` is the master clock rate
//! ([`StudioConfig::frame_period`](super::StudioConfig::frame_period)). The
//! destinations here are defaults for `StartOutput`, which may name others,
//! and [`OutputSettings::to_output_config`] builds one session's
//! [`OutputConfig`] from both. [`EncoderSetting`] is the config's spelling
//! of [`ac_output::Encoder`].
//!
//! # One set of ranges
//!
//! The encoder refuses a value outside its ranges ([`OutputConfig::validate`]:
//! even sizes up to 7680x4320, 1..=120 fps, 100..=100000 kbit/s video,
//! 32..=512 kbit/s audio, a keyframe every 1..=10 s). A config that passed
//! looser checks of its own would load and then fail when the operator
//! starts the output, possibly minutes into a show, so
//! [`OutputSettings::problems`] asks the encoder's own check
//! ([`OutputConfig::problems`], every bad value at once) instead of
//! repeating its numbers: it builds the session config a start would build,
//! with a stand-in recording path when the table names no destination and
//! a stand-in audio format so the AAC bitrate is checked whether or not
//! `audio_source` is set. The check stays pure: whether a recording path
//! exists, or its folder does, is asked at start (and by the readiness
//! check, [`crate::readiness`]).

use std::path::PathBuf;

use ac_output::{AudioFormat, Encoder, OutputConfig};
use serde::{Deserialize, Serialize};

/// The recording path checked when the table names no destination: any
/// valid one, since only the other values are in question then.
const STAND_IN_RECORDING: &str = "program.mp4";

/// The audio format checked with, a valid one, so the encoder's check
/// covers `audio_bitrate_kbps`; a session's real format comes from its
/// source when the output starts.
const STAND_IN_AUDIO: AudioFormat = AudioFormat {
    sample_rate: 48_000,
    channels: 2,
};

/// Which H.264 encoder to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderSetting {
    /// VideoToolbox on macOS when it can encode here, otherwise libx264.
    #[default]
    Auto,
    /// The x264 software encoder.
    Libx264,
    /// Apple's hardware encoder.
    Videotoolbox,
}

impl From<EncoderSetting> for Encoder {
    fn from(e: EncoderSetting) -> Self {
        match e {
            EncoderSetting::Auto => Encoder::Auto,
            EncoderSetting::Libx264 => Encoder::Libx264,
            EncoderSetting::Videotoolbox => Encoder::VideoToolbox,
        }
    }
}

/// Program picture, encoder and default destinations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputSettings {
    /// Program width in pixels; even, `16..=7680`.
    pub width: u32,
    /// Program height in pixels; even, `16..=4320`.
    pub height: u32,
    /// Program frames per second, `1..=120`; also the master clock rate.
    pub fps: u32,
    /// Video bitrate in kbit/s, `100..=100000`.
    pub video_bitrate_kbps: u32,
    /// Seconds between keyframes, `1..=10`; 2 suits RTMP ingest.
    pub keyframe_seconds: u32,
    /// H.264 encoder.
    pub encoder: EncoderSetting,
    /// ffmpeg executable. Defaults to the one file sources decode with:
    /// `AC_FFMPEG`, else `ffmpeg` on `PATH` (on macOS also Homebrew's
    /// directories), so one setting serves both. Unlike the config's file
    /// paths, it is run as given: a bare name is looked up on `PATH` and a
    /// relative path is taken from the working folder, not the config's.
    pub ffmpeg: PathBuf,
    /// Default RTMP URL (with stream key) for `StartOutput`; `rtmp://` or
    /// `rtmps://`. Empty means none.
    pub rtmp_url: Option<String>,
    /// Default recording path for `StartOutput`; it must not exist when the
    /// output starts. Empty means none. Like `ffmpeg` and unlike the
    /// config's other paths, a relative one is taken from the working
    /// folder, as a path typed at Start Output is.
    pub record_path: Option<PathBuf>,
    /// Source whose embedded audio becomes program audio; silent when unset.
    pub audio_source: Option<String>,
    /// AAC bitrate in kbit/s, `32..=512`.
    pub audio_bitrate_kbps: u32,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            video_bitrate_kbps: 6000,
            keyframe_seconds: 2,
            encoder: EncoderSetting::Auto,
            ffmpeg: ac_sources::FfmpegTools::from_env().ffmpeg,
            rtmp_url: None,
            record_path: None,
            audio_source: None,
            audio_bitrate_kbps: 160,
        }
    }
}

impl OutputSettings {
    /// Checks every value with the encoder's own ranges, as a start would,
    /// failing on the first of [`Self::problems`].
    pub fn validate(&self) -> Result<(), String> {
        self.problems().into_iter().next().map_or(Ok(()), Err)
    }

    /// Every value the encoder would refuse (see the module docs), each
    /// message naming its key; empty when the table is fine.
    pub fn problems(&self) -> Vec<String> {
        let rtmp_url = self.rtmp_url.clone().filter(|u| !u.trim().is_empty());
        let record_path = self
            .record_path
            .clone()
            .filter(|p| !p.as_os_str().is_empty());
        let record_path = match (&rtmp_url, record_path) {
            (None, None) => Some(PathBuf::from(STAND_IN_RECORDING)),
            (_, path) => path,
        };
        self.to_output_config(rtmp_url, record_path, Some(STAND_IN_AUDIO))
            .problems()
            .into_iter()
            .map(|p| format!("output: {p}"))
            .collect()
    }

    /// The encoder config for one output session.
    pub fn to_output_config(
        &self,
        rtmp_url: Option<String>,
        record_path: Option<PathBuf>,
        audio: Option<AudioFormat>,
    ) -> OutputConfig {
        OutputConfig {
            ffmpeg: self.ffmpeg.clone(),
            width: self.width,
            height: self.height,
            fps: self.fps,
            video_bitrate_kbps: self.video_bitrate_kbps,
            keyframe_seconds: self.keyframe_seconds,
            encoder: self.encoder.into(),
            rtmp_url,
            record_path,
            audio,
            audio_bitrate_kbps: self.audio_bitrate_kbps,
        }
    }
}

#[cfg(test)]
mod tests;
