//! The Rust host against the real Python server,
//! `sidecars/sam3-mlx/server.py`, on its fake backend: the two
//! implementations of protocol v1 are checked against each other, through
//! [`SidecarService`], [`SidecarDetector`] and [`SamHost`] exactly as the
//! engine uses them. Nothing here imports MLX.
//!
//! The fake answers the k-th distinct phrase of a request with the box
//! `[0.05 + 0.1 * (k % 9), 0.2, 0.08, 0.6]` at score 0.9, and fails a
//! request that asks for `fake:error` (see `sam3_sidecar/fake.py`).
//!
//! Skipped (with a note on stderr) when `python3` is not on `PATH` or is
//! older than 3.10, the server's minimum. CI's Ubuntu and macOS runners
//! have it.

#![cfg(unix)]

mod common;

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use ac_core::{Extent, QueryId};
use ac_sidecar::{DetectorConfig, HostConfig, HostError, HostState, SamHost, SidecarService};
use ac_vision::{Detector, PromptQuery, VisionError};

use common::{PROMPT, eventually, exists, frame, request};

/// The server script, from this crate's directory.
const SERVER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sidecars/sam3-mlx/server.py"
);

/// True when `python3` runs and is 3.10 or newer; otherwise says why the
/// test is skipped.
fn have_python() -> bool {
    let ok = Command::new("python3")
        .args([
            "-c",
            "import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("skipped: python3 3.10 or newer is not on PATH");
    }
    ok
}

/// The server with `args`, isolated from the user's Python setup (`-I`)
/// and writing no bytecode into the repository (`-B`), with short
/// deadlines.
fn server(args: &[&str]) -> HostConfig {
    let mut command = vec![
        "python3".to_string(),
        "-I".into(),
        "-B".into(),
        SERVER.into(),
    ];
    command.extend(args.iter().map(|a| a.to_string()));
    HostConfig {
        startup_timeout: Duration::from_secs(20),
        exit_grace: Duration::from_secs(2),
        ..HostConfig::new(command)
    }
}

/// The fake backend.
fn fake() -> HostConfig {
    server(&["--backend", "fake", "--log-level", "debug"])
}

/// Asserts `pid` was reaped.
fn assert_reaped(pid: u32) {
    assert!(eventually(PROMPT, || !exists(pid)), "pid {pid} left behind");
}

/// Asserts that `got` is the fake's box for the `k`-th distinct phrase.
fn assert_fake_box(got: ac_core::Rect, k: u32) {
    let want = [0.05 + 0.1 * f64::from(k % 9), 0.2, 0.08, 0.6];
    let got = [got.x, got.y, got.w, got.h];
    assert!(
        got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-6),
        "phrase {k}: {got:?} vs {want:?}"
    );
}

#[test]
fn the_fake_backend_answers_through_the_rust_detector() {
    if !have_python() {
        return;
    }
    let service = SidecarService::start(fake()).unwrap();
    let mut detector = service.detector(DetectorConfig::default()).unwrap();
    assert_eq!(detector.name(), "fake");
    let status = service.status();
    let hello = status.hello.clone().expect("a hello");
    assert_eq!((hello.weights_sha256, hello.input_side), (None, 672));
    assert!(!hello.capabilities.masks && !hello.capabilities.parts);
    let pid = status.pid.unwrap();
    let queries = [
        PromptQuery::new("speaker", "a person speaking"),
        PromptQuery::new("guitar", "a guitar"),
        PromptQuery::new("pastor", "a person speaking"),
        PromptQuery::new("drums", "a drum kit"),
    ];
    // A full-HD frame goes out at 1008x567: the server checks that the
    // payload is width * height * 3 bytes.
    let out = detector.detect(&frame(1920, 1080, 3), &queries).unwrap();
    let got: Vec<(&str, f32, Extent)> = out
        .iter()
        .map(|d| (d.query.0.as_str(), d.score, d.extent))
        .collect();
    assert_eq!(
        got,
        [
            ("speaker", 0.9, Extent::Person),
            ("pastor", 0.9, Extent::Person),
            ("guitar", 0.9, Extent::Object),
            ("drums", 0.9, Extent::Object),
        ]
    );
    // Three distinct phrases, in first-seen order.
    for (d, k) in out.iter().zip([0, 0, 1, 2]) {
        assert_fake_box(d.bbox, k);
    }
    // A second detector on another thread shares the same process.
    let client = service.client();
    let other = std::thread::spawn(move || {
        let mut d = client.detector(DetectorConfig::default()).unwrap();
        d.detect(&frame(64, 36, 1), &[PromptQuery::new("g", "a guitar")])
    });
    let other = other.join().unwrap().unwrap();
    assert_eq!(other.len(), 1);
    assert_fake_box(other[0].bbox, 0);
    let status = service.status();
    assert_eq!(status.pid, Some(pid));
    assert_eq!(status.state, HostState::Ready);
    let stats = status.stats;
    assert_eq!((stats.spawns, stats.requests, stats.results), (1, 2, 2));
    assert_eq!(
        (stats.stale, stats.protocol_errors, stats.failures),
        (0, 0, 0)
    );
    assert!(
        status
            .last_latency
            .is_some_and(|l| l < Duration::from_secs(1))
    );
    drop(detector);
    drop(service);
    assert_reaped(pid);
}

