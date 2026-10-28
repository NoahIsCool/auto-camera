//! Tests of the subscriber and the writer thread, each writing into a
//! temporary folder.
//!
//! Every event here comes from a call site of its own test, on the test's
//! thread, under a subscriber set for that thread only, so no other test
//! can reach these sites first (see `tracing`'s per-process interest
//! cache).

use std::path::Path;

use super::*;
use crate::logs::testdir::TempDir;

/// The lines of the log in `dir`.
fn lines(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("auto-camera.log"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A writer on a fresh log in `dir`.
fn writer(dir: &Path) -> Writer {
    Writer::start(RotatingFile::open(dir, "auto-camera", 1 << 20, 1).unwrap()).unwrap()
}

/// The part of `line` after its time.
fn after_time(line: &str) -> &str {
    line.split_once(' ').unwrap().1
}

#[test]
fn events_become_one_line_each_with_level_target_thread_and_fields() {
    let dir = TempDir::new("events");
    let mut w = writer(dir.path());
    tracing::subscriber::with_default(w.subscriber(Level::INFO), || {
        tracing::info!(target: "ac_sidecar::host", pid = 42, "sidecar started: python server.py");
        tracing::warn!(input = %"/media/a b.mp4", note = "quoted", "two\nlines\r");
        tracing::error!("plain");
        tracing::debug!("below the level, not written");
    });
    w.stop();
    let lines = lines(dir.path());
    let thread = std::thread::current().name().unwrap().to_owned();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(
        after_time(&lines[0]),
        format!("INFO  ac_sidecar::host [{thread}] sidecar started: python server.py pid=42")
    );
    assert_eq!(
        after_time(&lines[1]),
        format!(
            "WARN  auto_camera::logs::writer::tests [{thread}] two\\nlines\\r \
             input=/media/a b.mp4 note=\"quoted\""
        )
    );
    assert!(lines[2].ends_with("] plain"), "{}", lines[2]);
    // RFC 3339 UTC with milliseconds.
    let time = lines[0].split(' ').next().unwrap();
    assert_eq!(time.len(), "2026-10-28T09:41:05.123Z".len());
    assert!(time.ends_with('Z') && time.as_bytes()[10] == b'T', "{time}");
}

#[test]
fn spans_get_ids_and_their_events_are_logged() {
    let dir = TempDir::new("spans");
    let mut w = writer(dir.path());
    tracing::subscriber::with_default(w.subscriber(Level::INFO), || {
        let span = tracing::info_span!("session", id = 3);
        let _entered = span.enter();
        tracing::info!("inside a span");
    });
    w.stop();
    assert!(lines(dir.path())[0].ends_with("inside a span"));
}

#[test]
fn a_full_queue_drops_lines_and_the_log_says_how_many() {
    let dir = TempDir::new("dropped");
    // A subscriber whose queue holds one line and nobody reads yet.
    let (tx, rx) = sync_channel(1);
    let dropped = Arc::new(AtomicU64::new(0));
    let subscriber = LineSubscriber {
        tx: tx.clone(),
        max: Level::INFO,
        dropped: dropped.clone(),
        spans: AtomicU64::new(0),
    };
    tracing::subscriber::with_default(subscriber, || {
        for i in 0..4 {
            tracing::info!("line {i}");
        }
    });
    assert_eq!(dropped.load(Ordering::Relaxed), 3);
    // The writer notes the loss before the next line, then goes on.
    let file = RotatingFile::open(dir.path(), "auto-camera", 1 << 20, 1).unwrap();
    let reader = std::thread::spawn({
        let dropped = dropped.clone();
        move || run(rx, file, &dropped)
    });
    tx.send(Msg::Stop).unwrap();
    reader.join().unwrap();
    let lines = lines(dir.path());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("3 log lines dropped"), "{}", lines[0]);
    assert!(lines[1].ends_with("line 0"), "{}", lines[1]);
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
}

#[test]
fn after_stop_events_are_dropped_quietly() {
    let dir = TempDir::new("stopped");
    let mut w = writer(dir.path());
    let subscriber = w.subscriber(Level::TRACE);
    w.stop();
    w.stop();
    tracing::subscriber::with_default(subscriber, || {
        tracing::trace!("nobody writes this");
    });
    assert!(lines(dir.path()).is_empty());
}

#[test]
fn lines_are_escaped_and_cut_on_character_boundaries() {
    assert_eq!(one_line("a\nb\r\nc", 100), "a\\nb\\r\\nc");
    assert_eq!(one_line("abcdef", 4), "abcd [...]");
    // "é" is two bytes and does not fit after "abc" in four.
    assert_eq!(one_line("abcé", 4), "abc [...]");
    // An escape that does not fit is not split either.
    assert_eq!(one_line("abc\n", 4), "abc [...]");
    assert_eq!(one_line("", 4), "");
    let long = "x".repeat(3 * MAX_LINE);
    assert_eq!(one_line(&long, MAX_LINE).len(), MAX_LINE + CUT.len());
}
