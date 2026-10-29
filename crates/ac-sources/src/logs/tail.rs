//! The first and last lines of one child's stderr, kept for an error
//! message.
//!
//! # Theory
//!
//! ffmpeg says why it refused an input only on stderr, and says it first: a
//! capture device asked for a mode it does not have answers with "Selected
//! framerate (29.970030) is not supported by the device", then lists the
//! modes it has, one line each (dozens for some cameras), then the generic
//! "Error opening input" lines. A child that failed later, after hours on
//! air, says its last words last. A [`StderrTail`] therefore keeps the
//! first [`TAIL_OPENING`] lines and the newest [`TAIL_LAST`] after them,
//! and counts what it let go between, so a source's error can quote the
//! reason whichever end it is at ([`crate::capture`]). Its caller says how
//! many of the first lines to quote ([`StderrTail::summary`]): all it kept
//! for a child that never got going, whose whole answer is the refusal and
//! the list the operator must choose from, and [`TAIL_FIRST`] for one that
//! failed after it ran. The log still gets every line ([`super`]), but holds
//! back lines that differ only in their numbers, as every mode line does.
//!
//! # Invariants
//!
//! * Bounded: at most `TAIL_OPENING + TAIL_LAST` lines of at most
//!   [`TAIL_LINE`] characters each (cut on a character boundary and
//!   marked), whatever the child writes.
//! * A summary quotes lines in the order they were written, and says how
//!   many it left out between the first and the last ones it quotes.
//! * ffmpeg's log context (`[avfoundation @ 0x7f8e] `) is dropped from a
//!   kept line ([`strip_context`]): it names a memory address, not a cause.
//!   So is the indent of a listed item (a mode, a pixel format), which the
//!   `"; "` between quoted lines replaces.
//! * The drain thread ends the tail when the pipe ends, so
//!   [`StderrTail::summary`] can wait for a child's last words instead of
//!   racing the thread that reads them; the wait is bounded by its caller.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use super::CUT;
use crate::status::lock;

/// Lines a [`StderrTail`] keeps from the start of a child's stderr, and
/// quotes at most: enough for a refusal and the list of modes of a camera
/// that has many.
pub(crate) const TAIL_OPENING: usize = 64;

/// Lines of the start of a child's stderr quoted for a failure after the
/// child ran.
pub(crate) const TAIL_FIRST: usize = 4;

/// Lines a [`StderrTail`] keeps from the end of a child's stderr.
pub(crate) const TAIL_LAST: usize = 4;

/// Longest line a [`StderrTail`] keeps, in characters.
pub(crate) const TAIL_LINE: usize = 240;

/// The first and last lines of one child's stderr; see the module docs.
#[derive(Debug, Default)]
pub(crate) struct StderrTail {
    /// The lines kept so far.
    inner: Mutex<TailLines>,
    /// Signalled when the drain ends.
    ended: Condvar,
}

/// Contents of a [`StderrTail`].
#[derive(Debug, Default)]
struct TailLines {
    /// The first lines, at most [`TAIL_OPENING`].
    first: Vec<String>,
    /// The newest lines after `first`, at most [`TAIL_LAST`].
    last: VecDeque<String>,
    /// Lines between `first` and `last` that were let go.
    skipped: u64,
    /// The drain has ended: no line will follow.
    ended: bool,
}

impl StderrTail {
    /// An empty tail, waiting for its drain.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Keeps `line`, without its log context and the indent after it, cut
    /// at [`TAIL_LINE`] characters, letting the oldest of the last lines go
    /// when full.
    pub(crate) fn push(&self, line: &str) {
        let line = strip_context(line.trim()).trim_start();
        let line: String = match line.char_indices().nth(TAIL_LINE) {
            Some((cut, _)) => format!("{}{CUT}", &line[..cut]),
            None => line.to_owned(),
        };
        let mut t = lock(&self.inner);
        if t.first.len() < TAIL_OPENING {
            t.first.push(line);
            return;
        }
        if t.last.len() == TAIL_LAST {
            t.last.pop_front();
            t.skipped += 1;
        }
        t.last.push_back(line);
    }

