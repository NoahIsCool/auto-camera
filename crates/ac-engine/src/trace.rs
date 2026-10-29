//! The per-tick trace: how the engine hands one [`TraceRecord`] per tick to
//! whoever writes it down.
//!
//! A record is made in two steps. The core describes the tick it just ran
//! ([`EngineCore::trace_record`](crate::EngineCore::trace_record)), as a
//! deterministic function of its inputs. The engine thread then adds what
//! only the runtime knows, from its
//! [`TickSchedule`](crate::schedule::TickSchedule): whether the tick started
//! late and how many ticks the clock skipped just before it. It sends the
//! record on the [`TraceSender`] of its
//! [`EngineOptions::trace`](crate::EngineOptions::trace); the receiver, such
//! as the app's `--trace` writer, runs on a thread of its own.
//!
//! # Delivery
//!
//! The channel is a bounded `std::sync::mpsc::sync_channel`, and the engine
//! thread never waits on it (its `Tracer` only ever tries to send):
//!
//! * a full channel (the writer is a whole channel behind) drops the
//!   record, so the master clock never waits for a disk. Records are
//!   numbered by tick, so the gap shows in the trace;
//! * a closed channel (the writer is gone) ends tracing for the rest of the
//!   run, and the engine builds no more records.
//!
//! Records arrive in tick order, one per tick, since the engine thread is
//! the only sender. The engine's sender is dropped when its thread ends, so
//! a writer that reads until the channel closes, and holds no sender of its
//! own, has seen every record once the engine has shut down (or failed to
//! start, which drops the options and the sender in them).

use std::sync::mpsc::{SyncSender, TrySendError};

pub use ac_eval::TraceRecord;

/// The sending end of a trace channel; see the module docs.
pub type TraceSender = SyncSender<TraceRecord>;

/// The engine thread's end of a trace channel: one record per tick, never
/// waiting; see the module docs.
#[derive(Debug, Default)]
pub(crate) struct Tracer {
    /// The channel, until the writer is gone.
    tx: Option<TraceSender>,
}

impl Tracer {
    /// Sends to `tx`, or nowhere.
    pub(crate) fn new(tx: Option<TraceSender>) -> Self {
        Self { tx }
    }

    /// True while records are wanted, so that none is built for nobody.
    pub(crate) fn is_on(&self) -> bool {
        self.tx.is_some()
    }

    /// Sends `record` without waiting: dropped if the channel is full, and
    /// tracing ends if the writer is gone.
    pub(crate) fn send(&mut self, record: TraceRecord) {
        let Some(tx) = &self.tx else {
            return;
        };
        match tx.try_send(record) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => self.tx = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    use ac_eval::trace::SwitcherTrace;

    use super::*;

    /// An empty record of tick `tick`.
    fn record(tick: u64) -> TraceRecord {
        TraceRecord {
            version: ac_eval::TRACE_VERSION,
            tick,
            pts: Duration::ZERO,
            late: false,
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
    fn records_are_sent_in_order() {
        let (tx, rx) = sync_channel(8);
        let mut tracer = Tracer::new(Some(tx));
        assert!(tracer.is_on());
        for tick in 1..=3 {
            tracer.send(record(tick));
        }
        let got: Vec<u64> = rx.try_iter().map(|r| r.tick).collect();
        assert_eq!(got, [1, 2, 3]);
    }

    #[test]
    fn a_full_channel_drops_records_without_waiting() {
        let (tx, rx) = sync_channel(2);
        let mut tracer = Tracer::new(Some(tx));
        for tick in 1..=5 {
            tracer.send(record(tick));
        }
        assert!(tracer.is_on(), "a slow writer does not end the trace");
        let got: Vec<u64> = rx.try_iter().map(|r| r.tick).collect();
        assert_eq!(got, [1, 2]);
        tracer.send(record(6));
        assert_eq!(rx.try_recv().map(|r| r.tick), Ok(6), "the gap shows");
    }

    #[test]
    fn a_closed_channel_ends_the_trace() {
        let (tx, rx) = sync_channel(4);
        let mut tracer = Tracer::new(Some(tx));
        tracer.send(record(1));
        drop(rx);
        tracer.send(record(2));
        assert!(!tracer.is_on());
        tracer.send(record(3));
        // No channel at all: nothing is wanted, and sending is a no-op.
        let mut off = Tracer::new(None);
        assert!(!off.is_on());
        off.send(record(1));
        assert!(!Tracer::default().is_on());
    }
}
