//! Serializable source descriptions and the factory that opens them.
//!
//! A [`SourceConfig`] is what persistence and the operator UI store for a
//! source: plain data, tagged by `kind`, with defaults for everything optional
//! so hand-written configs stay short:
//!
//! ```json
//! {"kind": "file", "path": "rehearsal.mp4", "loop_playback": true, "max_width": 1920}
//! {"kind": "file", "path": "service.mp4", "start_s": 1800, "duration_s": 240}
//! {"kind": "synthetic", "width": 1280, "height": 720, "fps": 30}
//! ```
//!
//! A file's `start_s` and `duration_s` play a segment of it, as if it were a
//! clip of its own: its first frame is the first at or after `start_s`, with
//! PTS 0, and its audio starts with that frame ([`crate::segment`]).
//!
//! [`open_source`] turns a config into a running source behind the
//! [`VideoSource`] trait, so callers never depend on the concrete type.

use std::path::PathBuf;
use std::sync::Arc;

use ac_core::SourceId;
use ac_media::{MediaError, VideoSource};
use serde::{Deserialize, Serialize};

use crate::clock::{Clock, WallClock};
use crate::ffmpeg::check_max_width;
use crate::file::{FileOptions, FileSource};
use crate::segment::Segment;
use crate::synthetic::{SyntheticPattern, SyntheticSource};

/// Default for [`SourceConfig::File::audio`]: decode audio when present.
fn default_audio() -> bool {
    true
}

/// Default for [`SourceConfig::Synthetic::subjects`].
fn default_subjects() -> u32 {
    2
}

/// How to open a source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceConfig {
    /// A local video file decoded with ffmpeg.
    File {
        /// Path to the file.
        path: PathBuf,
        /// Restart at end of file instead of stopping (default `false`).
        #[serde(default)]
        loop_playback: bool,
        /// Scale frames down to at most this width (default: native size).
        #[serde(default)]
        max_width: Option<u32>,
        /// Decode the file's audio when it has any (default `true`).
        #[serde(default = "default_audio")]
        audio: bool,
        /// Start this many seconds into the file (default 0); the first
        /// frame at or after that time is delivered first, with PTS 0
        /// ([`crate::segment`]).
        #[serde(default)]
        start_s: Option<f64>,
        /// Play at most this many seconds from `start_s` (default: to the
        /// end of the file).
        #[serde(default)]
        duration_s: Option<f64>,
    },
    /// A deterministic moving test pattern.
    Synthetic {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Frames per second.
        fps: f64,
        /// Number of moving subjects (default 2).
        #[serde(default = "default_subjects")]
        subjects: u32,
    },
}

impl SourceConfig {
    /// Checks what can be checked before the source is opened, the mistakes
    /// no reopening can mend: a synthetic pattern's parameters, a file's
    /// `max_width` ([`check_max_width`]) and its segment
    /// ([`Segment::from_seconds`]). A file is not looked for, since it may
    /// appear later and a supervised source then picks it up
    /// ([`crate::supervisor`]).
    pub fn validate(&self) -> Result<(), MediaError> {
        match self {
            SourceConfig::File {
                max_width,
                start_s,
                duration_s,
                ..
            } => {
                check_max_width(*max_width)?;
                Segment::from_seconds(*start_s, *duration_s).map(|_| ())
            }
            SourceConfig::Synthetic {
                width,
                height,
                fps,
                subjects,
            } => SyntheticPattern::check(*width, *height, *fps, *subjects).map(|_| ()),
        }
    }
}

/// Opens `config` as source `id`, paced in real time.
pub fn open_source(
    config: &SourceConfig,
    id: SourceId,
) -> Result<Box<dyn VideoSource>, MediaError> {
    open_source_with_clock(config, id, Arc::new(WallClock::new()))
}