    /// Notes that the drain ended and wakes [`StderrTail::summary`].
    pub(crate) fn end(&self) {
        lock(&self.inner).ended = true;
        self.ended.notify_all();
    }

    /// The child's first `first` lines (at most [`TAIL_OPENING`]) and its
    /// last [`TAIL_LAST`] after them, joined by `"; "`, with a count of any
    /// left out between, once the drain has ended or `wait` has passed
    /// (real time); `None` when no line was written by then.
    pub(crate) fn summary(&self, wait: Duration, first: usize) -> Option<String> {
        let guard = lock(&self.inner);
        let (t, _) = self
            .ended
            .wait_timeout_while(guard, wait, |t| !t.ended)
            .unwrap_or_else(|e| e.into_inner());
        if t.first.is_empty() {
            return None;
        }
        let (head, rest) = t.first.split_at(first.min(t.first.len()));
        // The kept lines after the quoted head, oldest first, of which the
        // newest TAIL_LAST are quoted from `from` on.
        let rest: Vec<&String> = rest.iter().chain(&t.last).collect();
        let from = rest.len().saturating_sub(TAIL_LAST);
        let mut parts = head.to_vec();
        let skipped = t.skipped + from as u64;
        if skipped > 0 {
            parts.push(format!("({skipped} more lines)"));
        }
        parts.extend(rest[from..].iter().map(|l| (*l).clone()));
        Some(parts.join("; "))
    }
}

