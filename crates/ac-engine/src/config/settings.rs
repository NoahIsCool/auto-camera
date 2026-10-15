//! The simple sections of a [`StudioConfig`](super::StudioConfig).
//!
//! Each section is a flat table with defaults for every key
//! (`#[serde(default)]`) that rejects unknown keys, so a typo in a config
//! file is an error rather than a silently ignored setting. Durations are
//! written as seconds (`*_s` keys) and converted, with range checks, when the
//! engine builds its components.

use std::path::PathBuf;
use std::time::Duration;

use ac_director::{ContentProfile, DirectorMode};
use ac_output::{AudioFormat, Encoder, OutputConfig};
use ac_subjects::StoreConfig;
use ac_switcher::{Transition, TransitionKind};
use serde::{Deserialize, Serialize};

use super::ConfigError;

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

/// Default score of the synthetic detector.
fn default_score() -> f32 {
    0.9
}

/// The open-vocabulary detector used for discovery on every source.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DetectorSettings {
    /// No detector: only wide shots and operator reacquires.
    #[default]
    None,
    /// Ground truth of synthetic sources, answering every query with every
    /// moving box. For demos and tests without a model.
    Synthetic {
        /// Score given to every box.
        #[serde(default = "default_score")]
        score: f32,
    },
    /// OWLv2 / OWL-ViT through ONNX Runtime (needs the `onnx` feature).
    Owl {
        /// The combined graph, or the image detector when `text_encoder` is set.
        model: PathBuf,
        /// Text encoder of a split export.
        #[serde(default)]
        text_encoder: Option<PathBuf>,
        /// `tokenizer.json` of the model.
        tokenizer: PathBuf,
        /// Model input side (960 for OWLv2 base).
        #[serde(default)]
        input_size: Option<u32>,
        /// Score threshold for every query.
        #[serde(default)]
        score_threshold: Option<f32>,
        /// Try CoreML on macOS.
        #[serde(default)]
        use_coreml: Option<bool>,
    },
}

impl DetectorSettings {
    /// Checks scores and sizes.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            DetectorSettings::None => Ok(()),
            DetectorSettings::Synthetic { score } => {
                if *score > 0.0 && *score <= 1.0 {
                    Ok(())
                } else {
                    Err(format!("detector.score {score} must be in (0, 1]"))
                }
            }
            DetectorSettings::Owl {
                input_size,
                score_threshold,
                ..
            } => {
                if input_size.is_some_and(|s| !(32..=4096).contains(&s)) {
                    return Err("detector.input_size must be in 32..=4096".into());
                }
                if score_threshold.is_some_and(|t| !(0.0..=1.0).contains(&t)) {
                    return Err("detector.score_threshold must be in 0..=1".into());
                }
                Ok(())
            }
        }
    }
}

/// How often discovery runs and when its results are stale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoverySettings {
    /// Detector runs per second per source, at most.
    pub rate_hz: f64,
    /// Results computed on a frame older than this (relative to the newest
    /// frame of the source) are dropped, in seconds.
    pub max_result_age_s: f64,
    /// A request without an answer after this long is abandoned, in seconds.
    pub timeout_s: f64,
    /// A subject the detector has not seen for this long (relative to the
    /// newest detection) is no longer kept alive by its tracker, in seconds.
    pub max_undetected_s: f64,
}

impl Default for DiscoverySettings {
    fn default() -> Self {
        Self {
            rate_hz: 1.0,
            max_result_age_s: 2.0,
            timeout_s: 10.0,
            max_undetected_s: 3.0,
        }
    }
}

impl DiscoverySettings {
    /// Checks the rate and the ages.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.rate_hz > 0.0 && self.rate_hz <= 30.0) {
            return Err(format!(
                "discovery.rate_hz {} must be in (0, 30]",
                self.rate_hz
            ));
        }
        for (name, v) in [
            ("max_result_age_s", self.max_result_age_s),
            ("timeout_s", self.timeout_s),
            ("max_undetected_s", self.max_undetected_s),
        ] {
            if !(v > 0.0 && v <= 3600.0) {
                return Err(format!("discovery.{name} {v} must be in (0, 3600]"));
            }
        }
        Ok(())
    }

    /// Interval between requests.
    pub fn interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.rate_hz)
    }

    /// Largest accepted result age.
    pub fn max_result_age(&self) -> Duration {
        Duration::from_secs_f64(self.max_result_age_s)
    }

    /// Request timeout.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs_f64(self.timeout_s)
    }

    /// Longest a tracker may keep an undetected subject alive.
    pub fn max_undetected(&self) -> Duration {
        Duration::from_secs_f64(self.max_undetected_s)
    }
}

