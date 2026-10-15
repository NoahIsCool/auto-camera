//! Reuse of frame buffers.
//!
//! A 4K RGBA frame is 33 MB. Allocating one per frame costs page faults and
//! zeroing at every frame, so workers recycle buffers instead. A published
//! frame's `Arc<[u8]>` is shared with the mailbox and with any reader that
//! cloned it; once all of them have let go, the worker's own handle is the
//! only one left and [`Arc::get_mut`] succeeds. Only then is the buffer
//! written again, so a reader can never observe a frame changing under it.

use std::collections::VecDeque;
use std::sync::Arc;

/// Buffers remembered for reuse. The mailbox holds the newest frame and a
/// reader typically holds the one before, so the third most recent buffer is
/// normally free again; remembering three keeps steady-state allocation at
/// zero without hoarding memory (three 4K frames are 100 MB).
const SPARES: usize = 3;

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