/// `line` without ffmpeg's log context, the `[name @ 0x55d0c2a4] ` that
/// starts a line a component wrote; any other line as it is.
pub(crate) fn strip_context(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('[')
        && let Some((context, after)) = rest.split_once("] ")
        && context.contains(" @ ")
    {
        return after;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    /// What a tail quoting `first` of its first lines says once its drain
    /// has ended, after `lines`.
    fn quoted(lines: &[&str], first: usize) -> Option<String> {
        let tail = StderrTail::new();
        for line in lines {
            tail.push(line);
        }
        tail.end();
        tail.summary(Duration::ZERO, first)
    }

    /// [`quoted`] for a child that ran: its first [`TAIL_FIRST`] lines.
    fn summary_of(lines: &[&str]) -> Option<String> {
        quoted(lines, TAIL_FIRST)
    }

    /// `n` numbered lines.
    fn numbered(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    #[test]
    fn a_tail_keeps_the_first_and_last_lines_without_their_context() {
        assert_eq!(summary_of(&[]), None);
        assert_eq!(
            summary_of(&[
                "[avfoundation @ 0x7f8e] Selected framerate (29.970030) is not supported"
            ]),
            Some("Selected framerate (29.970030) is not supported".into())
        );
        let lines = numbered(12);
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        assert_eq!(
            summary_of(&borrowed).unwrap(),
            "line 0; line 1; line 2; line 3; (4 more lines); line 8; line 9; line 10; line 11"
        );
        // Exactly full: nothing let go, nothing counted.
        assert_eq!(
            summary_of(&borrowed[..TAIL_FIRST + TAIL_LAST]).unwrap(),
            "line 0; line 1; line 2; line 3; line 4; line 5; line 6; line 7"
        );
    }

    #[test]
    fn a_tail_quotes_a_whole_opening_up_to_its_bound() {
        // A refusal with its list of modes, as a camera with many answers.
        let mut lines = vec![
            "[avfoundation @ 0x7f8e] Selected framerate (29.970030) is not supported by the device."
                .to_owned(),
            "[avfoundation @ 0x7f8e] Supported modes:".to_owned(),
        ];
        lines.extend((1..=20).map(|i| {
            format!(
                "[avfoundation @ 0x7f8e]   {}x{}@[5.000000 30.000000]fps",
                i * 96,
                i * 54
            )
        }));
        lines.push("Error opening input: Input/output error".to_owned());
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        let all = quoted(&borrowed, TAIL_OPENING).unwrap();
        assert!(!all.contains("more lines"), "{all}");
        assert_eq!(all.split("; ").count(), 23, "{all}");
        assert!(all.contains("; 960x540@[5.000000 30.000000]fps; "), "{all}");
        assert!(all.ends_with(
            "1920x1080@[5.000000 30.000000]fps; Error opening input: Input/output error"
        ));
        // The same lines quoted for a child that ran: its first and last.
        let short = quoted(&borrowed, TAIL_FIRST).unwrap();
        assert!(short.contains("; (15 more lines); "), "{short}");
        // Past the bound, the opening is quoted with the newest lines.
        let lines = numbered(TAIL_OPENING + TAIL_LAST + 3);
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        let got = quoted(&borrowed, TAIL_OPENING).unwrap();
        let parts: Vec<&str> = got.split("; ").collect();
        assert_eq!(parts.len(), TAIL_OPENING + 1 + TAIL_LAST, "{got}");
        assert_eq!(
            parts[TAIL_OPENING - 1],
            format!("line {}", TAIL_OPENING - 1)
        );
        assert_eq!(parts[TAIL_OPENING], "(3 more lines)");
        assert_eq!(*parts.last().unwrap(), format!("line {}", lines.len() - 1));
        // A head longer than the lines written quotes them all.
        assert_eq!(quoted(&["a", "b"], usize::MAX).unwrap(), "a; b");
        assert_eq!(quoted(&["a", "b"], 0).unwrap(), "a; b");
    }

    #[test]
    fn a_short_head_leaves_out_only_the_lines_between() {
        // Ten lines, all among the first kept: four quoted from each end.
        let lines = numbered(10);
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        assert_eq!(
            summary_of(&borrowed).unwrap(),
            "line 0; line 1; line 2; line 3; (2 more lines); line 6; line 7; line 8; line 9"
        );
        let lines = numbered(TAIL_OPENING + 10);
        let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
        let got = summary_of(&borrowed).unwrap();
        let skipped = TAIL_OPENING + 10 - TAIL_FIRST - TAIL_LAST;
        assert!(
            got.contains(&format!("line 3; ({skipped} more lines); line ")),
            "{got}"
        );
        assert!(
            got.ends_with(&format!("line {}", TAIL_OPENING + 9)),
            "{got}"
        );
    }

    #[test]
    fn a_tail_cuts_long_lines_on_character_boundaries() {
        let long = "é".repeat(TAIL_LINE + 10);
        let got = summary_of(&[&long]).unwrap();
        assert_eq!(got.chars().count(), TAIL_LINE + CUT.chars().count());
        assert!(got.ends_with(CUT));
        let fits = "x".repeat(TAIL_LINE);
        assert_eq!(summary_of(&[&fits]).unwrap(), fits);
    }

    #[test]
    fn a_summary_waits_for_the_drain_only_so_long() {
        let tail = Arc::new(StderrTail::new());
        tail.push("early");
        let started = Instant::now();
        let got = tail.summary(Duration::from_millis(20), TAIL_FIRST);
        assert_eq!(got.as_deref(), Some("early"));
        assert!(started.elapsed() >= Duration::from_millis(20), "it waited");
        let late = tail.clone();
        let drain = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            late.push("last words");
            late.end();
        });
        let got = tail.summary(Duration::from_secs(10), TAIL_FIRST);
        assert_eq!(got.as_deref(), Some("early; last words"));
        drain.join().unwrap();
    }

    #[test]
    fn context_is_only_a_bracketed_component_at_an_address() {
        assert_eq!(strip_context("[in#0 @ 0x5579ee319ec0] Error"), "Error");
        assert_eq!(
            strip_context("[AVFoundation indev @ 0x13a604460] [0] FaceTime HD Camera"),
            "[0] FaceTime HD Camera"
        );
        for plain in [
            "[0] FaceTime HD Camera",
            "Error opening input files",
            "[unclosed @ 0x1",
            "",
        ] {
            assert_eq!(strip_context(plain), plain);
        }
    }
}
