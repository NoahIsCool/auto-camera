//! Shutdown leaves nothing behind: no engine, source, discovery, sidecar or
//! output thread, and no child process (ffmpeg, or the detector sidecar).
//! Threads are listed from `/proc/self/task`, so they are checked on Linux
//! only; children are checked on every Unix (procfs on Linux, `pgrep -P`
//! elsewhere, which covers macOS).
//!
//! Two studios run in turn: a synthetic one with ground-truth discovery,
//! recording with ffmpeg (when installed), and one whose detector is a
//! sidecar process (`kind = "sidecar"`, ADR 0004) shut down while it is
//! answering.
//!
//! This binary runs without libtest's harness (`harness = false` in
//! `Cargo.toml`), for two reasons:
//!
//! * it holds a single test, so no other test's threads or children are
//!   counted;
//! * it is also the fake sidecar of the second studio. Started with
//!   [`FAKE_FLAG`], `main` speaks protocol v1 on stdin and stdout
//!   ([`fake_sidecar`]) instead of testing. `ac-sidecar`'s own
//!   `ac-fake-sidecar` binary cannot be named reliably from another
//!   crate's tests, and libtest would print to stdout, which carries the
//!   protocol.
//!
//! Rayon's global pool is process-wide and persistent by design; it is
//! named apart and not counted.

#[cfg(unix)]
mod common;

/// The argument that makes this binary the fake sidecar.
#[cfg(unix)]
const FAKE_FLAG: &str = "--fake-sidecar";

/// The one test's name, for filters and `--list`.
#[cfg(unix)]
const NAME: &str = "shutdown_joins_every_thread_and_reaps_every_child";

/// Runs the fake sidecar or the test, like a one-test libtest binary:
/// `--list` names the test, and a filter that does not match it skips it.
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(FAKE_FLAG) {
        return unix::fake_sidecar();
    }
    if args.iter().any(|a| a == "--list") {
        println!("{NAME}: test");
        return std::process::ExitCode::SUCCESS;
    }
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if !filters.is_empty() && !filters.iter().any(|f| NAME.contains(f.as_str())) {
        println!("\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 1 filtered out\n");
        return std::process::ExitCode::SUCCESS;
    }
    println!("\nrunning 1 test");
    // A failed assertion panics, and the process exits with status 101.
    unix::shutdown_joins_every_thread_and_reaps_every_child();
    println!("test {NAME} ... ok\n\ntest result: ok. 1 passed; 0 failed\n");
    std::process::ExitCode::SUCCESS
}

/// Nothing to check where processes are not listed this way.
#[cfg(not(unix))]
fn main() {}

/// The test and the fake sidecar.
#[cfg(unix)]
mod unix {
    use std::io::{self, BufReader, BufWriter};
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use ac_core::ShotId;
    use ac_engine::{DetectorSettings, EngineCommand, OutputState, SidecarSettings};
    use ac_sidecar::protocol::{
        Capabilities, DetectResult, Found, Hello, Message, PROTOCOL_VERSION, read_message,
        write_message,
    };

    use super::FAKE_FLAG;
    use crate::common::{WAIT, have, start, studio, temp_path};

    /// Threads can be listed by name on this platform.
    const LISTS_THREADS: bool = cfg!(target_os = "linux");

    /// The fake sidecar's model name, as its hello reports it.
    const FAKE_MODEL: &str = "fake-for-shutdown";

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
        if let Some(path) = recording {
            assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 0));
            let _ = std::fs::remove_file(path);
        }
    }

    /// Two sources whose detector is this binary as a fake sidecar: one
    /// process for both, shut down while it is answering.
    fn sidecar_studio() {
        let mut config = studio(1);
        let second = config.sources[0].clone();
        config.sources.push(ac_engine::SourceEntry {
            id: "stage-2".into(),
            ..second
        });
        let me = std::env::current_exe().expect("this test binary");
        config.detector = DetectorSettings::Sidecar(SidecarSettings::new([
            me.to_str().expect("a UTF-8 path").to_owned(),
            FAKE_FLAG.to_owned(),
        ]));
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
        let report = engine.shutdown().unwrap();
        assert!(!report.final_snapshot.running);
        assert_eq!(
            report.threads_joined, 3,
            "two workers and the sidecar service"
        );
    }

    /// The fake sidecar: hello, then one box per phrase for every request
    /// (after a short pause, so shutdown can land during a pass), until
    /// stdin ends.
    pub fn fake_sidecar() -> ExitCode {
        let mut input = BufReader::new(io::stdin().lock());
        let mut out = BufWriter::new(io::stdout().lock());
        let hello = Message::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            model: FAKE_MODEL.into(),
            weights_sha256: None,
            input_side: 672,
            capabilities: Capabilities::default(),
        });
        if write_message(&mut out, &hello, &[]).is_err() {
            return ExitCode::from(5);
        }
        loop {
            let detect = match read_message(&mut input) {
                Ok(Some((Message::Detect(d), _pixels))) => d,
                Ok(None) => return ExitCode::SUCCESS,
                Ok(Some(_)) | Err(_) => return ExitCode::from(4),
            };
            std::thread::sleep(Duration::from_millis(20));
            let detections = detect
                .phrases
                .iter()
                .zip(0..)
                .filter(|(p, _)| p.threshold <= 0.9)
                .map(|(_, phrase)| Found {
                    phrase,
                    bbox: [0.3, 0.2, 0.2, 0.6],
                    score: 0.9,
                })
                .collect();
            let result = Message::Result(DetectResult {
                id: detect.id,
                source: detect.source,
                seq: detect.seq,
                pts_ns: detect.pts_ns,
                latency_ms: 20.0,
                detections,
            });
            if write_message(&mut out, &result, &[]).is_err() {
                return ExitCode::from(5);
            }
        }
    }
}
