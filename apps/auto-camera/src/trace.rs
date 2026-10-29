//! `--trace PATH`: the engine's per-tick trace, written as JSON Lines.
//!
//! The engine thread hands one record per tick to a bounded channel and
//! never waits on it ([`ac_engine::trace`]). A [`TraceFile`] owns the other
//! end: a writer thread (`ac-trace`) that takes the records in order and
//! writes each as one line ([`ac_engine::TraceRecord::to_json_line`]) through
//! a buffer, so the disk's pace never reaches the master clock. The channel
//! holds [`QUEUE`] records, half a minute of ticks at 30 fps; a writer
//! further behind than that loses records, and the gap shows in the
//! records' tick numbers.
//!
//! The file must not exist, as for `--record`, so a trace is never
//! overwritten. The writer ends when the channel closes, which is when
//! every sender is gone: the engine's goes with its thread, so
//! [`TraceFile::finish`] returns once the engine has stopped and no other
//! sender is left. It then reports the lines written, or the first write
//! error. After an error the writer stops reading, which closes the channel
//! and ends the engine's tracing too, so a full disk costs the trace, not
//! the show.
//!
//! Both modes start their engine with [`EngineOptions`] from [`attach`],
//! which creates the file and puts the channel in the options: a headless
//! run before its first phase, the window before it opens. Each waits for
//! [`TraceFile::finish`] once its engine has stopped, or, when the engine
//! did not start (or the window did not open), for
//! [`TraceFile::discard_if_empty`], which removes a file left without a
//! line. Since the path must not exist, an empty file left behind would
//! make the same command fail again with "File exists" once the real cause
//! is fixed, and hide that cause.
//!
//! The detection log (`--record-detections`, [`crate::detections`]) is
//! written the same way, by a `TraceFile` of its own record type: a file of
//! any [`Line`] is, and its messages name it.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use ac_engine::{EngineOptions, TraceRecord};

/// Records the channel holds for a writer that is behind: about 34 s of
/// ticks at 30 fps, a few megabytes.
pub const QUEUE: usize = 1024;

/// What a trace writer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceReport {
    /// Lines written, one per record.
    pub lines: u64,
    /// The first write error; the writer stopped there.
    pub error: Option<String>,
}

/// A record that a [`TraceFile`] writes, one per line.
pub trait Line: Send + 'static {
    /// What a file of them is called in messages, such as `"trace"`.
    const NOUN: &'static str;
    /// The name of the writer thread.
    const THREAD: &'static str;
    /// The record as one line of JSON Lines, with its final `\n`.
    fn to_json_line(&self) -> String;
}

impl Line for TraceRecord {
    const NOUN: &'static str = "trace";
    const THREAD: &'static str = "ac-trace";

    fn to_json_line(&self) -> String {
        TraceRecord::to_json_line(self)
    }
}

/// A trace file being written; see the module docs.
#[derive(Debug)]
pub struct TraceFile<T = TraceRecord> {
    /// Where.
    path: PathBuf,
    /// The writer thread.
    thread: JoinHandle<TraceReport>,
    /// The records it writes.
    records: PhantomData<fn(T)>,
}

impl<T: Line> TraceFile<T> {
    /// Creates the file at `path`, which must not exist, and starts its
    /// writer; the engine sends to the returned channel.
    pub fn create(path: &Path) -> Result<(Self, SyncSender<T>), String> {
        let noun = T::NOUN;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("cannot create the {noun} {}: {e}", path.display()))?;
        let (tx, rx) = sync_channel(QUEUE);
        let thread = std::thread::Builder::new()
            .name(T::THREAD.into())
            .spawn(move || write_lines(&rx, BufWriter::new(file)))
            .map_err(|e| format!("cannot start the {noun} writer: {e}"))?;
        let path = path.to_owned();
        let records = PhantomData;
        Ok((
            Self {
                path,
                thread,
                records,
            },
            tx,
        ))
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Waits until the channel has closed and every record is written, and
    /// says how it went. Blocks while any sender is left (a running
    /// engine), so call it after the engine has stopped.
    pub fn finish(self) -> TraceReport {
        self.thread.join().unwrap_or_else(|_| TraceReport {
            lines: 0,
            error: Some(format!("the {} writer panicked", T::NOUN)),
        })
    }

    /// [`finish`](Self::finish) for a run that failed before it began (an
    /// engine that did not start, a window that did not open), then removes
    /// the file if it holds no line, so that it does not block the next run
    /// (see the module docs). A file with lines (an engine that ticked
    /// before its window failed) is kept. The report's error is the
    /// writer's, or why the empty file could not be removed.
    pub fn discard_if_empty(self) -> TraceReport {
        let path = self.path.clone();
        let mut report = self.finish();
        if report.lines == 0
            && let Err(e) = std::fs::remove_file(&path)
        {
            report.error = report.error.or(Some(format!(
                "cannot remove the empty {} {}: {e}",
                T::NOUN,
                path.display()
            )));
        }
        report
    }
}

/// Writes every record from `rx` to `out`, one line each, until the channel
/// closes or a write fails, and flushes.
fn write_lines<T: Line>(rx: &Receiver<T>, mut out: impl Write) -> TraceReport {
    let mut lines = 0;
    for record in rx {
        if let Err(e) = out.write_all(record.to_json_line().as_bytes()) {
            return TraceReport {
                lines,
                error: Some(format!("cannot write the {}: {e}", T::NOUN)),
            };
        }
        lines += 1;
    }
    TraceReport {
        lines,
        error: out
            .flush()
            .err()
            .map(|e| format!("cannot write the {}: {e}", T::NOUN)),
    }
}

/// Creates the trace file at `path`, if one was asked for, and returns it
/// with `options` sending to it; without a path, `options` as they are.
pub fn attach(
    path: Option<&Path>,
    options: EngineOptions,
) -> Result<(Option<TraceFile>, EngineOptions), String> {
    let Some(path) = path else {
        return Ok((None, options));
    };
    let (file, tx) = TraceFile::create(path)?;
    let options = EngineOptions {
        trace: Some(tx),
        ..options
    };
    Ok((Some(file), options))
}

#[cfg(test)]
mod tests;
