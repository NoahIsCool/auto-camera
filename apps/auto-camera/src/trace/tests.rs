//! The trace writer: one line per record, never over an existing file, a
//! write error that ends the trace, an empty trace discarded, and an engine
//! traced through [`attach`].

use std::io;
use std::time::Duration;

use ac_engine::{Engine, MonitorSettings, StudioConfig};
use ac_eval::trace::SwitcherTrace;
use ac_sources::SourceConfig;

use super::*;

/// A fresh, unique path in the temp directory (not created).
fn temp_path(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "auto-camera-trace-{}-{nanos}-{name}",
        std::process::id()
    ))
}

/// An empty record of tick `tick`.
fn record(tick: u64) -> TraceRecord {
    TraceRecord {
        version: ac_eval::TRACE_VERSION,
        tick,
        pts: Duration::from_millis(tick * 33),
        late: tick == 2,
        skipped: 0,
        sources: Vec::new(),
        shots: Vec::new(),
        switcher: SwitcherTrace {
            program: None,
            preview: None,
            transition: None,
        },
    }
}

#[test]
fn records_are_written_one_line_each_in_order() {
    let path = temp_path("lines.jsonl");
    let (file, tx) = TraceFile::create(&path).unwrap();
    assert_eq!(file.path(), path);
    let sent: Vec<TraceRecord> = (1..=3).map(record).collect();
    for r in &sent {
        tx.send(r.clone()).unwrap();
    }
    drop(tx);
    assert_eq!(
        file.finish(),
        TraceReport {
            lines: 3,
            error: None
        }
    );
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(text.lines().count(), 3);
    assert_eq!(ac_eval::parse_trace(&text), Ok(sent));
}

#[test]
fn an_existing_file_is_never_overwritten() {
    let path = temp_path("taken.jsonl");
    std::fs::write(&path, "keep me\n").unwrap();
    let err = TraceFile::<TraceRecord>::create(&path).unwrap_err();
    assert!(err.starts_with("cannot create the trace"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me\n");
    std::fs::remove_file(&path).unwrap();
    let nowhere = temp_path("no-such-dir").join("trace.jsonl");
    assert!(TraceFile::<TraceRecord>::create(&nowhere).is_err());
}

#[test]
fn discarding_removes_an_empty_trace_and_keeps_one_with_lines() {
    let path = temp_path("empty.jsonl");
    let (file, tx) = TraceFile::<TraceRecord>::create(&path).unwrap();
    drop(tx);
    assert_eq!(
        file.discard_if_empty(),
        TraceReport {
            lines: 0,
            error: None
        }
    );
    assert!(!path.exists(), "the empty trace is removed");
    // So the path can be traced to again.
    let (file, tx) = TraceFile::create(&path).unwrap();
    tx.send(record(1)).unwrap();
    drop(tx);
    assert_eq!(file.discard_if_empty().lines, 1);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(ac_eval::parse_trace(&text), Ok(vec![record(1)]));
    // A file that is gone by then cannot be removed, and says so.
    let (file, tx) = TraceFile::<TraceRecord>::create(&path).unwrap();
    drop(tx);
    std::fs::remove_file(&path).unwrap();
    let report = file.discard_if_empty();
    assert_eq!(report.lines, 0);
    let error = report.error.unwrap();
    assert!(
        error.starts_with("cannot remove the empty trace"),
        "{error}"
    );
}

/// A writer that takes `room` bytes, then fails every write; its flush
/// fails when `bad_flush` is set.
struct Full {
    /// Bytes still accepted.
    room: usize,
    /// Flushing fails.
    bad_flush: bool,
}

impl Write for Full {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.room {
            return Err(io::Error::other("disk full"));
        }
        self.room -= buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.bad_flush {
            Err(io::Error::other("disk gone"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn a_write_error_stops_the_writer() {
    let line = record(1).to_json_line().len();
    let (tx, rx) = sync_channel(8);
    for tick in 1..=4 {
        tx.send(record(tick)).unwrap();
    }
    // The writer returns at the error, though senders are left.
    let report = write_lines(
        &rx,
        Full {
            room: line + 1,
            bad_flush: false,
        },
    );
    assert_eq!(report.lines, 1);
    assert_eq!(
        report.error.as_deref(),
        Some("cannot write the trace: disk full")
    );
    drop(tx);
    let report = write_lines(
        &rx,
        Full {
            room: usize::MAX,
            bad_flush: true,
        },
    );
    // The record that failed is gone; the other two are written, then the
    // flush fails.
    assert_eq!(report.lines, 2);
    assert_eq!(
        report.error.as_deref(),
        Some("cannot write the trace: disk gone")
    );
}

#[test]
fn an_engine_started_with_attached_options_traces_every_tick() {
    // What both modes do: attach, start the engine, stop it, finish.
    let mut config = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 1,
        },
    );
    config.output.width = 320;
    config.output.height = 180;
    config.monitors = MonitorSettings::disabled();
    let path = temp_path("engine.jsonl");
    let (file, options) = attach(Some(&path), EngineOptions::default()).unwrap();
    assert!(options.trace.is_some());
    let engine = Engine::start_with(config, options).unwrap();
    engine
        .handle()
        .wait_until(Duration::from_secs(20), |s| s.tick >= 5)
        .expect("the engine ticks");
    let report = engine.shutdown().unwrap();
    // The engine's sender went with its thread, so this returns.
    let trace = file.expect("a trace file").finish();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(trace.error, None);
    assert_eq!(trace.lines, report.ticks);
    assert_eq!(text.lines().count() as u64, report.ticks);
}

#[test]
fn attach_without_a_path_changes_nothing_and_refuses_an_existing_file() {
    let (file, options) = attach(None, EngineOptions::default()).unwrap();
    assert!(file.is_none() && options.trace.is_none());
    let path = temp_path("attach-taken.jsonl");
    std::fs::write(&path, "keep me\n").unwrap();
    let err = attach(Some(&path), EngineOptions::default()).unwrap_err();
    assert!(err.starts_with("cannot create the trace"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me\n");
    std::fs::remove_file(&path).unwrap();
}
