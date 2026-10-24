//! [`SidecarService`] and [`SidecarDetector`] against the fake sidecar:
//! the `Detector` contract, thresholds, extents, sharing one process,
//! errors as `VisionError`s, and shutdown. Every test checks that its
//! children were reaped.

#![cfg(unix)]

mod common;

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use ac_core::{Extent, QueryId, Rect};
use ac_sidecar::service::UNNAMED;
use ac_sidecar::{DetectorConfig, HostConfig, HostError, HostState, SidecarService};
use ac_vision::{Detector, PromptQuery, VisionError};

use common::{PROMPT, WINDOW, backing_off, eventually, exists, fake, frame};

/// Asserts `pid` was reaped (soon: procfs may lag a moment).
fn assert_reaped(pid: u32) {
    assert!(eventually(PROMPT, || !exists(pid)), "pid {pid} left behind");
}

/// The service's child pid.
fn pid_of(service: &SidecarService) -> u32 {
    service.status().pid.expect("a running child")
}

#[test]
fn detections_follow_the_detector_contract() {
    let service = SidecarService::start(fake("ok", &[])).unwrap();
    let config = DetectorConfig {
        thresholds: HashMap::from([
            (QueryId("pastor".into()), 0.3),
            (QueryId("drums".into()), 0.8),
        ]),
        ..DetectorConfig::default()
    };
    let mut detector = service.detector(config).unwrap();
    assert_eq!(detector.name(), "fake-sam");
    let pid = pid_of(&service);
    let queries = [
        PromptQuery::new("speaker", "a person speaking"),
        PromptQuery::new("guitar", "a guitar"),
        PromptQuery::new("pastor", "a person speaking"),
        PromptQuery::new("drums", "a drum kit"),
    ];
    let out = detector.detect(&frame(1920, 1080, 12), &queries).unwrap();
    // Phrases: "a person speaking" (0.9), "a guitar" (0.7), "a drum kit"
    // (0.5, below its 0.8 override, so the fake does not return it).
    let got: Vec<(&str, f32, Extent)> = out
        .iter()
        .map(|d| (d.query.0.as_str(), d.score, d.extent))
        .collect();
    assert_eq!(
        got,
        [
            ("speaker", 0.9, Extent::Person),
            ("pastor", 0.9, Extent::Person),
            ("guitar", 0.7, Extent::Object),
        ]
    );
    assert_eq!(out[0].bbox, Rect::new(0.05, 0.1, 0.15, 0.6));
    assert!(out.iter().all(|d| Rect::FULL.contains_rect(&d.bbox)));
    // Logged on stderr, which may lag the answer on stdout.
    assert!(
        eventually(PROMPT, || service
            .logs()
            .iter()
            .any(|l| l == "fake: detect id=1 1008x567 phrases=3")),
        "{:?}",
        service.logs()
    );
    let status = service.status();
    assert_eq!(status.state, HostState::Ready);
    assert_eq!((status.stats.requests, status.stats.results), (1, 1));
    drop(detector);
    drop(service);
    assert_reaped(pid);
}

#[test]
fn the_send_side_limits_the_image() {
    let service = SidecarService::start(fake("ok", &[])).unwrap();
    let config = DetectorConfig {
        send_side: 640,
        ..DetectorConfig::default()
    };
    let mut detector = service.detector(config).unwrap();
    let pid = pid_of(&service);
    let queries = [PromptQuery::new("a", "a person")];
    detector.detect(&frame(1280, 720, 1), &queries).unwrap();
    detector.detect(&frame(320, 180, 2), &queries).unwrap();
    // Logged on stderr, which may lag the answers on stdout.
    for size in [" 640x360 phrases=1", " 320x180 phrases=1"] {
        assert!(
            eventually(PROMPT, || service.logs().iter().any(|l| l.ends_with(size))),
            "{size}: {:?}",
            service.logs()
        );
    }
    drop(service);
    assert_reaped(pid);
}

#[test]
fn no_queries_means_no_request() {
    let service = SidecarService::start(fake("ok", &[])).unwrap();
    let mut detector = service.detector(DetectorConfig::default()).unwrap();
    let f = frame(64, 36, 0);
    assert!(detector.detect(&f, &[]).unwrap().is_empty());
    let blank = [PromptQuery::new("a", "  ")];
    assert!(detector.detect(&f, &blank).unwrap().is_empty());
    assert_eq!(service.status().stats.requests, 0);
}

