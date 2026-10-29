//! Audio-only inputs: the trait a board's feed (or a file standing in for
//! one) is read through, and the bounded ring its blocks wait in.
//!
//! Program audio is one stereo input (spec M12). It can be a video source's
//! embedded audio, or an [`AudioInput`]: an input with no picture, such as
//! the USB audio interface of the soundboard, or a WAV/RF64 recording of
//! the board played as a rehearsal stand-in for it.
//!
//! # Blocks and their tags
//!
//! An input delivers interleaved `f32` [`AudioBlock`]s of a few
//! milliseconds each. Every block carries the input's [`SourceId`], a
//! sequence number and a PTS:
//!
//! * the **sequence number** counts the blocks the input produced, from 0,
//!   one per block. A block lost on the way (dropped by a full
//!   [`AudioRing`], or never captured) keeps its number, so a gap in the
//!   numbers is audio missed;
//! * the **PTS** is the media time of the block's first sample on the
//!   input's timeline, which is measured on the clock the engine runs on: a
//!   file counts samples from the moment it starts playing on that clock, a
//!   live device stamps the time it captured the block, read on that clock.
//!   PTS never decreases; a gap in it is time with no audio.
//!
//! # Live inputs stall, drift and disconnect
//!
//! A file paced on the engine's clock delivers exactly `rate` sample frames
//! per second of that clock. A device does not: its sample clock is a
//! crystal of its own, a little fast or slow (drift, some parts per
//! million), its driver can hold blocks back and release them in a burst or
//! lose them (a stall), and it can be unplugged (an error; a supervisor
//! opens it again). The engine must never wait on any of that, so an input
//! never hands its blocks over by blocking: it keeps them in an
//! [`AudioRing`], a bounded FIFO that drops its *oldest* block when full,
//! and the engine takes whatever is there once per tick
//! ([`AudioInput::drain`], a swap under a lock held for no I/O). A reader
//! that falls behind therefore loses the stale audio and keeps the newest,
//! and a forgotten ring costs a fixed amount of memory. Drift is measured
//! from the PTS and resampled away before the program pacer (`ac-engine`'s
//! program audio, with `ac-audio`'s `DriftCorrector`), which is why a
//! device's PTS must be capture times on the engine's clock and not its
//! sample count; stalls, and drift beyond what that corrects, are the
//! pacer's to absorb (`AudioPacer`: silence for a gap, cutting a late
//! backlog or a lead).
//!
//! A device's audio callback runs on a real-time thread, which must not
//! wait on a lock another thread may hold; an adapter hands its buffers to
//! a thread of its own (or a lock-free queue) that pushes them into the
//! ring.
//!
//! # Invariants
//!
//! * An input's blocks have strictly increasing sequence numbers and
//!   non-decreasing PTS, across reconnections too (a supervisor restamps
//!   them, as it does a video source's frames).
//! * Every block of one connection has the format its
//!   [`AudioInputInfo::format`] gives.
//! * [`AudioRing::push`] and [`AudioRing::drain`] never wait for anything
//!   but the ring's own lock, which no holder keeps across I/O; a poisoned
//!   lock is recovered, since the queue holds plain data.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use ac_core::SourceId;

use crate::{AudioBlock, SourceState};

/// What an audio input delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInputInfo {
    /// Input identity, tagged on every block.
    pub id: SourceId,
    /// Sample rate and interleaved channel count of its blocks; `None`
    /// while it is not known (the description of an input that has not
    /// opened yet).
    pub format: Option<(u32, u16)>,
}

/// A running audio-only input; see the module docs.
///
/// Implementations capture or decode on their own threads and keep their
/// blocks in an [`AudioRing`], so a reader never waits on I/O and never
/// sees an unbounded backlog.
pub trait AudioInput: Send {
    /// Static description, fixed when the input started.
    fn info(&self) -> &AudioInputInfo;
    /// The description of what the input delivers now. A plain input's
    /// never changes, so this is [`AudioInput::info`]; a supervised one
    /// describes its newest connection.
    fn current_info(&self) -> AudioInputInfo {
        self.info().clone()
    }
    /// Current health.
    fn state(&self) -> SourceState;
    /// Blocks delivered since the previous call, oldest first.
    fn drain(&self) -> Vec<AudioBlock>;
    /// Waits up to `timeout` (real time) for a block to drain; true when one
    /// is there. For a thread that forwards the blocks on as soon as they
    /// arrive, such as a supervisor; the engine only drains.
    fn wait(&self, timeout: Duration) -> bool;
    /// Blocks dropped because they were not drained in time.
    fn dropped(&self) -> u64;
    /// The error behind the current state, if an error is the reason.
    fn error(&self) -> Option<String>;
    /// Stops capturing or decoding and releases the input. Idempotent.
    fn stop(&mut self);
}

