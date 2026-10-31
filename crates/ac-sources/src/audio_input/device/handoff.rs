//! The bounded hand-off from a device's audio callback to its input's
//! worker thread.
//!
//! # Theory
//!
//! CoreAudio calls the input callback on a real-time thread with a buffer
//! of interleaved samples and the host time it was captured at. The
//! callback has to return within the buffer's own duration, so it must
//! never wait for anything another thread holds (a lock, a full queue) and
//! never call the allocator, which can take a lock. It therefore only
//! copies. A fixed pool of [`Captured`] buffers, each with room for
//! `max_frames` sample frames, is made when the input opens and circulates
//! between two bounded queues: the *free* queue, from which the callback
//! takes a buffer to fill, and the *filled* queue, on which it puts the
//! buffer for the worker, which turns it into an
//! [`AudioBlock`](ac_media::AudioBlock) and gives it back to the free
//! queue.
//!
//! * A callback buffer longer than `max_frames` is cut into several
//!   blocks, each stamped with its own host time: the buffer's, plus its
//!   offset into the buffer at the nominal rate. The blocks after the
//!   first are marked as cut ([`Captured::cut`]): their host times are not
//!   readings of their own, so they share the buffer's error.
//! * When the free queue is empty (the worker is behind by the whole pool)
//!   the block is dropped. It is counted ([`HandoffReader::lost`]), and the
//!   next block that gets through carries how many blocks and sample frames
//!   were dropped just before it, so the worker leaves their sequence
//!   numbers and their time out and reports the drop.
//! * Nothing grows: the pool's buffers are the only sample memory, the
//!   queues hold at most the pool, and a buffer the callback could not
//!   queue is kept for its next block rather than freed.
//!
//! The queues are std's bounded channels. The callback only ever calls
//! `try_recv` and `try_send`; the worker polls the filled queue with
//! `try_recv` and gives buffers back to the free queue, which has room for
//! the whole pool, so that never waits either. No thread ever parks on a
//! queue, and a bounded channel takes its waker's lock only to wake a
//! parked thread, so the callback takes no lock.
//!
//! The device's other callback, for errors, reports through
//! [`StreamEvents`]: an overload (CoreAudio calls it on the real-time
//! thread too) bumps a counter, and the loss of the device keeps its first
//! reason in a once-set cell, which the worker polls.
//!
//! # Invariants
//!
//! * [`CaptureSink::deliver`] never blocks and never allocates or frees;
//!   neither does [`StreamEvents::overload`].
//! * Every sample frame the callback is given either reaches the worker in
//!   a block or is counted as dropped on the block after it; trailing
//!   samples that are not a whole frame are ignored.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// One block of captured audio on its way from the callback to the worker.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    /// Host time at which its first sample frame was captured.
    pub host_time: Duration,
    /// True for a block cut from a callback buffer after its first: its
    /// host time is the buffer's plus its offset, not a reading of its
    /// own.
    pub cut: bool,
    /// Interleaved samples, whole frames.
    pub samples: Vec<f32>,
    /// Blocks dropped just before this one because no buffer was free.
    pub lost_blocks: u64,
    /// The sample frames of those blocks.
    pub lost_frames: u64,
}

/// The callback's side of the hand-off; see the module docs.
#[derive(Debug)]
pub struct CaptureSink {
    /// Interleaved channels of the device's buffers.
    channels: usize,
    /// Nominal sample rate, for the host time of a cut block.
    sample_rate: u32,
    /// Most sample frames in one block.
    max_frames: usize,
    /// Buffers to fill.
    free: Receiver<Captured>,
    /// Filled buffers, for the worker.
    filled: SyncSender<Captured>,
    /// A buffer the filled queue did not take, kept for the next block.
    spare: Option<Captured>,
    /// Blocks dropped since the last block that got through.
    lost_blocks: u64,
    /// Their sample frames.
    lost_frames: u64,
    /// Every block dropped, shared with the reader.
    lost_total: Arc<AtomicU64>,
}

/// The worker's side of the hand-off; see the module docs.
#[derive(Debug)]
pub struct HandoffReader {
    /// Filled buffers.
    filled: Receiver<Captured>,
    /// Where emptied buffers go back.
    free: SyncSender<Captured>,
    /// Every block the callback dropped.
    lost_total: Arc<AtomicU64>,
}

/// A hand-off for a device of `channels` channels at `sample_rate`, whose
/// blocks hold at most `max_frames` sample frames, with a pool of
/// `buffers` buffers (each at least one).
pub fn handoff(
    channels: u16,
    sample_rate: u32,
    max_frames: usize,
    buffers: usize,
) -> (CaptureSink, HandoffReader) {
    let (channels, max_frames, buffers) = (
        usize::from(channels.max(1)),
        max_frames.max(1),
        buffers.max(1),
    );
    let (free_tx, free_rx) = sync_channel(buffers);
    let (filled_tx, filled_rx) = sync_channel(buffers);
    for _ in 0..buffers {
        // Room for every buffer: this send cannot fail.
        let _ = free_tx.try_send(Captured {
            host_time: Duration::ZERO,
            cut: false,
            samples: Vec::with_capacity(max_frames * channels),
            lost_blocks: 0,
            lost_frames: 0,
        });
    }
    let lost_total = Arc::new(AtomicU64::new(0));
    let sink = CaptureSink {
        channels,
        sample_rate,
        max_frames,
        free: free_rx,
        filled: filled_tx,
        spare: None,
        lost_blocks: 0,
        lost_frames: 0,
        lost_total: lost_total.clone(),
    };
    let reader = HandoffReader {
        filled: filled_rx,
        free: free_tx,
        lost_total,
    };
    (sink, reader)
}

