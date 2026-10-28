//! The `tracing` subscriber that turns events into log lines, and the one
//! thread that writes them.
//!
//! # Theory
//!
//! Events come from every thread: the engine's tick, the discovery
//! workers, the sidecar's log drain, ffmpeg's stderr readers. Some of them
//! are real-time, so the subscriber does on the calling thread only what
//! it must: it checks the level, formats the event into one line (UTC
//! time, level, target, thread, message, then the fields as `key=value`)
//! and offers it to a bounded channel without waiting. One writer thread
//! (`ac-log-writer`) owns the [`RotatingFile`], writes the lines in the
//! order they came and flushes whenever the channel is empty. When the
//! channel is full (the disk stalls) the line is dropped and counted, and
//! the next line that finds room is queued after a note of the count, in
//! the layout of every other line (thread `-`), so a gap shows as a gap
//! where it is. Lines dropped with none after them are noted when the
//! writer stops. Only the writer ever waits on the disk.
//!
//! An event with the field `log_header = true` ([`HEADER_FIELD`]) is one
//! of the session's header lines (what ran: build, process, config,
//! models): it is logged like any other, without the field, and the file
//! keeps it to write again at the top of every file a rotation starts (see
//! the `file` module).
//!
//! This workspace logs events only. Spans (a library such as ONNX
//! Runtime's binding may open them) get ids and are otherwise ignored, so
//! their events still reach the log.
//!
//! # Invariants
//!
//! * One event is one line: line breaks in messages and fields are
//!   escaped, and a line is cut at [`MAX_LINE`] bytes.
//! * At most [`QUEUE_LINES`] lines wait for the writer.
//! * Logging never blocks the caller; a line that cannot be queued or
//!   written is lost, never retried.
//! * After [`Writer::stop`] the thread has written and flushed every line
//!   queued before the stop and exited; later events are dropped.

use std::fmt::Write as _;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use super::TARGET;
use super::file::RotatingFile;
use super::time::Utc;

/// Lines that may wait for the writer.
pub const QUEUE_LINES: usize = 4096;

/// Longest line written, in bytes; a longer one is cut and marked.
pub const MAX_LINE: usize = 8 * 1024;

/// Marker appended to a line cut at [`MAX_LINE`].
const CUT: &str = " [...]";

/// The field that marks an event as a header line, when `true`.
pub const HEADER_FIELD: &str = "log_header";

/// What the writer thread is sent.
#[derive(Debug)]
enum Msg {
    /// A formatted line, without its newline.
    Line(String),
    /// A formatted header line, without its newline.
    Header(String),
    /// Write what came before, flush and exit.
    Stop,
}

/// The writer thread and the way to it; see the module docs.
#[derive(Debug)]
pub struct Writer {
    /// Lines for the thread.
    tx: SyncSender<Msg>,
    /// Lines dropped because the channel was full, not yet noted.
    dropped: Arc<AtomicU64>,
    /// The thread, until stopped.
    thread: Option<JoinHandle<()>>,
}

impl Writer {
    /// Starts the writer thread on `file`.
    pub fn start(file: RotatingFile) -> io::Result<Self> {
        let (tx, rx) = sync_channel(QUEUE_LINES);
        let dropped = Arc::new(AtomicU64::new(0));
        let counter = dropped.clone();
        let thread = std::thread::Builder::new()
            .name("ac-log-writer".into())
            .spawn(move || run(rx, file, &counter))?;
        Ok(Self {
            tx,
            dropped,
            thread: Some(thread),
        })
    }

    /// A subscriber that sends this writer every event at `max` or more
    /// severe.
    pub fn subscriber(&self, max: Level) -> LineSubscriber {
        LineSubscriber {
            tx: self.tx.clone(),
            max,
            dropped: self.dropped.clone(),
            spans: AtomicU64::new(0),
        }
    }

