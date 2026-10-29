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

/// The lines waiting in `rx`, taken out, without their times.
fn take(rx: &Receiver<Msg>) -> Vec<String> {
    rx.try_iter()
        .map(|m| match m {
            Msg::Line(line) | Msg::Header(line) => after_time(&line).to_owned(),
            Msg::Stop => "(stop)".to_owned(),
        })
        .collect()
}

#[test]
fn a_full_queue_drops_lines_and_the_log_says_how_many_where_they_were() {
    let dir = TempDir::new("dropped");
    // A subscriber whose queue holds two lines and nobody reads yet.
    let (tx, rx) = sync_channel(2);
    let dropped = Arc::new(AtomicU64::new(0));
    let subscriber = LineSubscriber {
        tx: tx.clone(),
        max: Level::INFO,
        dropped: dropped.clone(),
        spans: AtomicU64::new(0),
    };
    let note = |n: u64| {
        format!(
            "WARN  {} [-] {n} log lines dropped (the log could not keep up)",
            TARGET
        )
    };
    let ends = |lines: &[String], want: &[&str]| {
        assert_eq!(lines.len(), want.len(), "{lines:?}");
        for (line, want) in lines.iter().zip(want) {
            assert!(line.ends_with(want), "{line:?} is not {want:?}");
        }
    };
    tracing::subscriber::with_default(subscriber, || {
        for i in 0..5 {
            tracing::info!("line {i}");
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        // Still full: the note does not fit, so this line is lost too.
        tracing::info!("line 5");
        assert_eq!(dropped.load(Ordering::Relaxed), 4);
        ends(&take(&rx), &["] line 0", "] line 1"]);
        // The note goes where the gap is: just before the next line.
        tracing::info!("line 6");
        let lines = take(&rx);
        assert_eq!(lines[0], note(4));
        ends(&lines[1..], &["] line 6"]);
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        // Lines lost at the very end are noted when the writer stops.
        for i in 7..10 {
            tracing::info!("line {i}");
        }
    });
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
    let file = RotatingFile::open(dir.path(), "auto-camera", 1 << 20, 1).unwrap();
    let reader = std::thread::spawn({
        let dropped = dropped.clone();
        move || run(rx, file, &dropped)
    });
    tx.send(Msg::Stop).unwrap();
    reader.join().unwrap();
    let lines: Vec<String> = lines(dir.path())
        .iter()
        .map(|l| after_time(l).to_owned())
        .collect();
    ends(&lines[..2], &["] line 7", "] line 8"]);
    assert_eq!(lines[2..], [note(1)]);
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
}

#[test]
fn header_lines_begin_every_file_a_rotation_starts() {
    let dir = TempDir::new("header");
    // Room for the header and a few lines in each file, and all files kept.
    let file = RotatingFile::open(dir.path(), "auto-camera", 600, 100).unwrap();
    let mut w = Writer::start(file).unwrap();
    tracing::subscriber::with_default(w.subscriber(Level::INFO), || {
        tracing::info!(
            log_header = true,
            "auto-camera 0.1.0 (git abc) starting: pid 7"
        );
        tracing::info!(log_header = false, flag = true, "an ordinary line");
        for i in 0..30 {
            tracing::info!(n = i, "the show goes on");
        }
    });
    w.stop();
    // The files, newest first.
    let files: Vec<String> = std::iter::once("auto-camera.log".to_owned())
        .chain((1..).map(|n| format!("auto-camera.{n}.log")))
        .map_while(|name| std::fs::read_to_string(dir.path().join(name)).ok())
        .collect();
    assert!(files.len() >= 3, "the log rotated: {files:?}");
    for (n, text) in files.iter().enumerate() {
        let first = text.lines().next().unwrap();
        assert!(
            first.ends_with("] auto-camera 0.1.0 (git abc) starting: pid 7"),
            "file {n}: {text}"
        );
        assert_eq!(
            text.matches("starting: pid 7").count(),
            1,
            "file {n}: {text}"
        );
        assert!(
            !text.contains(HEADER_FIELD),
            "the mark is not written: {text}"
        );
    }
    // An ordinary line is written once; other bools are fields as usual.
    let all = files.concat();
    assert_eq!(
        all.matches("] an ordinary line flag=true").count(),
        1,
        "{all}"
    );
    assert_eq!(all.matches("] the show goes on").count(), 30, "{all}");
    assert!(
        files[0].ends_with("] the show goes on n=29\n"),
        "{}",
        files[0]
    );
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