#[test]
fn thresholds_include_the_boundary_on_both_sides() {
    if !have_python() {
        return;
    }
    let service = SidecarService::start(fake()).unwrap();
    let config = DetectorConfig {
        // The fake scores 0.9: kept at exactly 0.9 (the threshold crosses
        // JSON as an f32 and comes back as one), dropped above it.
        threshold: 0.9,
        thresholds: HashMap::from([(QueryId("strict".into()), 0.95)]),
        ..DetectorConfig::default()
    };
    let mut detector = service.detector(config).unwrap();
    let queries = [
        PromptQuery::new("boundary", "a person speaking"),
        PromptQuery::new("strict", "a guitar"),
    ];
    let out = detector.detect(&frame(160, 90, 1), &queries).unwrap();
    let ids: Vec<&str> = out.iter().map(|d| d.query.0.as_str()).collect();
    assert_eq!(ids, ["boundary"]);
    // Only a stricter query on a phrase: the server drops it itself.
    let out = detector
        .detect(
            &frame(160, 90, 2),
            &[PromptQuery::new("strict", "a guitar")],
        )
        .unwrap();
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn an_error_reply_fails_one_request_and_the_server_carries_on() {
    if !have_python() {
        return;
    }
    let service = SidecarService::start(fake()).unwrap();
    let mut detector = service.detector(DetectorConfig::default()).unwrap();
    let pid = service.status().pid.unwrap();
    let failing = [
        PromptQuery::new("ok", "a person"),
        PromptQuery::new("boom", "fake:error"),
    ];
    let err = detector.detect(&frame(64, 36, 1), &failing).unwrap_err();
    assert!(
        matches!(&err, VisionError::Model(m) if m.contains("fake backend was asked to fail")),
        "{err:?}"
    );
    let out = detector
        .detect(&frame(64, 36, 2), &[PromptQuery::new("ok", "a person")])
        .unwrap();
    assert_eq!(out.len(), 1);
    let status = service.status();
    assert_eq!(status.pid, Some(pid), "the same process");
    assert_eq!((status.stats.rejected, status.stats.failures), (1, 0));
}

#[test]
fn a_fatal_error_reaches_the_host_with_its_reason() {
    if !have_python() {
        return;
    }
    // A bad command line (exit 2, a side SAM 3.1 cannot run at included)
    // and a model folder that does not exist (exit 3) both send a fatal
    // error frame before exiting, without importing MLX or hashing weights.
    for (args, why) in [
        (&["--backend", "mlx"][..], "--model is required"),
        (
            &["--backend", "mlx", "--model", "/nonexistent/sam31-mlx"][..],
            "model folder /nonexistent/sam31-mlx does not exist",
        ),
        (
            &["--backend", "fake", "--side", "5"][..],
            "--side 5: side must be a multiple of 28",
        ),
        (
            &[
                "--backend",
                "mlx",
                "--model",
                "/nonexistent/m",
                "--side",
                "1000",
            ][..],
            "--side 1000: side must be a multiple of 28",
        ),
    ] {
        let service = SidecarService::start(server(args)).unwrap();
        let pid = service.status().pid.unwrap();
        // The first start fails; the detector would retry it, so ask the
        // client for the start's own error.
        let err = service.client().ready().unwrap_err();
        assert!(
            matches!(&err, HostError::Unavailable { reason, .. } if reason.contains("the sidecar failed") && reason.contains(why)),
            "{args:?}: {err:?}"
        );
        assert_reaped(pid);
    }
}

#[test]
fn closing_stdin_makes_the_server_exit_cleanly() {
    if !have_python() {
        return;
    }
    let mut host = SamHost::new(fake()).unwrap();
    let reply = host
        .detect(
            request(1, &["a person", "a person"], 0.5),
            Duration::from_secs(5),
        )
        .unwrap();
    let phrases: Vec<u32> = reply.detections.iter().map(|f| f.phrase).collect();
    assert_eq!(
        phrases,
        [0, 1],
        "a repeated text is answered for each index"
    );
    let pid = host.pid().unwrap();
    host.shutdown();
    assert_reaped(pid);
    let logs = host.logs();
    assert!(
        logs.iter()
            .any(|l| l.contains("exited with exit status: 0 at shutdown")),
        "{logs:?}"
    );
    assert!(logs.iter().any(|l| l.contains("stdin closed")), "{logs:?}");
    // The host can start it again after a clean shutdown.
    assert!(matches!(host.ready(), Ok(h) if h.model == "fake"));
    let again = host.pid().unwrap();
    assert_ne!(again, pid);
    drop(host);
    assert_reaped(again);
}
