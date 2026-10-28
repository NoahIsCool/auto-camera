//! A headless run holds the system awake for the whole run: a stand-in
//! `caffeinate` (a shell, so Unix only) runs from before the engine starts
//! until it has shut down, without the display flag, and it is the only one
//! even when the run records, since the engine's own hold is turned off.

#![cfg(unix)]

use super::*;
use ac_output::{AwakeMode, Caffeinate, caffeinate_args};

/// A stand-in that appends a line to `log` for each start (its process id,
/// then its arguments), then waits to be killed.
fn stand_in(log: &Path) -> Caffeinate {
    let script = r#"echo "$$ $*" >> "$0"; exec sleep 60"#;
    Caffeinate::command("/bin/sh", ["-c", script, log.to_str().unwrap()])
}

/// The starts `log` records so far, whole lines only: process id and
/// arguments.
fn starts(log: &Path) -> Vec<(u32, Vec<String>)> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.split_inclusive('\n')
        .filter_map(|line| {
            let mut words = line.strip_suffix('\n')?.split(' ');
            let pid = words.next()?.parse().ok()?;
            Some((pid, words.map(String::from).collect()))
        })
        .collect()
}

/// True while process `pid` exists, a zombie included.
fn exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs [`studio`] headless for half a second, recording to `record` if
/// given, with the stand-in of `log`, while a watcher checks that the first
/// stand-in is alive during the run; returns whether it was.
fn run_held(log: &Path, record: Option<PathBuf>) -> bool {
    let watched = log.to_owned();
    let watcher = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some((pid, _)) = starts(&watched).first() {
                return exists(*pid);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    });
    let s = run_with(
        studio(),
        Path::new("x.toml"),
        &options(0.5, record),
        EngineOptions {
            keep_awake: stand_in(log),
            ..EngineOptions::default()
        },
        &QuitSignal::new(),
    );
    assert!(s.ok, "{:?}", s.error);
    watcher.join().unwrap()
}

#[test]
fn a_headless_run_is_held_awake_from_start_to_shutdown() {
    let log = temp_path("caffeinate.log");
    assert!(run_held(&log, None), "not held during the run");
    let starts = starts(&log);
    assert_eq!(starts.len(), 1, "{starts:?}");
    let (pid, args) = &starts[0];
    // Nothing shows program, so the display may sleep.
    assert_eq!(
        args,
        &caffeinate_args(AwakeMode::System, std::process::id())
    );
    assert!(!exists(*pid), "{pid} is left behind");
    std::fs::remove_file(&log).unwrap();
}

#[test]
fn a_recording_run_holds_one_caffeinate_only() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let log = temp_path("caffeinate.log");
    let record = temp_path("awake.mp4");
    assert!(
        run_held(&log, Some(record.clone())),
        "not held during the run"
    );
    // The engine's own hold for the recording was turned off.
    let starts = starts(&log);
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert!(!exists(starts[0].0), "{} is left behind", starts[0].0);
    std::fs::remove_file(&log).unwrap();
    std::fs::remove_file(&record).unwrap();
}
