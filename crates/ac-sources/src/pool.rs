//! Reuse of frame buffers.
//!
//! A 4K RGBA frame is 33 MB. Allocating one per frame costs page faults and
//! zeroing at every frame, so workers recycle buffers instead. A published
//! frame's `Arc<[u8]>` is shared with the mailbox and with any reader that
//! cloned it; once all of them have let go, the worker's own handle is the
//! only one left and [`Arc::get_mut`] succeeds. Only then is the buffer
//! written again, so a reader can never observe a frame changing under it.
//!
//! A worker fills its next buffer as soon as it has published a frame, so
//! at that moment every recent frame a reader still holds is out of reach:
//! the pool must remember more buffers than readers hold. The engine holds
//! the frame it shows and, for every source but one, up to four recent
//! frames in a presentation buffer that picks frames by PTS (the frame
//! shown among them; `ac_engine::presentation::CAPACITY`); the mailbox holds
//! the newest, and a detector may be working on one more. A buffer is
//! allocated only when every remembered one is still held, so the pool
//! keeps no more buffers alive than were once in use at the same time, plus
//! the one being filled.

use std::collections::VecDeque;
use std::sync::Arc;

/// Buffers remembered for reuse: the engine's four, the mailbox's newest
/// and a detector's (see the module docs), and one more, which is then free
/// again, so steady-state allocation stays at zero. A source read only for
/// its newest frame still circulates three or four buffers; seven 4K
/// frames (230 MB) are reached only when a reader really holds that many.
const SPARES: usize = 7;

/// A small pool of equally sized byte buffers; see the module docs.
#[derive(Debug)]
pub(crate) struct FramePool {
    /// Length of every buffer in bytes.
    len: usize,
    /// Recently published buffers, oldest first.
    recent: VecDeque<Arc<[u8]>>,
}

impl FramePool {
    /// A pool of `len`-byte buffers.
    pub(crate) fn new(len: usize) -> Self {
        Self {
            len,
            recent: VecDeque::with_capacity(SPARES + 1),
        }
    }

    /// Lends `f` a buffer nobody else references and returns it with `f`'s
    /// result. The buffer holds stale data (an old frame or zeros), so `f`
    /// must overwrite all of it.
    pub(crate) fn fill<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> (Arc<[u8]>, R) {
        let reusable = self
            .recent
            .iter_mut()
            .position(|b| Arc::get_mut(b).is_some());
        let mut buf = match reusable.and_then(|i| self.recent.remove(i)) {
            Some(buf) => buf,
            None => std::iter::repeat_n(0u8, self.len).collect(),
        };
        let bytes = Arc::get_mut(&mut buf).expect("pooled and fresh buffers are unique");
        let result = f(bytes);
        (buf, result)
    }

    /// Remembers a published buffer so it can be reused once readers let go.
    pub(crate) fn recycle(&mut self, buf: Arc<[u8]>) {
        if buf.len() != self.len {
            return;
        }
        if self.recent.len() == SPARES {
            self.recent.pop_front();
        }
        self.recent.push_back(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_buffers_are_reused() {
        let mut pool = FramePool::new(4);
        let (a, ()) = pool.fill(|b| b.copy_from_slice(&[1, 2, 3, 4]));
        let addr = a.as_ptr();
        pool.recycle(a);
        let (b, seen) = pool.fill(|b| b.to_vec());
        assert_eq!(b.as_ptr(), addr);
        assert_eq!(seen, vec![1, 2, 3, 4]);
    }

    #[test]
    fn buffers_still_held_by_readers_are_not_touched() {
        let mut pool = FramePool::new(2);
        let (a, ()) = pool.fill(|b| b.fill(7));
        let reader = a.clone();
        pool.recycle(a);
        let (b, ()) = pool.fill(|b| b.fill(9));
        assert_ne!(b.as_ptr(), reader.as_ptr());
        assert_eq!(&reader[..], &[7, 7]);
    }

    #[test]
    fn a_reader_holding_a_presentation_ring_gets_every_buffer_back() {
        // A worker publishing frame after frame to a reader like the
        // engine's for a source it aligns by PTS: frames 4 to 1 before the
        // newest in its presentation buffer (the newest is not collected
        // yet when the worker fills the next buffer), the newest in the
        // mailbox, and every 30th frame kept 25 frames longer by a
        // detector.
        const RING: usize = 4;
        let mut pool = FramePool::new(16);
        let mut ring: VecDeque<Arc<[u8]>> = VecDeque::new();
        let mut mailbox: Option<Arc<[u8]>> = None;
        let mut detector: Option<(u64, Arc<[u8]>)> = None;
        let mut fresh = Vec::new();
        for n in 0..600u64 {
            let remembered: Vec<*const u8> = pool.recent.iter().map(|b| b.as_ptr()).collect();
            let (buf, ()) = pool.fill(|b| b.fill(n as u8));
            if !remembered.contains(&buf.as_ptr()) {
                fresh.push(n);
            }
            if let Some(previous) = mailbox.replace(buf.clone()) {
                ring.push_back(previous);
                if ring.len() > RING {
                    ring.pop_front();
                }
            }
            pool.recycle(buf);
            if n % 30 == 0 {
                detector = ring.back().map(|b| (n, b.clone()));
            }
            if detector.as_ref().is_some_and(|(since, _)| n >= since + 25) {
                detector = None;
            }
        }
        // The pool grows to what the reader once held at the same time
        // (the first time the detector's frame was among the recent ones),
        // plus the one being filled, and then allocates no more.
        assert!(fresh.len() <= SPARES, "{fresh:?}");
        assert!(fresh.iter().all(|&n| n < 60), "{fresh:?}");
    }

    #[test]
    fn wrong_sized_buffers_are_ignored_and_spares_are_bounded() {
        let mut pool = FramePool::new(2);
        pool.recycle(Arc::from(vec![0u8; 3]));
        assert!(pool.recent.is_empty());
        for _ in 0..10 {
            pool.recycle(Arc::from(vec![0u8; 2]));
        }
        assert_eq!(pool.recent.len(), SPARES);
        let (b, ()) = pool.fill(|_| ());
        assert_eq!(b.len(), 2);
    }
}
