//! Nothing is left behind: after every fake mode has run through
//! [`SidecarService`], [`SidecarDetector`] and [`SamHost`] and they are
//! dropped, this process has no child process (zombies included) and no
//! `ac-sam-*` thread. Threads are listed from `/proc/self/task`, so they
//! are checked on Linux only; children are checked on every Unix (procfs
//! on Linux, `pgrep -P` elsewhere, which covers macOS), as in
//! `ac-engine`'s shutdown test.
//!
//! This binary holds a single test so no other test's children or threads
//! are counted.

#![cfg(unix)]

mod common;

use std::thread;
use std::time::Duration;

use ac_sidecar::{DetectorConfig, HostConfig, SamHost, SidecarService};
use ac_vision::{Detector, PromptQuery};

use common::{children, fake, frame, request, settle};

/// Every mode of the fake, with its extra arguments.
const MODES: &[(&str, &[&str])] = &[
    ("ok", &[]),
    ("slow", &["--delay-ms", "300"]),
    ("hang", &[]),
    ("hang", &["--grandchild"]),
    ("crash-after-hello", &[]),
    ("crash-before-hello", &[]),
    ("garbage", &[]),
    ("wrong-seq", &[]),
    ("never-ready", &[]),
    ("error-reply", &[]),
    ("fatal-error", &[]),
];

/// Runs `mode` through a service and two detectors, twice each (so failing
/// modes restart), and drops everything.
fn through_the_service(mode: &str, extra: &[&str]) {
    let config = HostConfig {
        startup_timeout: Duration::from_millis(500),
        ..fake(mode, extra)
    };
    let service = SidecarService::start(config).unwrap();
    let detector_config = DetectorConfig {
        request_timeout: Duration::from_millis(200),
        ..DetectorConfig::default()
    };
    let queries = [PromptQuery::new("speaker", "a person speaking")];
    for round in 0..2 {
        let detectors: Vec<_> = (0..2)
            .filter_map(|_| service.detector(detector_config.clone()).ok())
            .collect();
        for mut d in detectors {
            let _ = d.detect(&frame(160, 90, round), &queries);
        }
        thread::sleep(Duration::from_millis(80));
    }
    let stats = service.status().stats;
    assert_eq!(stats.detached_threads, 0, "{mode}");
}

/// Runs `mode` through a bare host: a request, a restart, a shutdown in
/// the middle of nothing.
fn through_the_host(mode: &str, extra: &[&str]) {
    let config = HostConfig {
        startup_timeout: Duration::from_millis(500),
        ..fake(mode, extra)
    };
    let mut host = SamHost::new(config).unwrap();
    let _ = host.detect(request(1, &["a person"], 0.5), Duration::from_millis(200));
    thread::sleep(Duration::from_millis(80));
    let _ = host.detect(request(2, &["a person"], 0.5), Duration::from_millis(200));
    host.shutdown();
    let _ = host.start();
    assert_eq!(host.status().stats.detached_threads, 0, "{mode}");
}

#[test]
fn every_mode_leaves_no_child_and_no_thread() {
    for (mode, extra) in MODES {
        through_the_service(mode, extra);
        through_the_host(mode, extra);
    }
    assert_eq!(settle(children), Vec::<String>::new());
    #[cfg(target_os = "linux")]
    assert_eq!(settle(common::sidecar_threads), Vec::<String>::new());
}
