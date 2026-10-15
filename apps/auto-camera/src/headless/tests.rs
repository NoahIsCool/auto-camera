//! Headless runs on small synthetic studios (real time, about a second each).

use super::*;
use ac_core::ShotSize;
use ac_engine::{DetectorSettings, PresetSpec, QuerySettings};
use ac_sources::SourceConfig;

/// A small synthetic studio with ground-truth discovery and no director.
fn studio() -> StudioConfig {
    let mut c = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 2,
        },
    );
    c.detector = DetectorSettings::Synthetic { score: 0.9 };
    c.discovery.rate_hz = 10.0;
    c.queries.push(QuerySettings {
        id: None,
        name: "Person".into(),
        prompt: "person".into(),
        presets: vec![PresetSpec::Size(ShotSize::Medium)],
    });
    c.output.width = 320;
    c.output.height = 180;
    // Not a test of the encoder: libx264 behaves the same everywhere.
    c.output.encoder = ac_engine::EncoderSetting::Libx264;
    c.director.mode = DirectorMode::Off;
    c
}

/// Run options.
fn options(seconds: f64, record: Option<PathBuf>) -> HeadlessOptions {
    HeadlessOptions { seconds, record }
}

/// True when `tool -version` succeeds. When it does not and
/// `AC_REQUIRE_FFMPEG` is set (CI), panics instead of letting the caller
/// skip.
fn have(tool: &str) -> bool {
    let found = std::process::Command::new(tool)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let required = std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
    assert!(
        found || !required,
        "{tool} is not installed, and AC_REQUIRE_FFMPEG says it must be"
    );
    found
}

/// A fresh, unique path in the temp directory (not created).
fn temp_path(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("auto-camera-{}-{nanos}-{name}", std::process::id()))
}

#[test]
fn a_run_without_a_director_puts_the_wide_on_program() {
    let s = run(studio(), Path::new("studio.toml"), &options(0.6, None));
    assert!(s.ok, "{:?}", s.error);
    assert!(s.ticks > 10);
    assert_eq!(s.program.last.as_deref(), Some("stage/wide"));
    assert_eq!(s.program.log[0].1, "stage/wide");
    assert!(s.shots.iter().any(|v| v.id == "stage/wide"));
    assert_eq!(s.max_subjects, 2);
    assert_eq!(s.sources[0].size, [320, 180]);
    assert!(s.sources[0].discovery_applied > 0);
    assert!(s.output.is_none());
    assert_eq!(s.threads_joined, 1, "the discovery worker");
    assert_eq!(s.director.mode, DirectorMode::Off);
    assert!((s.timing.budget_ms - 33.33).abs() < 0.01);
}

#[test]
fn a_run_with_the_auto_director_switches_by_itself() {
    let mut c = studio();
    c.director.mode = DirectorMode::Auto;
    // The establishing take mixes up from black over 0.8 s.
    let s = run(c, Path::new("studio.toml"), &options(1.5, None));
    assert!(s.ok, "{:?}", s.error);
    assert!(s.program.changes >= 1);
    assert!(!s.program.log.is_empty());
    assert!(s.director.takes >= 1);
}

#[test]
fn unusable_setups_fail_with_a_summary() {
    let missing = StudioConfig::with_source(
        "f",
        SourceConfig::File {
            path: "/nonexistent/auto-camera/clip.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: false,
        },
    );
    let s = run(missing, Path::new("x.toml"), &options(1.0, None));
    assert!(!s.ok);
    assert!(s.error.unwrap().contains("did not start"));
    assert!(s.sources.is_empty());

    let existing = temp_path("existing.mp4");
    std::fs::write(&existing, b"keep me").unwrap();
    let s = run(
        studio(),
        Path::new("x.toml"),
        &options(1.0, Some(existing.clone())),
    );
    assert!(!s.ok);
    assert!(
        s.error.as_deref().unwrap().contains("already exists"),
        "{:?}",
        s.error
    );
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
    std::fs::remove_file(existing).unwrap();
}

/// Real time, except that the engine thread's clock reading number `fuse`
/// panics: a stand-in for any bug inside a tick.
#[derive(Debug)]
struct Grenade {
    /// The instant that reads as zero.
    origin: Instant,
    /// Readings taken on the engine thread so far.
    calls: std::sync::atomic::AtomicU64,
    /// Reading that panics.
    fuse: u64,
}

impl ac_sources::Clock for Grenade {
    fn now(&self) -> Duration {
        if std::thread::current().name() == Some("ac-engine")
            && self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == self.fuse
        {
            panic!("injected failure inside a tick");
        }
        self.origin.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[test]
fn a_run_whose_engine_dies_fails_at_once_with_the_reason() {
    let clock = Arc::new(Grenade {
        origin: Instant::now(),
        calls: Default::default(),
        fuse: 200,
    });
    let started = Instant::now();
    let s = run_with(
        studio(),
        Path::new("x.toml"),
        &options(30.0, None),
        EngineOptions {
            clock,
            ..EngineOptions::default()
        },
    );
    assert!(!s.ok);
    let error = s.error.as_deref().unwrap_or_default();
    assert!(error.contains("injected failure"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the run waited out its time: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_recording_run_finalizes_the_file() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let path = temp_path("headless.mp4");
    let s = run(
        studio(),
        Path::new("x.toml"),
        &options(1.0, Some(path.clone())),
    );
    assert!(s.ok, "{:?}", s.error);
    let out = s.output.as_ref().expect("output summary");
    assert_eq!(out.state, "finished");
    assert_eq!(out.exit_code, Some(0));
    assert!(out.frames_written >= 20, "{out:?}");
    assert_eq!(out.record_path.as_deref(), Some(path.as_path()));
    assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 0));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn the_summary_is_json() {
    let s = run(studio(), Path::new("studio.toml"), &options(0.2, None));
    let v: serde_json::Value = serde_json::to_value(&s).unwrap();
    for key in [
        "ok", "ticks", "sources", "shots", "program", "director", "timing", "output",
    ] {
        assert!(v.get(key).is_some(), "{key}");
    }
    assert_eq!(v["director"]["mode"], "off");
    assert_eq!(v["shots"][0]["status"], "wide");
    assert!(v["output"].is_null());
}

#[test]
fn rounding_keeps_the_requested_digits() {
    assert_eq!(round(1.23456, 2), 1.23);
    assert_eq!(round(-0.005, 2), -0.01);
    assert_eq!(round(7.0, 0), 7.0);
}

#[test]
fn the_sample_configs_load() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let demo = StudioConfig::load(&root.join("demo.toml")).unwrap();
    assert_eq!(demo.sources.len(), 1);
    assert_eq!(demo.detector, DetectorSettings::Synthetic { score: 0.9 });
    // Auto-take is opt-in: the demo only suggests.
    assert_eq!(demo.director.mode, DirectorMode::Suggest);
    assert_eq!(demo.discovery.rate_hz, 10.0);
    let file = StudioConfig::load(&root.join("file-source.example.toml")).unwrap();
    assert_eq!(file.queries.len(), 2);
    let SourceConfig::File { path, .. } = &file.sources[0].input else {
        panic!("a file source");
    };
    assert!(path.is_absolute() && path.ends_with("media/rehearsal.mp4"));
}
