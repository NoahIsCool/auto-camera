//! Helpers shared by the integration tests: the fake sidecar's command,
//! frames and requests, and process checks.
//!
//! Children are checked on every Unix (procfs on Linux, `pgrep -P`
//! elsewhere, which covers macOS), as in `ac-engine`'s shutdown test.

#![allow(dead_code)]

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::VideoFrame;
use ac_sidecar::{DetectRequest, HostConfig, Phrase, RgbImage};

/// The fake sidecar built with this crate.
pub const FAKE: &str = env!("CARGO_BIN_EXE_ac-fake-sidecar");

/// Generous bound for things that should happen at once.
pub const PROMPT: Duration = Duration::from_secs(5);

/// A host config running the fake in `mode` with `extra` arguments, with
/// short deadlines and backoff so tests run quickly.
pub fn fake(mode: &str, extra: &[&str]) -> HostConfig {
    let mut command = vec![FAKE.to_string(), mode.to_string()];
    command.extend(extra.iter().map(|s| s.to_string()));
    HostConfig {
        startup_timeout: Duration::from_secs(5),
        exit_grace: Duration::from_millis(300),
        restart_backoff: Duration::from_millis(50),
        max_restart_backoff: Duration::from_millis(200),
        ..HostConfig::new(command)
    }
}

/// A restart backoff long enough that a check that a request is refused
/// while backing off cannot miss the window. `fake`'s 50 ms is not: on a
/// loaded macOS runner, reaping the old child and checking it was gone
/// took longer than that, and the "refused" request started a new one.
pub const WINDOW: Duration = Duration::from_secs(1);

/// [`fake`] with a [`WINDOW`] restart backoff, doubling up to four times it,
/// for tests that check refusals during the backoff. Sleeping for `WINDOW`
/// after such a refusal is always past the backoff, as it began earlier.
pub fn backing_off(mode: &str, extra: &[&str]) -> HostConfig {
    HostConfig {
        restart_backoff: WINDOW,
        max_restart_backoff: 4 * WINDOW,
        ..fake(mode, extra)
    }
}

/// A `w` by `h` RGBA frame with a gradient, from source "cam".
pub fn frame(w: u32, h: u32, seq: u64) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128, 255]);
        }
    }
    VideoFrame::new(
        SourceId("cam".into()),
        seq,
        Duration::from_millis(33 * seq),
        w,
        h,
        Arc::from(data),
    )
    .unwrap()
}

/// A request for `phrases` (each at `threshold`) on a small image.
pub fn request(seq: u64, phrases: &[&str], threshold: f32) -> DetectRequest {
    DetectRequest {
        source: "cam".into(),
        seq,
        pts: Duration::from_millis(33 * seq),
        image: RgbImage {
            width: 4,
            height: 2,
            data: vec![7; 4 * 2 * 3],
        },
        phrases: phrases
            .iter()
            .map(|t| Phrase {
                text: (*t).into(),
                threshold,
            })
            .collect(),
        freshness: None,
    }
}

/// True while `pid` exists, zombies included (an unreaped child exists).
pub fn exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// True while `pid` is running: exists and is not a zombie. For processes
/// that are not this process's children (their reaping is not ours).
pub fn running(pid: u32) -> bool {
    let Ok(out) = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    out.status.success() && !stat.is_empty() && !stat.starts_with('Z')
}

/// Polls `f` until it is true or `limit` passes; returns its last value.
pub fn eventually(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if f() {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Child process ids of every thread of this process.
#[cfg(target_os = "linux")]
pub fn children() -> Vec<String> {
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
pub fn children() -> Vec<String> {
    let out = Command::new("pgrep")
        .arg("-P")
        .arg(std::process::id().to_string())
        .output()
        .expect("pgrep runs");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Names of this process's sidecar threads (`ac-sam-*`); Linux only.
#[cfg(target_os = "linux")]
pub fn sidecar_threads() -> Vec<String> {
    let mut names = Vec::new();
    for task in std::fs::read_dir("/proc/self/task").expect("procfs") {
        let comm = task.expect("task").path().join("comm");
        if let Ok(name) = std::fs::read_to_string(comm)
            && name.starts_with("ac-sam")
        {
            names.push(name.trim().to_owned());
        }
    }
    names
}

/// Polls `f` until it returns an empty list or two seconds pass: a reaped
/// child or joined thread can linger in procfs for a moment.
pub fn settle(f: fn() -> Vec<String>) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let left = f();
        if left.is_empty() || Instant::now() > deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The pid in the fake's "fake: grandchild N" log line, if logged yet.
pub fn grandchild_pid(logs: &[String]) -> Option<u32> {
    logs.iter()
        .find_map(|l| l.strip_prefix("fake: grandchild "))
        .and_then(|p| p.trim().parse().ok())
}

/// A path in the temporary directory unique to this process and `name`,
/// for marker files; nothing is created.
pub fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ac-sidecar-{name}-{}", std::process::id()))
}
