//! `--trace PATH`: the engine's per-tick trace, written as JSON Lines.
//!
//! The engine thread hands one record per tick to a bounded channel and
//! never waits on it ([`ac_engine::trace`]). A [`TraceFile`] owns the other
//! end: a writer thread (`ac-trace`) that takes the records in order and
//! writes each as one line ([`ac_eval::TraceRecord::to_json_line`]) through
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
//! [`TraceFile::finish`] once its engine has stopped.

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread::JoinHandle;

use ac_engine::{EngineOptions, TraceRecord, TraceSender};

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

/// A trace file being written; see the module docs.
#[derive(Debug)]
pub struct TraceFile {
    /// Where.
    path: PathBuf,
    /// The writer thread.
    thread: JoinHandle<TraceReport>,
}

impl TraceFile {
    /// Creates the file at `path`, which must not exist, and starts its
    /// writer; the engine sends to the returned channel.
    pub fn create(path: &Path) -> Result<(Self, TraceSender), String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("cannot create the trace {}: {e}", path.display()))?;
        let (tx, rx) = sync_channel(QUEUE);
        let thread = std::thread::Builder::new()
            .name("ac-trace".into())
            .spawn(move || write_lines(&rx, BufWriter::new(file)))
            .map_err(|e| format!("cannot start the trace writer: {e}"))?;
        let path = path.to_owned();
        Ok((Self { path, thread }, tx))
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
            error: Some("the trace writer panicked".into()),
        })
    }
}

/// Writes every record from `rx` to `out`, one line each, until the channel
/// closes or a write fails, and flushes.
fn write_lines(rx: &Receiver<TraceRecord>, mut out: impl Write) -> TraceReport {
    let mut lines = 0;
    for record in rx {
        if let Err(e) = out.write_all(record.to_json_line().as_bytes()) {
            return TraceReport {
                lines,
                error: Some(format!("cannot write the trace: {e}")),
            };
        }
        lines += 1;
    }
    TraceReport {
        lines,
        error: out
            .flush()
            .err()
            .map(|e| format!("cannot write the trace: {e}")),
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
