//! Latest-value mailboxes.
//!
//! Real-time stages must never queue unbounded work. Two single-slot
//! mailboxes cover every hand-off in the engine where only the newest value
//! matters:
//!
//! * [`Latest`] is a broadcast slot: one writer publishes, any number of
//!   readers clone the newest value (an `Arc` snapshot, a frame) without
//!   waiting and without consuming it. A version counter lets a reader tell
//!   whether anything new arrived since it last looked.
//! * [`Slot`] is a hand-off slot for one consumer thread: `put` replaces any
//!   value the consumer has not taken yet, and the consumer blocks in
//!   [`Slot::take`] until a value arrives, the timeout passes, or the slot is
//!   closed. A slow consumer therefore always works on the newest request and
//!   never on a backlog.
//!
//! Locks are held only to swap a value in or out, never across I/O or
//! computation, and a poisoned lock is recovered (the slot holds plain data
//! that is valid whatever a panicking holder was doing).

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Locks `m`, recovering the guard if a holder panicked.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A broadcast slot holding the newest published value; see the module docs.
#[derive(Debug)]
pub struct Latest<T> {
    /// The newest value and its version (0 before the first publish).
    slot: Mutex<(u64, Option<T>)>,
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self {
            slot: Mutex::new((0, None)),
        }
    }
}

impl<T: Clone> Latest<T> {
    /// An empty mailbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the value and returns its version, which starts at 1 and
    /// increases by one per publish.
    pub fn publish(&self, value: T) -> u64 {
        let mut slot = lock(&self.slot);
        slot.0 += 1;
        slot.1 = Some(value);
        slot.0
    }

    /// The newest value, if any was published.
    pub fn get(&self) -> Option<T> {
        lock(&self.slot).1.clone()
    }

    /// The newest value with its version, if any was published.
    pub fn get_versioned(&self) -> Option<(u64, T)> {
        let slot = lock(&self.slot);
        slot.1.clone().map(|v| (slot.0, v))
    }

    /// Version of the newest value; 0 before the first publish.
    pub fn version(&self) -> u64 {
        lock(&self.slot).0
    }
}

/// Why [`Slot::take`] returned without a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeError {
    /// The timeout passed with the slot empty.
    Timeout,
    /// The slot was closed; no value will ever arrive.
    Closed,
}

/// Contents of a [`Slot`].
#[derive(Debug)]
struct SlotState<T> {
    /// The pending value.
    value: Option<T>,
    /// Set once by [`Slot::close`].
    closed: bool,
    /// Values replaced before the consumer took them.
    replaced: u64,
}

/// A single-consumer hand-off slot that keeps only the newest value; see the
/// module docs.
#[derive(Debug)]
pub struct Slot<T> {
    /// The pending value and flags.
    state: Mutex<SlotState<T>>,
    /// Signalled on every put and on close.
    ready: Condvar,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(SlotState {
                value: None,
                closed: false,
                replaced: 0,
            }),
            ready: Condvar::new(),
        }
    }
}

impl<T> Slot<T> {
    /// An empty, open slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `value`, replacing a value not yet taken. Returns `false` (and
    /// drops `value`) when the slot is closed.
    pub fn put(&self, value: T) -> bool {
        let mut s = lock(&self.state);
        if s.closed {
            return false;
        }
        if s.value.replace(value).is_some() {
            s.replaced += 1;
        }
        drop(s);
        self.ready.notify_one();
        true
    }

    /// Waits up to `timeout` for a value and takes it.
    ///
    /// A value put before [`close`](Self::close) is still delivered; after
    /// that, a closed slot returns [`TakeError::Closed`] at once.
    pub fn take(&self, timeout: Duration) -> Result<T, TakeError> {
        let deadline = Instant::now() + timeout;
        let mut s = lock(&self.state);
        loop {
            if let Some(v) = s.value.take() {
                return Ok(v);
            }
            if s.closed {
                return Err(TakeError::Closed);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(TakeError::Timeout);
            }
            s = self
                .ready
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Closes the slot and wakes the consumer. Idempotent.
    pub fn close(&self) {
        lock(&self.state).closed = true;
        self.ready.notify_all();
    }

    /// True once [`close`](Self::close) was called.
    pub fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }

    /// How many values were replaced before the consumer took them.
    pub fn replaced(&self) -> u64 {
        lock(&self.state).replaced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn latest_starts_empty_and_keeps_only_the_newest() {
        let m = Latest::new();
        assert_eq!(m.get(), None::<u32>);
        assert_eq!(m.version(), 0);
        assert_eq!(m.publish(1), 1);
        assert_eq!(m.publish(2), 2);
        assert_eq!(m.get(), Some(2));
        assert_eq!(m.get_versioned(), Some((2, 2)));
        // Reading does not consume.
        assert_eq!(m.get(), Some(2));
    }

    #[test]
    fn latest_is_shared_across_threads() {
        let m = Arc::new(Latest::new());
        let w = m.clone();
        std::thread::spawn(move || {
            for i in 0..100u32 {
                w.publish(i);
            }
        })
        .join()
        .unwrap();
        assert_eq!(m.get(), Some(99));
        assert_eq!(m.version(), 100);
    }

    #[test]
    fn slot_replaces_untaken_values() {
        let s = Slot::new();
        assert!(s.put(1));
        assert!(s.put(2));
        assert_eq!(s.replaced(), 1);
        assert_eq!(s.take(Duration::ZERO), Ok(2));
        assert_eq!(s.take(Duration::ZERO), Err(TakeError::Timeout));
    }

    #[test]
    fn slot_take_waits_for_a_put() {
        let s = Arc::new(Slot::new());
        let p = s.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            p.put(7u8);
        });
        assert_eq!(s.take(Duration::from_secs(10)), Ok(7));
        t.join().unwrap();
    }

    #[test]
    fn closing_wakes_a_waiting_consumer_and_rejects_puts() {
        let s: Arc<Slot<u8>> = Arc::new(Slot::new());
        let c = s.clone();
        let t = std::thread::spawn(move || c.take(Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(20));
        let asked = Instant::now();
        s.close();
        assert_eq!(t.join().unwrap(), Err(TakeError::Closed));
        assert!(asked.elapsed() < Duration::from_secs(5));
        assert!(s.is_closed());
        assert!(!s.put(1));
    }

    #[test]
    fn a_value_put_before_close_is_still_delivered() {
        let s = Slot::new();
        s.put(3u8);
        s.close();
        assert_eq!(s.take(Duration::ZERO), Ok(3));
        assert_eq!(s.take(Duration::ZERO), Err(TakeError::Closed));
    }
}