/// Opens `config` as source `id`, paced by `clock`.
pub fn open_source_with_clock(
    config: &SourceConfig,
    id: SourceId,
    clock: Arc<dyn Clock>,
) -> Result<Box<dyn VideoSource>, MediaError> {
    match config {
        SourceConfig::File {
            path,
            loop_playback,
            max_width,
            audio,
            start_s,
            duration_s,
        } => {
            let options = FileOptions {
                loop_playback: *loop_playback,
                max_width: *max_width,
                audio: *audio,
                segment: Segment::from_seconds(*start_s, *duration_s)?,
                ..FileOptions::default()
            };
            Ok(Box::new(FileSource::open_with_clock(
                id, path, options, clock,
            )?))
        }
        SourceConfig::Synthetic {
            width,
            height,
            fps,
            subjects,
        } => {
            let pattern = SyntheticPattern::new(*width, *height, *fps, *subjects)?;
            Ok(Box::new(SyntheticSource::start_with_clock(
                id, pattern, clock,
            )?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::VirtualClock;
    use ac_media::SourceState;
    use std::time::{Duration, Instant};

    #[test]
    fn file_config_fills_defaults() {
        let c: SourceConfig = serde_json::from_str(r#"{"kind": "file", "path": "a.mp4"}"#).unwrap();
        assert_eq!(
            c,
            SourceConfig::File {
                path: "a.mp4".into(),
                loop_playback: false,
                max_width: None,
                audio: true,
                start_s: None,
                duration_s: None,
            }
        );
    }

    #[test]
    fn synthetic_config_fills_defaults() {
        let json = r#"{"kind": "synthetic", "width": 640, "height": 360, "fps": 30}"#;
        let c: SourceConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            c,
            SourceConfig::Synthetic {
                width: 640,
                height: 360,
                fps: 30.0,
                subjects: 2,
            }
        );
    }

    #[test]
    fn configs_round_trip_through_json() {
        let configs = [
            SourceConfig::File {
                path: "/media/clip.mov".into(),
                loop_playback: true,
                max_width: Some(1280),
                audio: false,
                start_s: Some(1800.5),
                duration_s: Some(240.0),
            },
            SourceConfig::Synthetic {
                width: 1920,
                height: 1080,
                fps: 59.94,
                subjects: 5,
            },
        ];
        for c in configs {
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<SourceConfig>(&json).unwrap(), c);
        }
        let json = serde_json::to_value(SourceConfig::Synthetic {
            width: 1,
            height: 1,
            fps: 1.0,
            subjects: 0,
        })
        .unwrap();
        assert_eq!(json["kind"], "synthetic");
    }

    #[test]
    fn malformed_configs_are_rejected() {
        for json in [
            r#"{"kind": "camera", "path": "x"}"#,
            r#"{"kind": "file"}"#,
            r#"{"kind": "file", "path": "a.mp4", "lop_playback": true}"#,
            r#"{"kind": "synthetic", "width": 64, "height": 36}"#,
            r#"{"path": "a.mp4"}"#,
        ] {
            assert!(
                serde_json::from_str::<SourceConfig>(json).is_err(),
                "{json}"
            );
        }
    }

    #[test]
    fn opens_a_synthetic_source() {
        let config = SourceConfig::Synthetic {
            width: 32,
            height: 18,
            fps: 30.0,
            subjects: 1,
        };
        let clock = Arc::new(VirtualClock::new());
        let mut src = open_source_with_clock(&config, SourceId("s".into()), clock).unwrap();
        assert_eq!(src.info().id, SourceId("s".into()));
        assert_eq!(src.info().fps, 30.0);
        assert_eq!(&src.current_info(), src.info(), "a plain source's is fixed");
        let deadline = Instant::now() + Duration::from_secs(10);
        while src.latest().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(src.latest().is_some());
        src.stop();
        assert_eq!(src.state(), SourceState::Stopped);
    }

    #[test]
    fn invalid_synthetic_config_is_an_open_error() {
        let config = SourceConfig::Synthetic {
            width: 0,
            height: 18,
            fps: 30.0,
            subjects: 1,
        };
        assert!(matches!(
            open_source(&config, SourceId("s".into())),
            Err(MediaError::Open(_))
        ));
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("size 0x18"), "{err}");
    }

    #[test]
    fn validation_checks_patterns_but_does_not_look_for_files() {
        let pattern = SourceConfig::Synthetic {
            width: 32,
            height: 18,
            fps: 30.0,
            subjects: 1,
        };
        assert!(pattern.validate().is_ok());
        let missing = SourceConfig::File {
            path: "/nonexistent/auto-camera/clip.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: true,
            start_s: Some(2.0),
            duration_s: Some(0.5),
        };
        assert!(missing.validate().is_ok(), "it may appear later");
        // A width no picture can be scaled to fails before any file is
        // looked for, and as opening it would.
        for (max_width, ok) in [(Some(0), false), (Some(1), false), (Some(2), true)] {
            let file = SourceConfig::File {
                path: "/nonexistent/auto-camera/clip.mp4".into(),
                loop_playback: false,
                max_width,
                audio: true,
                start_s: None,
                duration_s: None,
            };
            match file.validate() {
                Ok(()) => assert!(ok, "{max_width:?} accepted"),
                Err(e) => {
                    assert!(!ok, "{max_width:?} rejected: {e}");
                    let e = e.to_string();
                    assert!(e.contains("max_width must be at least 2"), "{e}");
                }
            }
        }
        // So does a segment no file can have.
        for (start_s, duration_s, key) in [
            (Some(-1.0), None, "start_s"),
            (Some(f64::NAN), None, "start_s"),
            (None, Some(0.0), "duration_s"),
        ] {
            let file = SourceConfig::File {
                path: "/nonexistent/auto-camera/clip.mp4".into(),
                loop_playback: false,
                max_width: None,
                audio: true,
                start_s,
                duration_s,
            };
            let e = file.validate().unwrap_err().to_string();
            assert!(e.contains(key), "{e}");
        }
    }

    #[test]
    fn missing_file_is_an_open_error() {
        let config = SourceConfig::File {
            path: "/nonexistent/auto-camera/clip.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: true,
            start_s: None,
            duration_s: None,
        };
        match open_source(&config, SourceId("f".into())) {
            Err(MediaError::Open(msg)) => assert!(msg.contains("clip.mp4"), "{msg}"),
            other => panic!(
                "expected an open error, got {:?}",
                other.map(|s| s.info().clone())
            ),
        }
    }

    #[test]
    fn a_segment_is_read_and_checked_before_the_file() {
        let json = r#"{"kind": "file", "path": "a.mp4", "start_s": 2, "duration_s": 0.5}"#;
        let c: SourceConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(
            c,
            SourceConfig::File {
                start_s: Some(2.0),
                duration_s: Some(0.5),
                ..
            }
        ));
        // A segment no file can have fails before the file is looked for.
        for (start_s, duration_s, key) in [
            (Some(-1.0), None, "start_s"),
            (None, Some(0.0), "duration_s"),
        ] {
            let config = SourceConfig::File {
                path: "/nonexistent/auto-camera/clip.mp4".into(),
                loop_playback: false,
                max_width: None,
                audio: true,
                start_s,
                duration_s,
            };
            match open_source(&config, SourceId("f".into())) {
                Err(MediaError::Open(msg)) => assert!(msg.contains(key), "{msg}"),
                other => panic!("expected an open error, got {:?}", other.is_ok()),
            }
        }
    }
}
