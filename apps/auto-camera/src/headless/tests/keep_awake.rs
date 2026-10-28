//! A headless run holds the system awake for the whole run: a stand-in
//! `caffeinate` (a shell, so Unix only) runs from before the engine starts
//! until it has shut down, without the display flag, and it is the only one
//! even when the run records, since the engine's own hold is turned off.
//! A run that is not held, or not to its end, says so once.

#![cfg(unix)]

use super::*;
use ac_output::{AwakeMode, Caffeinate, caffeinate_args};
use ac_sources::{Clock, WallClock};
use std::sync::Mutex;

/// A stand-in that appends a line to `log` for each start (its process id,
/// then its arguments), then runs `then` in the shell.
fn stand_in_then(log: &Path, then: &str) -> Caffeinate {
    let script = format!(r#"echo "$$ $*" >> "$0"; {then}"#);
    Caffeinate::command("/bin/sh", ["-c", &script, log.to_str().unwrap()])
}

/// A stand-in that records its start in `log`, then waits to be killed.
fn stand_in(log: &Path) -> Caffeinate {
    stand_in_then(log, "exec sleep 60")
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

/// The wall clock, noting when it was last read. The engine thread and the
/// sources read it until they stop, and all of them have stopped once the
/// engine has shut down; so a hold that ends after the last read lasted the
/// engine's whole life, the recording's finish included.
#[derive(Debug, Default)]
struct Probe {
    /// The clock read.
    clock: WallClock,
    /// When it was last read, or a sleep on it last ended.
    last: Mutex<Option<Instant>>,
}

impl Probe {
    /// Notes a read now.
    fn touch(&self) {
        *self.last.lock().unwrap() = Some(Instant::now());
    }

    /// When the clock was last read.
    fn last_read(&self) -> Instant {
        self.last
            .lock()
            .unwrap()
            .expect("the engine read its clock")
    }
}

impl Clock for Probe {
    fn now(&self) -> Duration {
        self.touch();
        self.clock.now()
    }

    fn sleep(&self, duration: Duration) {
        self.clock.sleep(duration);
        self.touch();
    }
}

/// Watches, from another thread, the first stand-in that `log` records:
/// returns when it was first seen gone after it had been seen running, or
/// `None` when it was never seen running or did not end within 30 s.
fn watch(log: &Path) -> std::thread::JoinHandle<Option<Instant>> {
    let log = log.to_owned();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut seen = false;
        while Instant::now() < deadline {
            if let Some((pid, _)) = starts(&log).first() {
                if !exists(*pid) {
                    return seen.then(Instant::now);
                }
                seen = true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    })
}

/// Runs [`studio`] headless for half a second, recording to `record` if
/// given, with the stand-in of `log`; returns the run's notes. Checks that
/// the stand-in ran and ended only after the engine last read its clock
/// ([`Probe`]), so that it held the engine's whole life.
fn run_held(log: &Path, record: Option<PathBuf>) -> Vec<String> {
    let watcher = watch(log);
    let probe = Arc::new(Probe::default());
    let mut notes = Vec::new();
    let s = run_with(
        studio(),
        Path::new("x.toml"),
        &options(0.5, record),
        EngineOptions {
            clock: probe.clone(),
            keep_awake: stand_in(log),
            ..EngineOptions::default()
        },
        &QuitSignal::new(),
        |n| notes.push(n.to_owned()),
    );
    assert!(s.ok, "{:?}", s.error);
    let gone = watcher.join().unwrap().expect("the stand-in ran and ended");
    assert!(
        gone > probe.last_read(),
        "the hold ended while the engine still ran"
    );
    notes
}

#[test]
fn a_headless_run_is_held_awake_from_start_to_shutdown() {
    let log = temp_path("caffeinate.log");
    let notes = run_held(&log, None);
    assert!(notes.is_empty(), "{notes:?}");
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
    let notes = run_held(&log, Some(record.clone()));
    assert!(notes.is_empty(), "{notes:?}");
    // The engine's own hold for the recording was turned off.
    let starts = starts(&log);
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert!(!exists(starts[0].0), "{} is left behind", starts[0].0);
    std::fs::remove_file(&log).unwrap();
    std::fs::remove_file(&record).unwrap();
}

#[test]
fn a_caffeinate_that_ends_during_the_run_is_noted_at_its_end() {
    let log = temp_path("caffeinate.log");
    let mut notes = Vec::new();
    let s = run_with(
        studio(),
        Path::new("x.toml"),
        &options(0.2, None),
        EngineOptions {
            keep_awake: stand_in_then(&log, "exit 7"),
            ..EngineOptions::default()
        },
        &QuitSignal::new(),
        |n| notes.push(n.to_owned()),
    );
    assert!(s.ok, "{:?}", s.error);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("not kept awake to the end"), "{notes:?}");
    assert_eq!(starts(&log).len(), 1);
    std::fs::remove_file(&log).unwrap();
}

#[cfg(not(target_os = "macos"))]
#[test]
fn elsewhere_a_headless_run_skips_the_hold_with_one_logged_note() {
    let mut notes = Vec::new();
    let s = run_with(
        studio(),
        Path::new("x.toml"),
        &options(0.2, None),
        EngineOptions::default(),
        &QuitSignal::new(),
        |n| notes.push(n.to_owned()),
    );
    assert!(s.ok, "{:?}", s.error);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("only on macOS"), "{notes:?}");
}
