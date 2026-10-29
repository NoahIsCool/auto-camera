//! Shared helpers for the engine's integration tests, and the harness of
//! the one-test binaries that are also the fake sidecar ([`one_test`]).

#![allow(dead_code)]

#[cfg(unix)]
pub mod one_test;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::ShotSize;
use ac_director::DirectorMode;
use ac_engine::{
    DetectorSettings, EncoderSetting, Engine, EngineOptions, MonitorSettings, PresetSpec,
    QuerySettings, StudioConfig,
};
use ac_sources::{Clock, SourceConfig};

/// Generous real-time bound for conditions in virtual-clock runs.
pub const WAIT: Duration = Duration::from_secs(60);

/// A small synthetic studio with ground-truth discovery and no director.
pub fn studio(subjects: u32) -> StudioConfig {
    let mut c = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects,
        },
    );
    c.detector = DetectorSettings::Synthetic { score: 0.9 };
    c.discovery.rate_hz = 10.0;
    c.queries.push(QuerySettings {
        id: None,
        name: "Person".into(),
        prompt: "person".into(),
        presets: Some(vec![
            PresetSpec::Size(ShotSize::Medium),
            PresetSpec::Size(ShotSize::Close),
        ]),
        literal: false,
        priority: None,
    });
    c.output.width = 320;
    c.output.height = 180;
    // These tests are not about the encoder: libx264 behaves the same on
    // every machine, where VideoToolbox depends on the Mac it runs on.
    c.output.encoder = EncoderSetting::Libx264;
    c.director.mode = DirectorMode::Off;
    c.monitors = MonitorSettings {
        width: 160,
        height: 90,
        rate_hz: 30.0,
        thumb_width: 64,
        thumb_height: 36,
        thumb_rate_hz: 10.0,
    };
    c
}

/// How much faster than real time [`FastClock`] runs.
pub const SPEED: u32 = 4;

/// Real time sped up by [`SPEED`].
///
/// A fully virtual clock would let the engine run unpaced while the discovery
/// workers still run in real time, so every detection would arrive "seconds"
/// late in media time and be dropped as too old. Scaling real time keeps the
/// workers fast relative to the media clock while the tests stay quick.
#[derive(Debug)]
pub struct FastClock {
    /// The instant that reads as zero.
    origin: Instant,
}

impl Clock for FastClock {
    fn now(&self) -> Duration {
        self.origin.elapsed() * SPEED
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration / SPEED);
    }
}

/// Options with a fresh [`FastClock`].
pub fn fast_time() -> EngineOptions {
    EngineOptions {
        clock: Arc::new(FastClock {
            origin: Instant::now(),
        }),
        ..EngineOptions::default()
    }
}

/// Starts `config` on a [`FastClock`].
pub fn start(config: StudioConfig) -> Engine {
    Engine::start_with(config, fast_time()).expect("engine starts")
}

/// Set (to anything but `0`) to make tests that need ffmpeg fail when it is
/// missing instead of skipping; CI sets it.
pub const REQUIRE_FFMPEG: &str = "AC_REQUIRE_FFMPEG";

/// True when `tool` (ffmpeg or ffprobe) answers `-version`. When it does
/// not and [`REQUIRE_FFMPEG`] is set, panics instead of letting the caller
/// skip.
pub fn have(tool: &str) -> bool {
    let found = std::process::Command::new(tool)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let required = std::env::var_os(REQUIRE_FFMPEG).is_some_and(|v| !v.is_empty() && v != "0");
    assert!(
        found || !required,
        "{tool} is not installed, and {REQUIRE_FFMPEG} says it must be"
    );
    found
}

/// A fresh, unique path in the temp directory (not created).
pub fn temp_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("ac-engine-{}-{nanos}-{name}", std::process::id()))
}
