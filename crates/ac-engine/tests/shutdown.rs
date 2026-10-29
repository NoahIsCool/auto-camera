//! Shutdown leaves nothing behind: no engine, source, discovery, sidecar or
//! output thread, and no child process (ffmpeg, or the detector sidecar).
//! Threads are listed from `/proc/self/task`, so they are checked on Linux
//! only; children are checked on every Unix (procfs on Linux, `pgrep -P`
//! elsewhere, which covers macOS).
//!
//! Two studios run in turn: a synthetic one with ground-truth discovery,
//! recording with ffmpeg (when installed), and one whose detector is a
//! sidecar process (`kind = "sidecar"`, ADR 0004) shut down while the
//! sidecar hangs in a pass, which must not hold the quit up for the
//! request deadline.
//!
//! This binary runs without libtest's harness, so it holds a single test
//! (no other test's threads or children are counted) and is also the fake
//! sidecar of the second studio; see `common::one_test`.
//!
//! Rayon's global pool is process-wide and persistent by design; it is
//! named apart and not counted.

#[cfg(unix)]
mod common;

/// The one test's name, for filters and `--list`.
#[cfg(unix)]
const NAME: &str = "shutdown_joins_every_thread_and_reaps_every_child";

/// Runs the test, or the fake sidecar; see `common::one_test`.
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    common::one_test::run(
        NAME,
        unix::shutdown_joins_every_thread_and_reaps_every_child,
    )
}

/// Nothing to check where processes are not listed this way.
#[cfg(not(unix))]
fn main() {}

/// The test.
#[cfg(unix)]
mod unix {
    use std::time::{Duration, Instant};

    use ac_core::ShotId;
    use ac_engine::{DetectorSettings, EngineCommand, OutputState, SidecarSettings};

    use crate::common::one_test::FAKE_FLAG;
    use crate::common::{WAIT, have, start, studio, temp_path};

    /// Threads can be listed by name on this platform.
    const LISTS_THREADS: bool = cfg!(target_os = "linux");

    /// Names of this process's threads that belong to the pipeline (Linux
    /// only; see [`LISTS_THREADS`]).
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

    /// Polls `f` until it returns an empty list or two seconds pass: a
    /// joined thread or reaped child can linger in procfs for a moment.
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

    /// Asserts that nothing of an engine is left.
    fn assert_nothing_left(what: &str) {
        if LISTS_THREADS {
            assert_eq!(settle(pipeline_threads), Vec::<String>::new(), "{what}");
        }
        assert_eq!(settle(children), Vec::<String>::new(), "{what}");
    }

    /// The test; see the crate docs.
    pub fn shutdown_joins_every_thread_and_reaps_every_child() {
        // The tracker and compositor share rayon's global pool, which lives
        // for the whole process by design. Its threads would otherwise
        // inherit the name of the thread that first used it ("ac-engine"),
        // so name them.
        rayon::ThreadPoolBuilder::new()
            .thread_name(|i| format!("rayon-{i}"))
            .build_global()
            .expect("the pool is not built yet");
        ground_truth_studio_with_ffmpeg();
        assert_nothing_left("after the recording studio");
        sidecar_studio();
        assert_nothing_left("after the sidecar studio");
    }

    /// The synthetic studio, recording when ffmpeg is installed.
    fn ground_truth_studio_with_ffmpeg() {
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
            h.send(EngineCommand::Cut {
                preview: Some(ShotId("stage/wide".into())),
            })
            .unwrap();
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
        if let Some(path) = recording {
            assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 0));
            let _ = std::fs::remove_file(path);
        }
    }

    /// How many requests the fake answers before it hangs: two per source.
    const ANSWERED: u32 = 4;

    /// Two sources whose detector is this binary as a fake sidecar: one
    /// process for both, taking turns. Once each source has applied two
    /// passes the fake hangs in the middle of the next one and ignores the
    /// end of its input, so shutdown must interrupt the pass and kill the
    /// process after its 2 s grace rather than wait out the 30 s request
    /// deadline (the engine drops the sidecar before it joins the workers).
    fn sidecar_studio() {
        let mut config = studio(1);
        let second = config.sources[0].clone();
        config.sources.push(ac_engine::SourceEntry {
            id: "stage-2".into(),
            ..second
        });
        let me = std::env::current_exe().expect("this test binary");
        config.detector = DetectorSettings::Sidecar(SidecarSettings {
            request_timeout_s: 30.0,
            ..SidecarSettings::new([
                me.to_str().expect("a UTF-8 path").to_owned(),
                FAKE_FLAG.to_owned(),
                "--hang-after".into(),
                ANSWERED.to_string(),
            ])
        });
        config.discovery.timeout_s = 60.0;
        let engine = start(config);
        let h = engine.handle();
        let snap = h
            .wait_until(WAIT, |s| {
                s.sources
                    .iter()
                    .all(|v| v.discovery.applied >= 2 && v.subjects >= 1)
            })
            .expect("both sources apply the sidecar's boxes");
        assert!(snap.sources.iter().all(|v| v.discovery.errors == 0));
        assert_eq!(children().len(), 1, "one sidecar for both sources");
        if LISTS_THREADS {
            let threads = pipeline_threads();
            assert!(threads.iter().any(|n| n == "ac-sam-host"), "{threads:?}");
            let workers = threads.iter().filter(|n| n.starts_with("ac-discovery"));
            assert_eq!(workers.count(), 2, "{threads:?}");
        }
        // The next request is the one the fake hangs on.
        let requested = |s: &ac_engine::EngineSnapshot| {
            s.sources.iter().map(|v| v.discovery.requests).sum::<u64>()
        };
        h.wait_until(WAIT, |s| requested(s) > u64::from(ANSWERED))
            .expect("a request the fake hangs on");
        std::thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        let report = engine.shutdown().unwrap();
        let took = started.elapsed();
        assert!(
            took < Duration::from_secs(4),
            "shutdown waited for the hung pass: {took:?}"
        );
        assert!(!report.final_snapshot.running);
        assert_eq!(
            report.threads_joined, 3,
            "two workers and the sidecar service"
        );
    }
}
