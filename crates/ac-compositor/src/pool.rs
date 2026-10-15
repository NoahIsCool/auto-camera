//! Recycling pool of shared frame buffers.
//!
//! Rendered pictures leave the compositor as `Arc<[u8]>` inside a
//! [`VideoFrame`](ac_media::VideoFrame), so the encoder, the recorder and the
//! monitor can all hold the same pixels without copying. The pool keeps its
//! own clone of every buffer it hands out; once every consumer has dropped
//! theirs, the pool's clone is the only reference left, `Arc::get_mut`
//! succeeds, and the buffer is rendered into again. In steady state, with
//! consumers that release frames within a few frame periods, the compositor
//! therefore allocates nothing.
//!
//! Invariants: the pool never holds more than its capacity; it never writes
//! into a buffer someone else can see (uniqueness is checked by `Arc::get_mut`,
//! not assumed); and when every buffer is still in use it allocates a fresh
//! one instead of waiting, so a slow consumer can cost memory but never stalls
//! the program output. The oldest buffer is forgotten (left to its holders)
//! when the pool is full.

use std::sync::Arc;

/// A bounded set of reusable `Arc<[u8]>` buffers.
#[derive(Debug)]
pub(crate) struct FramePool {
    /// Buffers handed out at least once, oldest first.
    buffers: Vec<Arc<[u8]>>,
    /// Largest number of buffers kept for reuse.
    capacity: usize,
}

impl FramePool {
    /// An empty pool keeping up to `capacity` buffers (at least one).
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            buffers: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    /// Fills a buffer of `len` bytes with `fill` and returns it shared.
    ///
    /// `fill` must write every byte: a recycled buffer holds an older frame.
    pub(crate) fn fill_with(&mut self, len: usize, fill: impl FnOnce(&mut [u8])) -> Arc<[u8]> {
        for buffer in &mut self.buffers {
            if buffer.len() == len
                && let Some(bytes) = Arc::get_mut(buffer)
            {
                fill(bytes);
                return Arc::clone(buffer);
            }
        }
        let mut bytes = vec![0u8; len];
        fill(&mut bytes);
        let buffer: Arc<[u8]> = bytes.into();
        if self.buffers.len() >= self.capacity {
            // Prefer forgetting a buffer of the wrong size (left over from a
            // different tile size); otherwise forget the oldest.
            let victim = self
                .buffers
                .iter()
                .position(|b| b.len() != len)
                .unwrap_or(0);
            self.buffers.remove(victim);
        }
        self.buffers.push(Arc::clone(&buffer));
        buffer
    }

    /// Number of buffers currently kept.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.buffers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_buffers_are_reused() {
        let mut pool = FramePool::new(2);
        let first = pool.fill_with(4, |b| b.fill(1));
        let ptr = first.as_ptr();
        drop(first);
        let second = pool.fill_with(4, |b| b.fill(2));
        assert_eq!(second.as_ptr(), ptr);
        assert_eq!(&*second, &[2, 2, 2, 2]);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn held_buffers_are_never_overwritten() {
        let mut pool = FramePool::new(4);
        let held = pool.fill_with(4, |b| b.fill(1));
        let other = pool.fill_with(4, |b| b.fill(2));
        assert_ne!(held.as_ptr(), other.as_ptr());
        assert_eq!(&*held, &[1, 1, 1, 1]);
    }

    #[test]
    fn pool_stays_within_capacity_when_consumers_hold_everything() {
        let mut pool = FramePool::new(2);
        let held: Vec<_> = (0..5).map(|i| pool.fill_with(3, |b| b.fill(i))).collect();
        assert_eq!(pool.len(), 2);
        assert!(held.iter().enumerate().all(|(i, b)| b[0] == i as u8));
    }

    #[test]
    fn wrong_sized_buffers_are_evicted_first() {
        let mut pool = FramePool::new(2);
        drop(pool.fill_with(8, |b| b.fill(0)));
        let _a = pool.fill_with(4, |b| b.fill(0));
        let _b = pool.fill_with(4, |b| b.fill(0));
        assert_eq!(pool.len(), 2);
        assert!(pool.buffers.iter().all(|b| b.len() == 4));
    }

    #[test]
    fn zero_capacity_is_raised_to_one() {
        let mut pool = FramePool::new(0);
        drop(pool.fill_with(1, |b| b.fill(0)));
        assert_eq!(pool.len(), 1);
    }
}
