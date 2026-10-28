//! Tests of crash reports: real panics on named threads, with the
//! reporter's hook installed for the test and the previous hook put back
//! after it. Every report goes into a temporary folder.
//!
//! The panic hook is one per process, so the tests that install one hold
//! [`serial`] for as long as theirs is installed.

use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use super::*;
use crate::logs::testdir::TempDir;
use crate::logs::{fixtures, summary};

/// Held by every test that replaces the process's panic hook.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    /// The lock.
    static HOOK: Mutex<()> = Mutex::new(());
    HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `f` with `reporter`'s hook installed, then puts the previous hook
/// back.
fn with_hook(reporter: &Arc<CrashReporter>, f: impl FnOnce()) {
    let previous = install(reporter.clone());
    f();
    std::panic::set_hook(Box::new(move |info| previous(info)));
}

/// Panics with `message` on a new thread named `name`, and checks that it
/// did.
fn panic_on_thread(name: &str, message: &str) {
    let message = message.to_owned();
    let joined = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || -> () { panic!("{message}") })
        .unwrap()
        .join();
    assert!(joined.is_err(), "the thread panicked");
}

/// The crash reports in `dir`, in name order.
fn reports(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(PREFIX))
        .collect();
    found.sort();
    found
}

#[test]
fn a_panic_on_a_thread_writes_a_crash_file_with_the_snapshot_summary() {
    let _serial = serial();
    let dir = TempDir::new("crash");
    let reporter = Arc::new(CrashReporter::new(
        dir.path(),
        "auto-camera 0.1.0 (git test)",
    ));
    let mut snap = fixtures::snapshot();
    snap.sidecar = Some(fixtures::sidecar());
    let summary = summary::render(&snap);
    reporter.set_summary(&summary);
    let message = format!("deliberate test panic in {}", std::process::id());
    with_hook(&reporter, || panic_on_thread("ac-crash-test", &message));

    let found = reports(dir.path());
    assert_eq!(found.len(), 1, "{found:?}");
    let name = found[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.ends_with(&format!("-{}-0.txt", std::process::id())),
        "{name}"
    );
    let text = std::fs::read_to_string(&found[0]).unwrap();
    assert!(
        text.starts_with("Auto Camera crash report\napp: auto-camera 0.1.0 (git test)\ntime: 20"),
        "{text}"
    );
    assert!(text.contains("\nthread: ac-crash-test\n"), "{text}");
    assert!(text.contains(&format!("\npanic: {message}\n")), "{text}");
    assert!(
        text.contains("\nat: apps/auto-camera/src/logs/crash/tests.rs:"),
        "{text}"
    );
    assert!(
        text.contains(&format!("\nlast snapshot:\n{summary}\n\nbacktrace:\n")),
        "{text}"
    );
    assert!(text.contains("sidecar: ready, pid 4242"), "{text}");
    assert!(text.contains("source stage (stage)"), "{text}");
}

#[test]
fn a_thread_that_panics_holding_the_summary_does_not_hang_the_hook() {
    let _serial = serial();
    let dir = TempDir::new("busy");
    let reporter = Arc::new(CrashReporter::new(dir.path(), "x"));
    reporter.set_summary("engine: tick 1");
    let holder = reporter.clone();
    with_hook(&reporter, || {
        let joined = std::thread::Builder::new()
            .name("ac-holder".into())
            .spawn(move || {
                let _held = holder.summary.lock().unwrap();
                panic!("panicked while holding the summary");
            })
            .unwrap()
            .join();
        assert!(joined.is_err());
    });
    let text = std::fs::read_to_string(&reports(dir.path())[0]).unwrap();
    assert!(
        text.contains("last snapshot:\n(the summary was being updated and could not be read)\n"),
        "{text}"
    );
    // The lock is poisoned now; its text is still read.
    assert!(reporter.summary.is_poisoned());
    assert_eq!(reporter.summary(), "engine: tick 1");
    reporter.set_summary("engine: tick 2");
    assert_eq!(reporter.summary(), "engine: tick 2");
}

#[test]
fn reports_are_capped_per_process_and_the_previous_hook_always_runs() {
    let _serial = serial();
    let dir = TempDir::new("capped");
    let reporter = Arc::new(CrashReporter::new(dir.path(), "x"));
    with_hook(&reporter, || {
        for i in 0..MAX_REPORTS + 2 {
            panic_on_thread("ac-crash-loop", &format!("panic {i}"));
        }
    });
    assert_eq!(reports(dir.path()).len(), MAX_REPORTS);

    // A reporter whose folder is gone writes nothing and still chains.
    let original = std::panic::take_hook();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    std::panic::set_hook(Box::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let gone = Arc::new(CrashReporter::new(&dir.path().join("gone"), "x"));
    let previous = install(gone);
    panic_on_thread("ac-crash-gone", "no folder");
    std::panic::set_hook(original);
    drop(previous);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!dir.path().join("gone").exists());
}

#[test]
fn the_summary_is_cut_and_starts_as_a_note() {
    let reporter = CrashReporter::new(Path::new("."), "x");
    assert_eq!(reporter.summary(), NO_SUMMARY);
    reporter.set_summary(&"é".repeat(MAX_SUMMARY));
    let summary = reporter.summary();
    assert_eq!(summary.len(), MAX_SUMMARY, "two-byte characters fill it");
    reporter.set_summary(&format!("a{}", "é".repeat(MAX_SUMMARY)));
    assert_eq!(reporter.summary().len(), MAX_SUMMARY - 1, "not split");
    // Held by this very thread: read without waiting, as a note.
    let held = reporter.summary.lock().unwrap();
    assert!(reporter.summary().contains("could not be read"));
    drop(held);
}

#[test]
fn a_report_reads_top_down() {
    let text = render(&Report {
        app: "auto-camera 0.1.0 (git abc)",
        time: Utc::since_epoch(Duration::from_millis(1_793_180_465_123)),
        thread: "ac-engine",
        message: "index out of bounds\nsecond line",
        location: Some("crates/x.rs:3:5".into()),
        backtrace: "   0: main\n",
        summary: "engine: tick 1\n",
    });
    let pid = std::process::id();
    assert_eq!(
        text,
        format!(
            "Auto Camera crash report\napp: auto-camera 0.1.0 (git abc)\n\
             time: 2026-10-28T09:41:05.123Z\npid: {pid}\nthread: ac-engine\n\
             panic: index out of bounds\\nsecond line\nat: crates/x.rs:3:5\n\
             \nlast snapshot:\nengine: tick 1\n\nbacktrace:\n   0: main\n"
        )
    );
    assert_eq!(cut("abcé", 4), "abc");
    assert_eq!(cut("abc", 4), "abc");
    assert_eq!(cut("", 0), "");
}

#[test]
fn prune_keeps_the_newest_reports_and_nothing_else_is_touched() {
    let dir = TempDir::new("prune");
    let names = [
        "crash-20261001-090000-11-0.txt",
        "crash-20261002-090000-12-0.txt",
        "crash-20261003-090000-13-0.txt",
        "crash-20261003-090000-13-1.txt",
        "crash-20261004-090000-14-0.txt",
    ];
    for n in names.iter().chain(&["auto-camera.log", "crash-notes.md"]) {
        std::fs::write(dir.path().join(n), b"x").unwrap();
    }
    assert_eq!(prune(dir.path(), 2), 3);
    let left: Vec<String> = reports(dir.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, [names[3], names[4], "crash-notes.md"]);
    assert!(dir.path().join("auto-camera.log").exists());
    assert_eq!(prune(dir.path(), 2), 0);
    assert_eq!(prune(&dir.path().join("missing"), 0), 0);
}