#[test]
fn detectors_on_many_threads_share_one_process() {
    let service = SidecarService::start(fake("ok", &[])).unwrap();
    let pid = pid_of(&service);
    let workers: Vec<_> = (0..3)
        .map(|i| {
            let client = service.client();
            thread::spawn(move || {
                let mut detector = client.detector(DetectorConfig::default()).unwrap();
                let queries = [PromptQuery::new(&format!("q{i}"), "a person")];
                for seq in 0..5 {
                    let out = detector.detect(&frame(64, 36, seq), &queries).unwrap();
                    assert_eq!(out.len(), 1);
                    assert_eq!(out[0].query, QueryId(format!("q{i}")));
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    let stats = service.status().stats;
    assert_eq!((stats.spawns, stats.requests, stats.results), (1, 15, 15));
    assert_eq!(stats.stale, 0);
    drop(service);
    assert_reaped(pid);
}

#[test]
fn deadlines_and_restarts_reach_the_detector_as_vision_errors() {
    let service = SidecarService::start(backing_off("slow", &["--delay-ms", "400"])).unwrap();
    let first = pid_of(&service);
    let queries = [PromptQuery::new("a", "a person")];
    let f = frame(64, 36, 0);
    let mut hasty = service
        .detector(DetectorConfig {
            request_timeout: Duration::from_millis(100),
            ..DetectorConfig::default()
        })
        .unwrap();
    let mut patient = service.detector(DetectorConfig::default()).unwrap();
    let err = hasty.detect(&f, &queries).unwrap_err();
    assert!(matches!(err, VisionError::Timeout { .. }), "{err}");
    let err = patient.detect(&f, &queries).unwrap_err();
    assert!(
        matches!(&err, VisionError::Unavailable(m) if m.contains("retrying")),
        "{err}"
    );
    assert_reaped(first);
    thread::sleep(WINDOW);
    assert_eq!(patient.detect(&f, &queries).unwrap().len(), 1);
    let status = service.status();
    assert_eq!(status.stats.spawns, 2);
    let second = status.pid.unwrap();
    drop(service);
    assert_reaped(second);
}

#[test]
fn error_replies_are_model_errors() {
    let service = SidecarService::start(fake("error-reply", &[])).unwrap();
    let mut detector = service.detector(DetectorConfig::default()).unwrap();
    let err = detector
        .detect(&frame(8, 8, 0), &[PromptQuery::new("a", "a person")])
        .unwrap_err();
    assert!(
        matches!(&err, VisionError::Model(m) if m.contains("fake failure")),
        "{err}"
    );
}

#[test]
fn a_first_start_that_fails_is_retried_by_the_detector() {
    let marker = common::temp_path("fail-load-once");
    let _ = std::fs::remove_file(&marker);
    let service = SidecarService::start(backing_off(
        "fail-load-once",
        &["--marker", marker.to_str().unwrap()],
    ))
    .unwrap();
    // The start fails, but a detector is made: its passes go through the
    // host, which restarts the process after the backoff.
    let mut detector = service.detector(DetectorConfig::default()).unwrap();
    assert_eq!(detector.name(), UNNAMED);
    let queries = [PromptQuery::new("a", "a person")];
    let err = detector.detect(&frame(8, 8, 0), &queries).unwrap_err();
    assert!(
        matches!(&err, VisionError::Unavailable(m) if m.contains("exited before hello") && m.contains("retrying")),
        "{err}"
    );
    thread::sleep(WINDOW);
    assert_eq!(detector.detect(&frame(8, 8, 1), &queries).unwrap().len(), 1);
    let status = service.status();
    assert_eq!((status.stats.spawns, status.stats.results), (2, 1));
    let pid = pid_of(&service);
    drop(service);
    assert_reaped(pid);
    let _ = std::fs::remove_file(&marker);

    // A start that misses its deadline is no load failure either.
    let config = HostConfig {
        startup_timeout: Duration::from_millis(300),
        ..fake("never-ready", &[])
    };
    let service = SidecarService::start(config).unwrap();
    let pid = pid_of(&service);
    let detector = service.detector(DetectorConfig::default()).unwrap();
    assert_eq!(detector.name(), UNNAMED);
    assert_reaped(pid);
    assert!(matches!(
        service.status().state,
        HostState::Unavailable { .. }
    ));
}

#[test]
fn a_frame_that_waited_too_long_is_skipped_without_a_pass() {
    // Behind another source's 400 ms pass, a frame allowed 150 ms is
    // skipped; on its own it is sent.
    let service = SidecarService::start(fake("slow", &["--delay-ms", "400"])).unwrap();
    let config = DetectorConfig {
        max_age: Some(Duration::from_millis(150)),
        ..DetectorConfig::default()
    };
    let mut first = service.detector(config.clone()).unwrap();
    let mut second = service.detector(config).unwrap();
    let queries = [PromptQuery::new("a", "a person")];
    let busy = thread::spawn(move || first.detect(&frame(8, 8, 0), &queries).map(|d| d.len()));
    thread::sleep(Duration::from_millis(100));
    let queries = [PromptQuery::new("a", "a person")];
    let err = second.detect(&frame(8, 8, 0), &queries).unwrap_err();
    assert!(
        matches!(&err, VisionError::Unavailable(m) if m.contains("skipped") && m.contains("0.1 s a result may be old")),
        "{err}"
    );
    assert_eq!(busy.join().unwrap().unwrap(), 1);
    assert_eq!(second.detect(&frame(8, 8, 1), &queries).unwrap().len(), 1);
    let stats = service.status().stats;
    assert_eq!((stats.requests, stats.results, stats.expired), (2, 2, 1));
}

#[test]
fn a_frame_held_up_by_a_restart_is_not_sent() {
    // The second start takes 300 ms to say hello: the frame that started
    // it is older than its 100 ms by then, so it is skipped, and the next
    // one is answered.
    let marker = common::temp_path("restart-hello-delay");
    let _ = std::fs::remove_file(&marker);
    let service = SidecarService::start(fake(
        "crash-once",
        &[
            "--marker",
            marker.to_str().unwrap(),
            "--hello-delay-ms",
            "300",
        ],
    ))
    .unwrap();
    let mut detector = service
        .detector(DetectorConfig {
            max_age: Some(Duration::from_millis(100)),
            ..DetectorConfig::default()
        })
        .unwrap();
    let queries = [PromptQuery::new("a", "a person")];
    let crashed = detector.detect(&frame(8, 8, 0), &queries).unwrap_err();
    assert!(matches!(crashed, VisionError::Unavailable(_)), "{crashed}");
    thread::sleep(Duration::from_millis(80));
    let skipped = detector.detect(&frame(8, 8, 1), &queries).unwrap_err();
    assert!(skipped.to_string().contains("skipped"), "{skipped}");
    assert_eq!(detector.detect(&frame(8, 8, 2), &queries).unwrap().len(), 1);
    let stats = service.status().stats;
    assert_eq!(
        (stats.spawns, stats.requests, stats.results, stats.expired),
        (2, 2, 1, 1)
    );
    let _ = std::fs::remove_file(&marker);
}

#[test]
fn a_missing_program_fails_at_start() {
    let err =
        SidecarService::start(HostConfig::new(["/nonexistent/python", "server.py"])).unwrap_err();
    assert!(
        matches!(&err, HostError::Spawn(m) if m.contains("/nonexistent/python")),
        "{err}"
    );
    assert!(matches!(
        SidecarService::start(HostConfig::default()),
        Err(HostError::Config(_))
    ));
}

#[test]
fn invalid_detector_settings_are_refused() {
    let service = SidecarService::start(fake("ok", &[])).unwrap();
    let err = service
        .detector(DetectorConfig {
            threshold: 2.0,
            ..DetectorConfig::default()
        })
        .unwrap_err();
    assert!(matches!(err, VisionError::InvalidConfig(_)), "{err}");
}

#[test]
fn dropping_the_service_fails_its_clients() {
    let service = SidecarService::start(fake("ok", &[])).unwrap();
    let mut detector = service.detector(DetectorConfig::default()).unwrap();
    let client = service.client();
    let pid = pid_of(&service);
    service.shutdown();
    assert_reaped(pid);
    let err = detector
        .detect(&frame(8, 8, 0), &[PromptQuery::new("a", "a person")])
        .unwrap_err();
    assert!(matches!(err, VisionError::Unavailable(_)), "{err}");
    assert_eq!(client.ready(), Err(HostError::Stopped));
}

#[test]
fn dropping_the_service_interrupts_a_model_still_loading() {
    let config = HostConfig {
        startup_timeout: Duration::from_secs(60),
        ..fake("never-ready", &[])
    };
    let service = SidecarService::start(config).unwrap();
    let pid = pid_of(&service);
    let client = service.client();
    let loader = thread::spawn(move || client.detector(DetectorConfig::default()));
    thread::sleep(Duration::from_millis(150));
    let started = Instant::now();
    drop(service);
    assert!(started.elapsed() < PROMPT, "quitting waited for the load");
    let err = loader.join().unwrap().unwrap_err();
    assert!(matches!(err, VisionError::Unavailable(_)), "{err}");
    assert_reaped(pid);
}
