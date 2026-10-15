//! The sidecar's stderr, kept as a bounded ring of recent lines.
//!
//! # Theory
//!
//! The sidecar logs to stderr (stdout carries only protocol frames). A
//! thread must drain that pipe all the time, or a chatty child fills the
//! pipe buffer and blocks inside a print. The lines are worth keeping: the
//! last one usually says why a child died ("Traceback ... MemoryError"),
//! and the host puts it into the reason it reports. So the drain thread
//! pushes each line into a ring that keeps the newest `capacity` lines and
//! drops the oldest.
//!
//! # Invariants
//!
//! * Memory is bounded: at most `capacity` lines of at most [`MAX_LINE`]
//!   bytes each; a longer line is cut and marked.
//! * The drain never blocks on the host: the ring's lock is held only to
//!   push one line or to copy the lines out, never across I/O. This ring
//!   is the crate's one piece of state shared between threads (the drain
//!   writes, readers take snapshots).
//! * Empty lines are not kept; carriage returns are trimmed.

use std::collections::VecDeque;
use std::io::{BufRead, ErrorKind};
use std::sync::{Arc, Mutex, MutexGuard};

/// Longest line kept, in bytes; the rest of a longer line is dropped.
pub const MAX_LINE: usize = 2000;

/// Marker appended to a line that was cut at [`MAX_LINE`].
const CUT: &str = " [...]";

/// Locks `m`, recovering the data if a holder panicked: every critical
/// section here leaves the data consistent.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Recent stderr lines of the sidecar; cheap to clone (shared).
#[derive(Debug, Clone)]
pub(crate) struct LogRing {
    /// The lines, oldest first.
    lines: Arc<Mutex<VecDeque<String>>>,
    /// Most lines kept (at least 1).
    capacity: usize,
}

impl LogRing {
    /// An empty ring keeping the newest `capacity` lines (at least 1).
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::new())),
            capacity: capacity.max(1),
        }
    }

    /// Adds a line, dropping the oldest when full. Empty lines are skipped.
    pub(crate) fn push(&self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        let mut lines = lock(&self.lines);
        while lines.len() >= self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// The kept lines, oldest first.
    pub(crate) fn lines(&self) -> Vec<String> {
        lock(&self.lines).iter().cloned().collect()
    }

    /// Drains `input` line by line into the ring until it ends or fails.
    pub(crate) fn drain(&self, mut input: impl BufRead) {
        while let Some(line) = read_line(&mut input, MAX_LINE) {
            self.push(line);
        }
    }
}

/// Reads one line of at most `cap` bytes (the rest of the line is skipped
/// and the line marked as cut), lossily decoded, without its line ending.
/// `None` at the end of input, or on a read error with nothing read.
pub(crate) fn read_line(input: &mut impl BufRead, cap: usize) -> Option<String> {
    let mut line = Vec::new();
    let mut cut = false;
    let mut any = false;
    loop {
        let (used, done) = match input.fill_buf() {
            Ok([]) => break,
            Ok(buf) => {
                any = true;
                let (chunk, used, done) = match buf.iter().position(|&b| b == b'\n') {
                    Some(i) => (&buf[..i], i + 1, true),
                    None => (buf, buf.len(), false),
                };
                let room = cap.saturating_sub(line.len());
                cut |= chunk.len() > room;
                line.extend_from_slice(&chunk[..chunk.len().min(room)]);
                (used, done)
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        input.consume(used);
        if done {
            break;
        }
    }
    if !any {
        return None;
    }
    let mut text = String::from_utf8_lossy(&line)
        .trim_end_matches('\r')
        .to_string();
    if cut {
        text.push_str(CUT);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_keeps_the_newest_lines() {
        let ring = LogRing::new(2);
        for l in ["a", "b", "", "  ", "c"] {
            ring.push(l.into());
        }
        assert_eq!(ring.lines(), ["b", "c"]);
        assert_eq!(LogRing::new(0).capacity, 1);
    }

    #[test]
    fn draining_splits_lines_and_trims_endings() {
        let ring = LogRing::new(10);
        ring.drain(&b"loading\r\nready\n\nlast without newline"[..]);
        assert_eq!(ring.lines(), ["loading", "ready", "last without newline"]);
    }

    #[test]
    fn long_lines_are_cut_and_marked() {
        let mut input = &b"abcdefgh\nij\n"[..];
        assert_eq!(read_line(&mut input, 3).as_deref(), Some("abc [...]"));
        assert_eq!(read_line(&mut input, 3).as_deref(), Some("ij"));
        assert_eq!(read_line(&mut input, 3), None);
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        let mut input = &b"caf\xe9\n"[..];
        assert_eq!(read_line(&mut input, 100).as_deref(), Some("caf\u{fffd}"));
    }

    #[test]
    fn lines_spanning_buffer_refills_are_joined() {
        let data = b"first line is long\nsecond\n";
        let mut input = std::io::BufReader::with_capacity(4, &data[..]);
        assert_eq!(
            read_line(&mut input, 100).as_deref(),
            Some("first line is long")
        );
        assert_eq!(read_line(&mut input, 100).as_deref(), Some("second"));
        assert_eq!(read_line(&mut input, 100), None);
    }
}