/// The time `frames` sample frames take at `sample_rate` (zero for a rate
/// of zero), to the nanosecond, rounded down.
pub(crate) fn frames_duration(frames: u64, sample_rate: u32) -> Duration {
    if sample_rate == 0 {
        return Duration::ZERO;
    }
    let nanos = u128::from(frames) * 1_000_000_000 / u128::from(sample_rate);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

impl CaptureSink {
    /// Hands `samples` (interleaved) over to the worker, captured from host
    /// time `host_time` on: one block per `max_frames` sample frames, or
    /// counted as dropped when no buffer is free. Never blocks, allocates
    /// or frees; for the device's callback.
    pub fn deliver(&mut self, host_time: Duration, samples: &[f32]) {
        let whole = samples.len() - samples.len() % self.channels;
        let step = self.max_frames * self.channels;
        for (i, chunk) in samples[..whole].chunks(step).enumerate() {
            let offset = (i * self.max_frames) as u64;
            let at = host_time.saturating_add(frames_duration(offset, self.sample_rate));
            self.put(at, i > 0, chunk);
        }
    }

    /// Queues `chunk` (whole frames, at most `max_frames`) as one block
    /// captured at `at` (`cut` from a longer buffer after its first), or
    /// counts it as dropped.
    fn put(&mut self, at: Duration, cut: bool, chunk: &[f32]) {
        let frames = (chunk.len() / self.channels) as u64;
        let Some(mut buffer) = self.spare.take().or_else(|| self.free.try_recv().ok()) else {
            self.drop_block(frames);
            return;
        };
        buffer.samples.clear();
        // Within the capacity every pool buffer was made with.
        buffer.samples.extend_from_slice(chunk);
        buffer.host_time = at;
        buffer.cut = cut;
        buffer.lost_blocks = std::mem::take(&mut self.lost_blocks);
        buffer.lost_frames = std::mem::take(&mut self.lost_frames);
        if let Err(TrySendError::Full(back) | TrySendError::Disconnected(back)) =
            self.filled.try_send(buffer)
        {
            // The block is dropped after the ones it carried.
            self.lost_blocks = back.lost_blocks;
            self.lost_frames = back.lost_frames;
            self.spare = Some(back);
            self.drop_block(frames);
        }
    }

    /// Counts a block of `frames` sample frames as dropped.
    fn drop_block(&mut self, frames: u64) {
        self.lost_blocks += 1;
        self.lost_frames += frames;
        self.lost_total.fetch_add(1, Ordering::Relaxed);
    }
}

impl HandoffReader {
    /// The oldest filled block, if there is one; never waits.
    pub fn take(&self) -> Option<Captured> {
        self.filled.try_recv().ok()
    }

    /// Gives an emptied buffer back to the pool.
    pub fn recycle(&self, buffer: Captured) {
        // The free queue has room for the whole pool; a callback that is
        // gone (its receiver dropped) needs no buffers.
        let _ = self.free.try_send(buffer);
    }

    /// Blocks the callback dropped so far because no buffer was free.
    pub fn lost(&self) -> u64 {
        self.lost_total.load(Ordering::Relaxed)
    }

    /// The counter behind [`HandoffReader::lost`], for the input's handle.
    pub(crate) fn lost_counter(&self) -> Arc<AtomicU64> {
        self.lost_total.clone()
    }
}

/// What a device's error callback reports; see the module docs. Clones
/// share one record.
#[derive(Debug, Clone, Default)]
pub struct StreamEvents {
    /// The record.
    inner: Arc<Events>,
}

/// The record behind [`StreamEvents`].
#[derive(Debug, Default)]
struct Events {
    /// Overloads reported so far.
    overloads: AtomicU64,
    /// Why the device was lost, if it was: the first reason given.
    lost: OnceLock<String>,
}

impl StreamEvents {
    /// A record with nothing reported.
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes an overload (the device's I/O cycle ran late). Never blocks
    /// or allocates; for a real-time thread.
    pub fn overload(&self) {
        self.inner.overloads.fetch_add(1, Ordering::Relaxed);
    }

    /// Notes that the stream is gone (the device was unplugged, or its
    /// format changed under it) and why; only the first reason is kept.
    pub fn lost(&self, why: impl Into<String>) {
        let _ = self.inner.lost.set(why.into());
    }

    /// Overloads reported so far.
    pub fn overloads(&self) -> u64 {
        self.inner.overloads.load(Ordering::Relaxed)
    }

    /// Why the stream is gone, once it is.
    pub fn lost_reason(&self) -> Option<&str> {
        self.inner.lost.get().map(String::as_str)
    }
}
