//! `--trace` end to end: the built binary runs the demo studio (synthetic,
//! no camera, model or network) headless for 2 s and writes one JSON line
//! per tick, as many as the summary's tick count; and a studio whose engine
//! does not start (a detector sidecar whose program is missing), headless
//! or in the window, leaves no trace behind.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The app built with these tests.
const APP: &str = env!("CARGO_BIN_EXE_auto-camera");

/// The demo studio, from the repository root.
const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/demo.toml");

/// Longest a run may take before it is killed as hung.
const LIMIT: Duration = Duration::from_secs(120);

/// A fresh, unique path in the temp directory (not created).
fn temp_path(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "auto-camera-e2e-{}-{nanos}-{name}",
        std::process::id()
    ))
}

/// Runs the demo headless for `seconds` with `--trace trace`, and returns
/// its exit status, summary (stdout) and log (stderr).
fn run_demo(seconds: &str, trace: &Path) -> (ExitStatus, String, String) {
    run_app(
        [
            OsStr::new("--config"),
            OsStr::new(DEMO),
            OsStr::new("--headless"),
            OsStr::new("--seconds"),
            OsStr::new(seconds),
            OsStr::new("--trace"),
            trace.as_os_str(),
        ]
        .as_slice(),
    )
}

/// Runs the app with `args`, and returns its exit status, stdout and
/// stderr. A run that takes longer than [`LIMIT`] is killed, so a hang
/// fails the test instead of outliving it.
fn run_app(args: &[&OsStr]) -> (ExitStatus, String, String) {
    let mut child = Command::new(APP)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the app starts");
    // Read both pipes while the app runs, so a full pipe cannot stall it.
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
            panic!("the app did not exit within {LIMIT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (status, stdout.join().unwrap(), stderr.join().unwrap())
}

#[test]
fn a_headless_demo_writes_one_trace_line_per_tick() {
    let path = temp_path("demo.jsonl");
    let (status, summary, log) = run_demo("2", &path);
    let text = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    assert!(status.success(), "{status:?}: {log}");
    let text = text.expect("the trace was written");
    let summary: serde_json::Value = serde_json::from_str(&summary).expect("a JSON summary");
    let ticks = summary["ticks"].as_u64().expect("a tick count");
    assert!(ticks >= 60, "2 s at 30 fps and the setup: {ticks}");
    assert_eq!(text.lines().count() as u64, ticks);
    assert_eq!(summary["trace"]["records"], ticks);
    assert_eq!(summary["trace"]["missing"], 0);
    let records = ac_eval::parse_trace(&text).expect("every line is a record");
    assert!(records.iter().zip(1..).all(|(r, n)| r.tick == n));
    let last = records.last().unwrap();
    assert_eq!(last.sources[0].id.0, "stage");
    assert!(!last.sources[0].subjects.is_empty());
    assert_eq!(
        last.switcher.program.as_ref().map(|p| p.0.as_str()),
        summary["program"]["last"].as_str()
    );
}

#[test]
fn an_existing_trace_file_is_refused() {
    let path = temp_path("taken.jsonl");
    std::fs::write(&path, "keep me\n").unwrap();
    let (status, summary, _) = run_demo("1", &path);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(status.code(), Some(1));
    let summary: serde_json::Value = serde_json::from_str(&summary).expect("a JSON summary");
    assert_eq!(summary["ok"], false);
    let error = summary["error"].as_str().unwrap();
    assert!(error.contains("cannot create the trace"), "{error}");
    assert_eq!(text, "keep me\n");
}

#[test]
fn an_engine_that_does_not_start_leaves_no_trace_to_refuse_the_next_run() {
    // A detector sidecar whose program does not exist: the engine refuses
    // it when it starts, before its first tick, so the window mode exits
    // before any window opens. A missing source file would not do: that
    // source starts offline and is retried, so the engine starts and ticks.
    let studio = temp_path("unstartable.toml");
    let missing = temp_path("missing-python");
    std::fs::write(
        &studio,
        format!(
            "[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 320\nheight = 180\n\
             fps = 30\nsubjects = 1\n\n[detector]\nkind = \"sidecar\"\ncommand = [{:?}]\n",
            missing.display().to_string()
        ),
    )
    .unwrap();
    let path = temp_path("unstarted.jsonl");
    // Twice in each mode: the first run must not leave a file that the
    // second refuses, hiding the real cause.
    let mut runs = Vec::new();
    for headless in [true, true, false, false] {
        let mut args = vec![OsStr::new("--config"), studio.as_os_str()];
        if headless {
            args.extend([
                OsStr::new("--headless"),
                OsStr::new("--seconds"),
                OsStr::new("1"),
            ]);
        }
        args.extend([OsStr::new("--trace"), path.as_os_str()]);
        let (status, summary, log) = run_app(&args);
        let leftover = path.exists();
        let _ = std::fs::remove_file(&path);
        runs.push((headless, status, summary, log, leftover));
    }
    std::fs::remove_file(&studio).unwrap();
    for (run, (headless, status, summary, log, leftover)) in runs.into_iter().enumerate() {
        let case = format!("run {run} (headless {headless})");
        assert_eq!(status.code(), Some(1), "{case}: {log}");
        let error = if headless {
            let summary: serde_json::Value =
                serde_json::from_str(&summary).expect("a JSON summary");
            assert!(summary["trace"].is_null(), "{case}: {summary}");
            summary["error"].as_str().unwrap().to_owned()
        } else {
            log
        };
        assert!(error.contains("program not found"), "{case}: {error}");
        assert!(!error.contains("the trace"), "{case}: {error}");
        assert!(!leftover, "{case} left its empty trace");
    }
}
