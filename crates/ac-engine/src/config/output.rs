//! The `[output]` table: program picture, encoder and default destinations.
//!
//! The program rate is also the engine's: `fps` is the master clock rate
//! ([`StudioConfig::frame_period`](super::StudioConfig::frame_period)). The
//! destinations here are defaults for `StartOutput`, which may name others,
//! and [`OutputSettings::to_output_config`] builds one session's
//! [`OutputConfig`] from both. [`EncoderSetting`] is the config's spelling
//! of [`ac_output::Encoder`].

use std::path::PathBuf;

use ac_output::{AudioFormat, Encoder, OutputConfig};
use serde::{Deserialize, Serialize};

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
    /// Program width in pixels; even.
    pub width: u32,
    /// Program height in pixels; even.
    pub height: u32,
    /// Program frames per second; also the master clock rate.
    pub fps: u32,
    /// Video bitrate in kbit/s.
    pub video_bitrate_kbps: u32,
    /// Seconds between keyframes.
    pub keyframe_seconds: u32,
    /// H.264 encoder.
    pub encoder: EncoderSetting,
    /// ffmpeg executable. Defaults to the one file sources decode with:
    /// `AC_FFMPEG`, else `ffmpeg` on `PATH` (on macOS also Homebrew's
    /// directories), so one setting serves both.
    pub ffmpeg: PathBuf,
    /// Default RTMP URL (with stream key) for `StartOutput`.
    pub rtmp_url: Option<String>,
    /// Default recording path for `StartOutput`.
    pub record_path: Option<PathBuf>,
    /// Source whose embedded audio becomes program audio; silent when unset.
    pub audio_source: Option<String>,
    /// AAC bitrate in kbit/s.
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
    /// Checks sizes, rate and bitrates.
    pub fn validate(&self) -> Result<(), String> {
        let even = |v: u32| v.is_multiple_of(2) && (16..=7680).contains(&v);
        if !even(self.width) || !even(self.height) {
            return Err(format!(
                "output size {}x{} must be even and within 16..=7680",
                self.width, self.height
            ));
        }
        if !(1..=120).contains(&self.fps) {
            return Err(format!("output.fps {} is outside 1..=120", self.fps));
        }
        if self.video_bitrate_kbps == 0 || self.audio_bitrate_kbps == 0 {
            return Err("output bitrates must be positive".into());
        }
        if !(1..=60).contains(&self.keyframe_seconds) {
            return Err("output.keyframe_seconds must be in 1..=60".into());
        }
        Ok(())
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
