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
//! This binary runs without libtest's harness (`harness = false` in
//! `Cargo.toml`), for two reasons:
//!
//! * it holds a single test, so no other test's threads or children are
//!   counted;
//! * it is also the fake sidecar of the second studio. Started with
//!   [`FAKE_FLAG`], `main` speaks protocol v1 on stdin and stdout
//!   ([`unix::fake_sidecar`]) instead of testing. `ac-sidecar`'s own
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

/// What the command line asks of this binary.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    /// Be the fake sidecar, with these arguments.
    Fake(Vec<String>),
    /// List the test if it is selected.
    List(bool),
    /// Run the test if it is selected.
    Run(bool),
}

/// libtest options that take a value as the next argument; it is not a
/// filter.
#[cfg(unix)]
const VALUED: [&str; 7] = [
    "--test-threads",
    "--skip",
    "--format",
    "--color",
    "--logfile",
    "--shuffle-seed",
    "-Z",
];

/// Reads the command line like a one-test libtest binary: filters (exact
/// with `--exact`), `--skip`, `--ignored` (this test is not ignored, so it
/// selects nothing), `--list`, and the values of the options in [`VALUED`],
/// which are not filters. Other options are accepted and ignored.
#[cfg(unix)]
fn asked(args: &[String]) -> Asked {
    if args.first().map(String::as_str) == Some(FAKE_FLAG) {
        return Asked::Fake(args[1..].to_vec());
    }
    let (mut filters, mut skips) = (Vec::new(), Vec::new());
    let (mut exact, mut ignored_only, mut list) = (false, false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--list" => list = true,
            "--exact" => exact = true,
            "--ignored" => ignored_only = true,
            "--skip" => skips.extend(it.next().cloned()),
            a if a.starts_with("--skip=") => skips.push(a["--skip=".len()..].to_owned()),
            a if VALUED.contains(&a) => {
                it.next();
            }
            a if a.starts_with('-') => {}
            filter => filters.push(filter.to_owned()),
        }
    }
    let hits = |f: &String| {
        if exact {
            NAME == f
        } else {
            NAME.contains(f.as_str())
        }
    };
    let selected = !ignored_only
        && (filters.is_empty() || filters.iter().any(hits))
        && !skips.iter().any(hits);
    if list {
        Asked::List(selected)
    } else {
        Asked::Run(selected)
    }
}

/// Checks [`asked`] on the command lines `cargo test` passes.
#[cfg(unix)]
fn check_asked() {
    let ask = |line: &str| {
        asked(
            &line
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>(),
        )
    };
    let cases = [
        ("", Asked::Run(true)),
        ("--test-threads 1", Asked::Run(true)),
        ("--test-threads=1 --nocapture", Asked::Run(true)),
        ("shutdown", Asked::Run(true)),
        ("unrelated", Asked::Run(false)),
        ("--skip shutdown", Asked::Run(false)),
        ("--skip=shutdown", Asked::Run(false)),
        ("--skip unrelated", Asked::Run(true)),
        ("--ignored", Asked::Run(false)),
        ("--include-ignored", Asked::Run(true)),
        ("--exact shutdown", Asked::Run(false)),
        (&format!("--exact {NAME}"), Asked::Run(true)),
        ("--format terse --color never -q", Asked::Run(true)),
        ("--list", Asked::List(true)),
        ("--list --ignored", Asked::List(false)),
        (
            "--fake-sidecar --hang-after 4",
            Asked::Fake(vec!["--hang-after".into(), "4".into()]),
        ),
    ];
    for (line, want) in cases {
        assert_eq!(ask(line), want, "{line:?}");
    }
}

/// Runs the fake sidecar or the test, like a one-test libtest binary; see
/// [`asked`].
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selected = match asked(&args) {
        Asked::Fake(args) => return unix::fake_sidecar(&args),
        Asked::List(selected) => {
            if selected {
                println!("{NAME}: test");
            }
            return std::process::ExitCode::SUCCESS;
        }
        Asked::Run(selected) => selected,
    };
    check_asked();
    if !selected {
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

    /// The fake sidecar: hello, then one box per phrase for every request
    /// after a short pause, until stdin ends. With `--hang-after N` it
    /// answers N requests, then hangs in the next one and ignores stdin. It
    /// exits once its parent is gone, so a failed test leaves none behind.
    pub fn fake_sidecar(args: &[String]) -> ExitCode {
        let hang_after = match args {
            [] => None,
            [flag, n] if flag == "--hang-after" => match n.parse::<u32>() {
                Ok(n) => Some(n),
                Err(_) => return ExitCode::from(64),
            },
            _ => return ExitCode::from(64),
        };
        let parent = std::os::unix::process::parent_id();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(50));
                if std::os::unix::process::parent_id() != parent {
                    std::process::exit(5);
                }
            }
        });
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
        let mut answered = 0;
        loop {
            let detect = match read_message(&mut input) {
                Ok(Some((Message::Detect(d), _pixels))) => d,
                Ok(None) => return ExitCode::SUCCESS,
                Ok(Some(_)) | Err(_) => return ExitCode::from(4),
            };
            if hang_after.is_some_and(|n| answered >= n) {
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
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
            answered += 1;
        }
    }
}
