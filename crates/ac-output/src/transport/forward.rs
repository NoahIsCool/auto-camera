//! ffmpeg's stderr in the app's log.
//!
//! # Theory
//!
//! The stderr reader already parses every line ffmpeg logs ([`LogLine`]);
//! [`forward`] hands each one on as a `tracing` event of target
//! [`TARGET`], at the severity ffmpeg tagged it with, so the app's log
//! holds the encoder's own account of a session (stream mapping, encoder
//! settings, every reconnection attempt) beside the engine's. This crate
//! only emits events: whoever installed a subscriber (the app) decides
//! where they go, and without one an event costs a check.
//!
//! # Invariants
//!
//! * The text is the one the operator reads for an error (the reader's
//!   `line_text`): the object's class and the message, stream key
//!   redacted, so no secret reaches the log.
//! * Severities follow ffmpeg's tags: panic, fatal and error are errors,
//!   warning a warning, info info, verbose and debug debug, trace trace.
//!   An untagged line is a warning: it was written around ffmpeg's logger
//!   (or by a stand-in), and every such line was an error while ffmpeg
//!   logged errors only (see [`crate::logline`]), but it carries no
//!   severity to be sure of.

use crate::logline::{Level, LogLine};

/// The target of ffmpeg's lines in the log.
pub const TARGET: &str = "ac_output::ffmpeg";

/// Sends `line`, read as `text`, to the log; see the module docs.
pub fn forward(line: &LogLine, text: &str) {
    match line.level {
        Some(Level::Panic | Level::Fatal | Level::Error) => {
            tracing::error!(target: TARGET, "{text}");
        }
        Some(Level::Warning) | None => tracing::warn!(target: TARGET, "{text}"),
        Some(Level::Info) => tracing::info!(target: TARGET, "{text}"),
        Some(Level::Verbose | Level::Debug) => tracing::debug!(target: TARGET, "{text}"),
        Some(Level::Trace) => tracing::trace!(target: TARGET, "{text}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    use super::*;
    use crate::transport::{Shared, stderr_reader};

    /// One captured event: its level, target and message.
    type Captured = (tracing::Level, String, String);

    /// A subscriber that keeps every event, for the assertions below.
    #[derive(Default)]
    struct Capture {
        /// The events, in order.
        events: Arc<Mutex<Vec<Captured>>>,
        /// Next span id (spans are not used, but must get ids).
        spans: AtomicU64,
    }

    /// Collects an event's message.
    struct Message<'a>(&'a mut String);

    impl Visit for Message<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                let _ = write!(self.0, "{value:?}");
            }
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(self.spans.fetch_add(1, Ordering::Relaxed) + 1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut text = String::new();
            event.record(&mut Message(&mut text));
            let meta = event.metadata();
            let entry = (*meta.level(), meta.target().to_owned(), text);
            self.events.lock().unwrap().push(entry);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    /// The events `f` emits on this thread.
    ///
    /// `tracing` caches whether anyone listens to a call site for the whole
    /// process, and another test's thread hitting a site for the first time
    /// without a subscriber can cache "nobody" while this one listens. So
    /// `f` runs twice: the first run makes sure every site it reaches is
    /// registered, the cache is then rebuilt with this subscriber in it
    /// (no later registration can undo that), and only the second run
    /// counts.
    fn captured(f: impl Fn()) -> Vec<Captured> {
        let capture = Capture::default();
        let events = capture.events.clone();
        tracing::subscriber::with_default(capture, || {
            f();
            tracing::callsite::rebuild_interest_cache();
            events.lock().unwrap().clear();
            f();
        });
        events.lock().unwrap().clone()
    }

    #[test]
    fn every_stderr_line_reaches_the_log_redacted_at_its_severity() {
        let url = "rtmp://host/live/super-secret";
        let stderr = b"[info] Stream mapping:\n\
            [fifo @ 0x1] [error] Error opening rtmp://host/live/super-secret: refused\n\
            [flv @ 0x2] [warning] Failed to update header\r[tcp @ 0x3] [debug] probing\n\
            [trace] deep\n\
            fake ffmpeg failed on purpose\n\
            [fifo @ 0x1] [fatal] key super-secret rejected\n\n";
        let events = captured(|| {
            stderr_reader(
                &stderr[..],
                Arc::new(Shared::default()),
                Some(url.into()),
                None,
            );
        });
        use tracing::Level as L;
        let want = [
            (L::INFO, "Stream mapping:"),
            (
                L::ERROR,
                "fifo: Error opening rtmp://host/live/<redacted>: refused",
            ),
            (L::WARN, "flv: Failed to update header"),
            (L::DEBUG, "tcp: probing"),
            (L::TRACE, "deep"),
            (L::WARN, "fake ffmpeg failed on purpose"),
            (L::ERROR, "fifo: key <redacted> rejected"),
        ];
        assert_eq!(events.len(), want.len(), "{events:?}");
        for ((level, target, text), (want_level, want_text)) in events.iter().zip(want) {
            assert_eq!((*level, text.as_str()), (want_level, want_text));
            assert_eq!(target, TARGET);
        }
        assert!(events.iter().all(|(_, _, t)| !t.contains("super-secret")));
    }

    #[test]
    fn without_a_subscriber_forwarding_is_a_no_op() {
        let line = LogLine::parse("[error] nobody listens");
        forward(&line, "nobody listens");
    }
}
