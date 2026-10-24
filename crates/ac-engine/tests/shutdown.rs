//! Shutdown leaves nothing behind: no engine, source, discovery or output
//! thread and no ffmpeg child. Threads are listed from `/proc/self/task`, so
//! they are checked on Linux only; children are checked on every Unix
//! (procfs on Linux, `pgrep -P` elsewhere, which covers macOS).
//!
//! This binary holds a single test so no other test's threads are counted.
//! Rayon's global pool is process-wide and persistent by design; it is named
//! apart and not counted.

#![cfg(unix)]

mod common;

use std::time::{Duration, Instant};

use ac_core::ShotId;
use ac_engine::{EngineCommand, OutputState};

use common::{WAIT, have, start, studio, temp_path};

/// Threads can be listed by name on this platform.
const LISTS_THREADS: bool = cfg!(target_os = "linux");

/// Names of this process's threads that belong to the pipeline (Linux only;
/// see [`LISTS_THREADS`]).
fn pipeline_threads() -> Vec<String> {
    let mut names = Vec::new();
    for task in std::fs::read_dir("/proc/self/task").expect("procfs") {
        let comm = task.expect("task").path().join("comm");
        if let Ok(name) = std::fs::read_to_string(comm)
            && name.starts_with("ac-")
        {
            names.push(name.trim().to_owned());
        }
    }
    names
}

/// Child process ids of every thread of this process.
#[cfg(target_os = "linux")]
fn children() -> Vec<String> {
    let mut pids = Vec::new();
    for task in std::fs::read_dir("/proc/self/task").expect("procfs") {
        let path = task.expect("task").path().join("children");
        if let Ok(list) = std::fs::read_to_string(path) {
            pids.extend(list.split_whitespace().map(str::to_owned));
        }
    }
    pids
}

/// Child process ids of this process, as `pgrep -P` lists them.
#[cfg(not(target_os = "linux"))]
fn children() -> Vec<String> {
    let out = std::process::Command::new("pgrep")
        .arg("-P")
        .arg(std::process::id().to_string())
        .output()
        .expect("pgrep runs");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Polls `f` until it returns an empty list or two seconds pass: a joined
/// thread can linger in procfs for a moment after `join` returns.
fn settle(f: fn() -> Vec<String>) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let left = f();
        if left.is_empty() || Instant::now() > deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn shutdown_joins_every_thread_and_reaps_ffmpeg() {
    // The tracker and compositor share rayon's global pool, which lives for
    // the whole process by design. Its threads would otherwise inherit the
    // name of the thread that first used it ("ac-engine"), so name them.
    rayon::ThreadPoolBuilder::new()
        .thread_name(|i| format!("rayon-{i}"))
        .build_global()
        .expect("the pool is not built yet");
    let engine = start(studio(2));
    let h = engine.handle();
    h.wait_until(WAIT, |s| s.subjects.len() == 2)
        .expect("subjects");
    let recording = have("ffmpeg").then(|| temp_path("shutdown.mp4"));
    if let Some(path) = &recording {
        h.send(EngineCommand::SetPreview {
            shot: ShotId("stage/wide".into()),
        })
        .unwrap();
        h.send(EngineCommand::Cut).unwrap();
        h.send(EngineCommand::StartOutput {
            rtmp_url: None,
            record_path: Some(path.clone()),
        })
        .unwrap();
        h.wait_until(WAIT, |s| s.output.state == OutputState::Running)
            .expect("encoder running");
        assert!(!children().is_empty(), "ffmpeg is a child of this process");
    }
    if LISTS_THREADS {
        let threads = pipeline_threads();
        assert!(threads.iter().any(|n| n == "ac-engine"), "{threads:?}");
        assert!(
            threads.iter().any(|n| n.starts_with("ac-discovery")),
            "{threads:?}"
        );
    }

    // Shut down with the encoder still running: shutdown finishes it.
    let report = engine.shutdown().unwrap();
    assert!(!report.final_snapshot.running);
    if LISTS_THREADS {
        assert_eq!(settle(pipeline_threads), Vec::<String>::new());
    }
    assert_eq!(settle(children), Vec::<String>::new());
    if let Some(path) = recording {
        assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 0));
        let _ = std::fs::remove_file(path);
    }
}