/// Subject store tuning (the remaining store parameters keep their defaults).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubjectSettings {
    /// Soft cap on subjects per source, in `1..=16`.
    pub max_subjects: usize,
    /// Detections below this score are ignored.
    pub min_score: f32,
    /// Seconds without a confident observation before a subject is lost.
    pub lost_after_s: f64,
    /// Seconds lost before an unprotected subject is retired.
    pub retire_after_s: f64,
    /// Track observations below this confidence are ignored.
    pub track_min_confidence: f32,
}

impl Default for SubjectSettings {
    fn default() -> Self {
        let d = StoreConfig::default();
        Self {
            max_subjects: d.max_subjects,
            min_score: d.min_score,
            lost_after_s: d.lost_after.as_secs_f64(),
            retire_after_s: d.retire_after.as_secs_f64(),
            track_min_confidence: d.track_min_confidence,
        }
    }
}

impl SubjectSettings {
    /// The store config, with durations range-checked.
    pub fn to_store_config(&self) -> Result<StoreConfig, ConfigError> {
        let secs = |name: &str, v: f64| {
            if v.is_finite() && v > 0.0 && v <= 86_400.0 {
                Ok(Duration::from_secs_f64(v))
            } else {
                Err(ConfigError::Invalid(format!(
                    "subjects.{name} {v} must be in (0, 86400]"
                )))
            }
        };
        Ok(StoreConfig {
            max_subjects: self.max_subjects,
            min_score: self.min_score,
            lost_after: secs("lost_after_s", self.lost_after_s)?,
            retire_after: secs("retire_after_s", self.retire_after_s)?,
            track_min_confidence: self.track_min_confidence,
            ..StoreConfig::default()
        })
    }
}

/// Auto-director mode and content profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DirectorSettings {
    /// Off, suggest (default) or auto.
    pub mode: DirectorMode,
    /// Talk (default) or music.
    pub profile: ContentProfile,
}

/// The switcher's initial transition.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SwitcherSettings {
    /// Cut, mix (default) or dip.
    pub transition: TransitionKind,
    /// Duration in seconds for mix and dip.
    pub duration: f64,
}

impl Default for SwitcherSettings {
    fn default() -> Self {
        let t = Transition::default();
        Self {
            transition: t.kind,
            duration: t.duration,
        }
    }
}

impl SwitcherSettings {
    /// Checks the duration.
    pub fn validate(&self) -> Result<(), String> {
        if self.transition != TransitionKind::Cut
            && !(self.duration.is_finite() && self.duration > 0.0 && self.duration <= 10.0)
        {
            return Err(format!(
                "switcher.duration {} must be in (0, 10] seconds",
                self.duration
            ));
        }
        Ok(())
    }

    /// The transition.
    pub fn to_transition(&self) -> Transition {
        Transition {
            kind: self.transition,
            duration: self.duration,
        }
    }
}

/// What is rendered for the operator UI.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MonitorSettings {
    /// Program and preview monitor width.
    pub width: u32,
    /// Program and preview monitor height.
    pub height: u32,
    /// Monitor refreshes per second; 0 disables the monitors.
    pub rate_hz: f64,
    /// Multiview tile width.
    pub thumb_width: u32,
    /// Multiview tile height.
    pub thumb_height: u32,
    /// Multiview refreshes per second; 0 disables the tiles.
    pub thumb_rate_hz: f64,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            width: 960,
            height: 540,
            rate_hz: 30.0,
            thumb_width: 320,
            thumb_height: 180,
            thumb_rate_hz: 10.0,
        }
    }
}

impl MonitorSettings {
    /// Settings with monitors and tiles disabled (headless runs).
    pub fn disabled() -> Self {
        Self {
            rate_hz: 0.0,
            thumb_rate_hz: 0.0,
            ..Self::default()
        }
    }

    /// Checks sizes and rates.
    pub fn validate(&self) -> Result<(), String> {
        let side = |v: u32| (16..=3840).contains(&v);
        if !side(self.width) || !side(self.height) || !side(self.thumb_width) {
            return Err("monitor sizes must be within 16..=3840".into());
        }
        if !side(self.thumb_height) {
            return Err("monitor sizes must be within 16..=3840".into());
        }
        for (name, v) in [
            ("rate_hz", self.rate_hz),
            ("thumb_rate_hz", self.thumb_rate_hz),
        ] {
            if !(v.is_finite() && (0.0..=120.0).contains(&v)) {
                return Err(format!("monitors.{name} {v} must be in 0..=120"));
            }
        }
        Ok(())
    }
}