/// Bounded FIFO of audio blocks that drops its oldest block when full; see
/// the module docs.
#[derive(Debug)]
pub struct AudioRing {
    /// Queued blocks, oldest first.
    blocks: Mutex<VecDeque<AudioBlock>>,
    /// Signalled on every push.
    pushed: Condvar,
    /// Most blocks held at once; at least 1.
    capacity: usize,
    /// Blocks dropped because the ring was full.
    dropped: AtomicU64,
}

impl AudioRing {
    /// A ring holding up to `capacity` blocks (at least one).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            blocks: Mutex::new(VecDeque::with_capacity(capacity)),
            pushed: Condvar::new(),
            capacity,
            dropped: AtomicU64::new(0),
        }
    }

    /// Locks the queue, recovering it if a holder panicked.
    fn lock(&self) -> MutexGuard<'_, VecDeque<AudioBlock>> {
        self.blocks.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Most blocks held at once.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Appends `block`, dropping the oldest block when full, and wakes a
    /// waiter.
    pub fn push(&self, block: AudioBlock) {
        let mut q = self.lock();
        if q.len() >= self.capacity {
            q.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(block);
        drop(q);
        self.pushed.notify_all();
    }

    /// Removes and returns every queued block, oldest first.
    pub fn drain(&self) -> Vec<AudioBlock> {
        self.lock().drain(..).collect()
    }

    /// Waits up to `timeout` for a block to be queued; true when one is.
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = self.lock();
        let (guard, _) = self
            .pushed
            .wait_timeout_while(guard, timeout, |q| q.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        !guard.is_empty()
    }

    /// Number of blocks dropped so far.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;

    /// Block `seq` of one mono sample, `seq` as its value.
    fn block(seq: u64) -> AudioBlock {
        AudioBlock {
            source: SourceId("board".into()),
            seq,
            pts: Duration::from_millis(seq * 10),
            sample_rate: 48_000,
            channels: 1,
            samples: Arc::from(vec![seq as f32]),
        }
    }

    /// The sequence numbers of `blocks`.
    fn seqs(blocks: &[AudioBlock]) -> Vec<u64> {
        blocks.iter().map(|b| b.seq).collect()
    }

    #[test]
    fn a_ring_drains_in_order() {
        let r = AudioRing::new(4);
        r.push(block(1));
        r.push(block(2));
        assert_eq!(seqs(&r.drain()), [1, 2]);
        assert!(r.drain().is_empty());
        assert_eq!((r.dropped(), r.capacity()), (0, 4));
    }

    #[test]
    fn a_full_ring_drops_the_oldest_and_counts() {
        let r = AudioRing::new(2);
        for seq in 1..=5 {
            r.push(block(seq));
        }
        assert_eq!(seqs(&r.drain()), [4, 5], "the newest audio is kept");
        assert_eq!(r.dropped(), 3);
    }

    #[test]
    fn a_zero_capacity_still_keeps_the_newest_block() {
        let r = AudioRing::new(0);
        assert_eq!(r.capacity(), 1);
        r.push(block(1));
        r.push(block(2));
        assert_eq!(seqs(&r.drain()), [2]);
        assert_eq!(r.dropped(), 1);
    }

    #[test]
    fn wait_returns_at_once_with_audio_and_times_out_without() {
        let r = Arc::new(AudioRing::new(4));
        let started = Instant::now();
        assert!(!r.wait(Duration::from_millis(30)));
        assert!(started.elapsed() >= Duration::from_millis(30));
        r.push(block(1));
        assert!(r.wait(Duration::ZERO), "a queued block is there at once");
        r.drain();
        let writer = r.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            writer.push(block(2));
        });
        assert!(r.wait(Duration::from_secs(30)), "a push wakes the waiter");
        assert_eq!(seqs(&r.drain()), [2]);
        t.join().unwrap();
    }

    #[test]
    fn a_plain_input_describes_itself_as_it_started() {
        /// An input that never delivers.
        struct Quiet(AudioInputInfo);
        impl AudioInput for Quiet {
            fn info(&self) -> &AudioInputInfo {
                &self.0
            }
            fn state(&self) -> SourceState {
                SourceState::Starting
            }
            fn drain(&self) -> Vec<AudioBlock> {
                Vec::new()
            }
            fn wait(&self, _: Duration) -> bool {
                false
            }
            fn dropped(&self) -> u64 {
                0
            }
            fn error(&self) -> Option<String> {
                None
            }
            fn stop(&mut self) {}
        }
        let info = AudioInputInfo {
            id: SourceId("board".into()),
            format: Some((48_000, 16)),
        };
        let input = Quiet(info.clone());
        assert_eq!(input.current_info(), info);
    }
}