    /// Writes what is queued, flushes and joins the thread. Idempotent.
    pub fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.tx.send(Msg::Stop);
            let _ = thread.join();
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The writer thread: writes lines in order, and flushes whenever nothing
/// waits. At the end it notes the lines dropped since the last one queued.
fn run(rx: Receiver<Msg>, mut file: RotatingFile, dropped: &AtomicU64) {
    loop {
        let msg = match rx.try_recv() {
            Ok(msg) => msg,
            Err(TryRecvError::Empty) => {
                let _ = file.flush();
                match rx.recv() {
                    Ok(msg) => msg,
                    Err(_) => break,
                }
            }
            Err(TryRecvError::Disconnected) => break,
        };
        match msg {
            Msg::Line(line) => {
                let _ = file.write_line(&line);
            }
            Msg::Header(line) => {
                let _ = file.write_header(&line);
            }
            Msg::Stop => break,
        }
    }
    let lost = dropped.swap(0, Ordering::Relaxed);
    if lost > 0 {
        let _ = file.write_line(&dropped_note(lost, &Utc::now()));
    }
    let _ = file.flush();
}

/// The line that says `lost` lines were dropped, noted at `time`.
fn dropped_note(lost: u64, time: &Utc) -> String {
    format!(
        "{} {:<5} {TARGET} [-] {lost} log lines dropped (the log could not keep up)",
        time.rfc3339(),
        Level::WARN.as_str()
    )
}

/// Formats events into lines for a [`Writer`]; see the module docs.
#[derive(Debug)]
pub struct LineSubscriber {
    /// Lines for the writer.
    tx: SyncSender<Msg>,
    /// Least severe level logged.
    max: Level,
    /// Lines dropped because the channel was full.
    dropped: Arc<AtomicU64>,
    /// Ids given to spans so far.
    spans: AtomicU64,
}

impl Subscriber for LineSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= self.max
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::from_level(self.max))
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(self.spans.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let thread = std::thread::current();
        let now = Utc::now();
        let msg = render(event, &now, thread.name().unwrap_or("-"));
        // The lines lost since the last one queued are noted before this
        // one, where the gap is; while there is no room, all are counted.
        let lost = self.dropped.swap(0, Ordering::Relaxed);
        let note = (lost > 0).then(|| Msg::Line(dropped_note(lost, &now)));
        if note.is_some_and(|note| self.tx.try_send(note).is_err()) {
            self.dropped.fetch_add(lost + 1, Ordering::Relaxed);
        } else if self.tx.try_send(msg).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

/// Collects an event's message and fields.
#[derive(Default)]
struct Fields {
    /// The message.
    message: String,
    /// ` key=value` for every other field.
    rest: String,
    /// Whether [`HEADER_FIELD`] was `true`.
    header: bool,
}

impl Visit for Fields {
    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == HEADER_FIELD {
            self.header = value;
        } else {
            self.record_debug(field, &value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

/// The log line of `event`, logged at `time` on thread `thread`:
/// `2026-10-28T09:41:05.123Z WARN  target [thread] message key=value`, as
/// a header line when the event is marked as one.
fn render(event: &Event<'_>, time: &Utc, thread: &str) -> Msg {
    let mut fields = Fields::default();
    event.record(&mut fields);
    let meta = event.metadata();
    let line = format!(
        "{} {:<5} {} [{thread}] {}{}",
        time.rfc3339(),
        meta.level().as_str(),
        meta.target(),
        fields.message,
        fields.rest
    );
    let line = one_line(&line, MAX_LINE);
    if fields.header {
        Msg::Header(line)
    } else {
        Msg::Line(line)
    }
}

/// `text` with line breaks escaped, cut at `max` bytes (on a character
/// boundary) and marked when it was longer.
pub fn one_line(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max) + CUT.len());
    for c in text.chars() {
        let escaped = match c {
            '\n' => "\\n",
            '\r' => "\\r",
            _ => "",
        };
        let width = if escaped.is_empty() {
            c.len_utf8()
        } else {
            escaped.len()
        };
        if out.len() + width > max {
            out.push_str(CUT);
            return out;
        }
        if escaped.is_empty() {
            out.push(c);
        } else {
            out.push_str(escaped);
        }
    }
    out
}

#[cfg(test)]
mod tests;
