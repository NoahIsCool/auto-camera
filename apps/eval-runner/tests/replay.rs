//! The runner binary end to end on the demo studio (synthetic, no camera,
//! model or network): a run's recorded detections, replayed, give its
//! trace record for record; a log with a pass removed still replays, and
//! skips just that update; and outputs are never overwritten, nor left
//! behind by a run that could not start.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use ac_eval::trace::parse_trace;
use ac_vision::{PassRecord, parse_pass_log};
use serde_json::Value;

/// The runner built with these tests.
const RUNNER: &str = env!("CARGO_BIN_EXE_eval-runner");

/// The demo studio, from the repository root.
const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/demo.toml");

/// Longest a run may take before it is killed as hung.
const LIMIT: Duration = Duration::from_secs(120);

/// A temporary directory removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    /// A fresh directory unique to this process and `name`.
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("ac-eval-runner-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// The path of `file` in the directory (not created).
    fn join(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the runner with `args`, and returns its exit status, stdout and
/// stderr. A run that takes longer than [`LIMIT`] is killed, so a hang
/// fails the test instead of outliving it.
fn run_runner(args: &[&OsStr]) -> (ExitStatus, String, String) {
    let mut child = Command::new(RUNNER)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the runner starts");
    // Read both pipes while it runs, so a full pipe cannot stall it.
    let read = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            text
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + LIMIT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the runner did not exit within {LIMIT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (
        status,
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default(),
    )
}

/// Runs the demo studio for 2 s writing its trace to `trace`, with the
/// further `args`; asserts success and returns the printed report.
fn run_demo(trace: &Path, args: &[&OsStr]) -> Value {
    let mut all = vec![
        OsStr::new("--config"),
        OsStr::new(DEMO),
        OsStr::new("--seconds"),
        OsStr::new("2"),
        OsStr::new("--trace"),
        trace.as_os_str(),
    ];
    all.extend_from_slice(args);
    let (status, stdout, stderr) = run_runner(&all);
    assert!(status.success(), "{status}: {stderr}");
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"))
}

/// The file at `path` as text.
fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Source 0's counter `key` in `report`.
fn count(report: &Value, key: &str) -> u64 {
    report["sources"][0][key].as_u64().unwrap()
}

/// Records the demo's passes in a live run into `dir`: the trace's and
/// the log's paths.
fn record(dir: &TempDir) -> (PathBuf, PathBuf) {
    let (trace, log) = (dir.join("live.jsonl"), dir.join("passes.jsonl"));
    let report = run_demo(
        &trace,
        &[OsStr::new("--record-detections"), log.as_os_str()],
    );
    assert_eq!(report["detections"], "live");
    assert_eq!(report["ticks"], 60);
    assert_eq!(report["logged"].as_u64(), Some(count(&report, "applied")));
    (trace, log)
}

#[test]
fn a_replay_of_the_demo_studio_gives_the_live_trace_record_for_record() {
    let dir = TempDir::new("replay");
    let (live, log) = record(&dir);
    let passes = parse_pass_log(&read(&log)).unwrap();
    assert!(passes.len() >= 10, "{} passes", passes.len());
    assert!(passes.iter().all(|p| p.source.0 == "stage"));
    let replay = dir.join("replay.jsonl");
    let report = run_demo(&replay, &[OsStr::new("--replay"), log.as_os_str()]);
    assert_eq!(report["detections"], "replay");
    assert_eq!(count(&report, "misses"), 0);
    assert_eq!(count(&report, "applied"), passes.len() as u64);
    assert_eq!(count(&report, "recorded"), passes.len() as u64);
    let a = parse_trace(&read(&live)).unwrap();
    let b = parse_trace(&read(&replay)).unwrap();
    assert_eq!(a.len(), 60);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x, y, "tick {}", x.tick);
    }
    assert_eq!(read(&live), read(&replay), "and byte for byte");
    // The studio did find and frame its three subjects.
    assert_eq!(a[59].sources[0].subjects.len(), 3);
    assert!(a[59].shots.len() > 1);
}

#[test]
fn a_log_with_a_pass_removed_still_replays_and_skips_that_update() {
    let dir = TempDir::new("removed");
    let (live, log) = record(&dir);
    let mut passes = parse_pass_log(&read(&log)).unwrap();
    let removed: PassRecord = passes.remove(passes.len() / 2);
    let edited = dir.join("edited.jsonl");
    let text: String = passes.iter().map(PassRecord::to_json_line).collect();
    std::fs::write(&edited, &text).unwrap();
    let (replay, relog) = (dir.join("replay.jsonl"), dir.join("relog.jsonl"));
    let report = run_demo(
        &replay,
        &[
            OsStr::new("--replay"),
            edited.as_os_str(),
            OsStr::new("--record-detections"),
            relog.as_os_str(),
        ],
    );
    assert_eq!(report["ticks"], 60, "it ran to the end");
    assert_eq!((count(&report, "misses"), count(&report, "errors")), (1, 0));
    assert_eq!(
        read(&relog),
        text,
        "every other pass was applied, as before"
    );
    // Up to the tick that asked for the missing pass, nothing changed.
    let a = parse_trace(&read(&live)).unwrap();
    let b = parse_trace(&read(&replay)).unwrap();
    let asked = a
        .iter()
        .position(|r| r.sources[0].frame.is_some_and(|f| f.seq == removed.seq))
        .unwrap();
    assert_eq!(a[..=asked], b[..=asked]);
    assert_eq!(b.len(), 60);
}

#[test]
fn an_existing_output_is_never_overwritten() {
    let dir = TempDir::new("exists");
    let trace = dir.join("trace.jsonl");
    std::fs::write(&trace, "keep me\n").unwrap();
    let (status, stdout, stderr) = run_runner(&[
        OsStr::new("--config"),
        OsStr::new(DEMO),
        OsStr::new("--seconds"),
        OsStr::new("1"),
        OsStr::new("--trace"),
        trace.as_os_str(),
    ]);
    assert_eq!(status.code(), Some(1), "{stdout}");
    assert!(stderr.contains("cannot create the trace"), "{stderr}");
    assert_eq!(read(&trace), "keep me\n");
    // A detection log that exists fails the run too, and the trace it had
    // created already is removed.
    let (fresh, log) = (dir.join("fresh.jsonl"), dir.join("log.jsonl"));
    std::fs::write(&log, "").unwrap();
    let (status, _, stderr) = run_runner(&[
        OsStr::new("--config"),
        OsStr::new(DEMO),
        OsStr::new("--seconds"),
        OsStr::new("1"),
        OsStr::new("--trace"),
        fresh.as_os_str(),
        OsStr::new("--record-detections"),
        log.as_os_str(),
    ]);
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains("cannot create the detection log"),
        "{stderr}"
    );
    assert!(!fresh.exists());
}

