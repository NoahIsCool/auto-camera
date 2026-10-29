//! The runner over a decoded clip: a segment of a file, read frame by
//! frame on simulated time and answered from a detection log, so no model
//! runs.
//!
//! The fixture is made from ffmpeg's `lavfi` generators (4 s at 10 fps),
//! and the studio plays its segment from 2 s for 1 s. Its detector is OWL
//! with model files that do not exist: a replay never loads the detector,
//! so a studio set up for a model replays without one. The test skips
//! (passes with a note) when ffmpeg is not installed, unless
//! `AC_REQUIRE_FFMPEG` is set (CI), which makes a missing tool a failure.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ac_core::{Extent, QueryId, Rect, SourceId};
use ac_engine::config::StudioConfig;
use ac_eval::trace::parse_trace;
use ac_sources::{FfmpegTools, VirtualClock};
use ac_vision::{Detection, PassRecord};
use eval_runner::{Detections, RunOptions, Runner};

/// The tools, or `None` (and a note) when ffmpeg is not installed; panics
/// instead when `AC_REQUIRE_FFMPEG` is set.
fn tools() -> Option<FfmpegTools> {
    let tools = FfmpegTools::from_env();
    if tools.is_available() {
        Some(tools)
    } else {
        let required =
            std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
        assert!(!required, "ffmpeg/ffprobe are required (AC_REQUIRE_FFMPEG)");
        eprintln!("skipping: ffmpeg/ffprobe not found");
        None
    }
}

/// A temporary directory removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    /// A fresh directory unique to this process and `name`.
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("ac-eval-runner-clip-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a 4 s, 10 fps, 64x36 test clip into `dir`.
fn make_clip(tools: &FfmpegTools, dir: &Path) -> PathBuf {
    let path = dir.join("clip.mkv");
    let status = Command::new(&tools.ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=s=64x36:r=10:d=4"])
        .args(["-c:v", "mpeg4", "-q:v", "2"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make the fixture");
    path
}

/// The studio playing `clip` from 2 s for 1 s, with an OWL detector whose
/// files do not exist, one query, and a 10 fps program.
fn studio(dir: &Path) -> StudioConfig {
    let text = r#"
        [[sources]]
        id = "clip"
        kind = "file"
        path = "clip.mkv"
        start_s = 2.0
        duration_s = 1.0

        [[queries]]
        name = "Person"
        prompt = "person"

        [detector]
        kind = "owl"
        model = "absent/detector.onnx"
        tokenizer = "absent/tokenizer.json"

        [discovery]
        rate_hz = 10.0

        [output]
        width = 320
        height = 180
        fps = 10
    "#;
    let path = dir.join("studio.toml");
    std::fs::write(&path, text).unwrap();
    StudioConfig::load(&path).unwrap()
}

/// A recorded pass on frame `seq` of the segment (at `seq / 10` s) that
/// found one person at `x`.
fn pass(seq: u64, x: f64) -> PassRecord {
    let q = QueryId("person".into());
    PassRecord::new(
        SourceId("clip".into()),
        seq,
        Duration::from_millis(100 * seq),
        vec![q.clone()],
        vec![Detection {
            query: q,
            bbox: Rect::new(x, 0.2, 0.3, 0.6),
            score: 0.8,
            extent: Extent::Person,
        }],
    )
}

#[test]
fn a_segment_of_a_file_replays_until_it_ends() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("segment");
    make_clip(&tools, &dir.0);
    let config = studio(&dir.0);
    let log = vec![pass(0, 0.30), pass(3, 0.32), pass(6, 0.34)];
    let replay = Detections::Replay {
        log,
        tolerance: None,
    };
    // No length: the segment ends by itself.
    let runner = Runner::new(&config, &replay, &RunOptions::default()).unwrap();
    let mut trace = Vec::new();
    let report = runner.run(&VirtualClock::new(), &mut trace, None).unwrap();
    // One second at 10 fps, from PTS 0, then the clip has ended.
    assert_eq!(report.ticks, 10);
    let records = parse_trace(std::str::from_utf8(&trace).unwrap()).unwrap();
    for (n, r) in (0u64..).zip(&records) {
        let frame = r.sources[0].frame.unwrap();
        assert_eq!((frame.seq, frame.pts), (n, Duration::from_millis(100 * n)));
    }
    // Asked about every frame, answered from the three it has.
    let s = &report.sources[0];
    assert_eq!((s.requests, s.misses, s.errors), (10, 7, 0));
    assert_eq!((s.applied, s.recorded), (3, Some(3)));
    // The person was found on the first frame and is followed after.
    assert!(records[0].sources[0].subjects.is_empty());
    let subject = &records[1].sources[0].subjects[0];
    assert_eq!(subject.query, QueryId("person".into()));
    assert!((subject.bbox.x - 0.30).abs() < 0.05, "{:?}", subject.bbox);
    assert_eq!(records[9].sources[0].subjects.len(), 1);
}

#[test]
fn a_live_run_needs_a_detector_it_can_load() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("live");
    make_clip(&tools, &dir.0);
    let config = studio(&dir.0);
    // Without its model (and, in most builds, without ONNX) the studio's
    // detector cannot be loaded, which a live run reports up front.
    let err = Runner::new(&config, &Detections::Live, &RunOptions::default()).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("source \"clip\"")
            && (message.contains("onnx") || message.contains("not found")),
        "{message}"
    );
}