#[test]
fn a_run_that_cannot_start_writes_nothing() {
    let dir = TempDir::new("setup");
    let trace = dir.join("trace.jsonl");
    let garbage = dir.join("garbage.jsonl");
    std::fs::write(&garbage, "{\"version\":1}\nnot json\n").unwrap();
    let cases: [(&[&OsStr], &str); 3] = [
        (
            &[
                OsStr::new("--seconds"),
                OsStr::new("1"),
                OsStr::new("--replay"),
                garbage.as_os_str(),
            ],
            "line 1",
        ),
        (&[], "never ends"),
        (&[OsStr::new("--seconds=-1")], "positive"),
    ];
    for (extra, expected) in cases {
        let mut args = vec![
            OsStr::new("--config"),
            OsStr::new(DEMO),
            OsStr::new("--trace"),
            trace.as_os_str(),
        ];
        args.extend_from_slice(extra);
        let (status, _, stderr) = run_runner(&args);
        assert_eq!(status.code(), Some(1), "{expected}: {stderr}");
        assert!(stderr.contains(expected), "{expected}: {stderr}");
        assert!(!trace.exists(), "{expected}: no trace is left");
    }
    let (status, _, _) = run_runner(&[OsStr::new("--config"), OsStr::new(DEMO)]);
    assert_eq!(status.code(), Some(2), "bad arguments");
}
