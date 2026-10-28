//! Audio decoding into small blocks on a continuous, loop-aligned timeline.
//!
//! The audio decoder writes interleaved little-endian `f32` samples at the
//! file's own rate and channel count. The worker cuts them into blocks of
//! about [`BLOCK_MILLIS`] and stamps each block from a running sample count
//! (`SampleClock`), so audio PTS is exact, gap-free and strictly increasing:
//! block `n + 1` starts at the sample where block `n` ended.
//!
//! A block is released when its *last* sample is due on the source's timeline,
//! as a real capture device delivers a buffer once it has been filled. Blocks
//! wait in a bounded queue (`AudioQueue`); when nobody drains it, the oldest
//! block is dropped and counted, so a forgotten queue costs a fixed amount of
//! memory and the newest audio is always available.
//!
//! The video worker defines the timeline and shares how far it has got.
//! A block is released only once the video has published frames up to the
//! block's end, or once the video's pass has ended. Under real-time pacing the
//! video is normally there first, so this adds no delay; when video decoding
//! falls behind, audio waits for it instead of drifting ahead. When the file
//! loops, audio past the end of the video's pass is trimmed and a short audio
//! pass is padded with silence, so every pass of audio starts at exactly the
//! PTS of the first video frame of the same pass, paced or unpaced.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ac_core::SourceId;
use ac_media::AudioBlock;

use crate::file::Shared;
use crate::process::{Respawner, Spawned, read_full};
use crate::status::lock;

/// Target block length in milliseconds.
pub const BLOCK_MILLIS: u32 = 10;

/// Bytes per decoded sample (`f32`).
const SAMPLE_BYTES: usize = 4;

/// Nanoseconds per second.
const NANOS: u128 = 1_000_000_000;

/// Bounded FIFO of decoded audio that drops its oldest block when full.
#[derive(Debug)]
pub(crate) struct AudioQueue {
    /// Queued blocks, oldest first.
    blocks: Mutex<VecDeque<AudioBlock>>,
    /// Most blocks held at once; at least 1.
    capacity: usize,
    /// Blocks dropped because the queue was full.
    dropped: AtomicU64,
}

impl AudioQueue {
    /// A queue holding up to `capacity` blocks (at least one).
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            blocks: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            dropped: AtomicU64::new(0),
        }
    }

    /// Appends `block`, dropping the oldest block when full.
    pub(crate) fn push(&self, block: AudioBlock) {
        let mut q = lock(&self.blocks);
        if q.len() >= self.capacity {
            q.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(block);
    }

    /// Removes and returns every queued block, oldest first.
    pub(crate) fn drain(&self) -> Vec<AudioBlock> {
        lock(&self.blocks).drain(..).collect()
    }

    /// Number of blocks dropped so far.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Position on a source's audio timeline, counted in sample frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SampleClock {
    /// Samples per second per channel; non-zero.
    rate: u32,
    /// Interleaved channels; non-zero.
    channels: u16,
    /// Index of the next sample frame to emit.
    pos: u64,
}

impl SampleClock {
    /// A clock at sample zero. `rate` and `channels` are raised to 1 if zero.
    pub(crate) fn new(rate: u32, channels: u16) -> Self {
        Self {
            rate: rate.max(1),
            channels: channels.max(1),
            pos: 0,
        }
    }

    /// Sample frames per block: [`BLOCK_MILLIS`] worth, at least one.
    pub(crate) fn block_frames(&self) -> usize {
        (self.rate as usize * BLOCK_MILLIS as usize / 1000).max(1)
    }

    /// Bytes of decoder output per block.
    pub(crate) fn block_bytes(&self) -> usize {
        self.block_frames() * self.frame_bytes()
    }

    /// Bytes of decoder output per sample frame.
    fn frame_bytes(&self) -> usize {
        self.channels as usize * SAMPLE_BYTES
    }

    /// Media time of sample frame `frame`, rounded down to the nanosecond.
    pub(crate) fn time_of(&self, frame: u64) -> Duration {
        let nanos = frame as u128 * NANOS / self.rate as u128;
        let secs = u64::try_from(nanos / NANOS).unwrap_or(u64::MAX);
        Duration::new(secs, (nanos % NANOS) as u32)
    }

    /// The sample frame nearest to media time `t`.
    pub(crate) fn frame_at(&self, t: Duration) -> u64 {
        let frames = (t.as_nanos() * self.rate as u128 + NANOS / 2) / NANOS;
        u64::try_from(frames).unwrap_or(u64::MAX)
    }

    /// How many of the next `frames` sample frames fit before `limit`.
    pub(crate) fn fit(&self, frames: usize, limit: Option<u64>) -> usize {
        match limit {
            Some(limit) => {
                let room = limit.saturating_sub(self.pos);
                frames.min(usize::try_from(room).unwrap_or(usize::MAX))
            }
            None => frames,
        }
    }

    /// Media time at which a block of `frames` sample frames starting at the
    /// current position ends.
    pub(crate) fn end_after(&self, frames: usize) -> Duration {
        self.time_of(self.pos.saturating_add(frames as u64))
    }

    /// Stamps `samples` (whole interleaved frames) at the current position and
    /// moves past them.
    pub(crate) fn block(&mut self, source: &SourceId, samples: Arc<[f32]>) -> AudioBlock {
        let frames = samples.len() / self.channels as usize;
        let block = AudioBlock {
            source: source.clone(),
            pts: self.time_of(self.pos),
            sample_rate: self.rate,
            channels: self.channels,
            samples,
        };
        self.pos = self.pos.saturating_add(frames as u64);
        block
    }
}

/// Decodes a file's audio for the lifetime of a source.
pub(crate) struct AudioWorker {
    /// Source the blocks are tagged with.
    pub(crate) source: SourceId,
    /// Position on the audio timeline.
    pub(crate) clock: SampleClock,
    /// Whether the file loops.
    pub(crate) looping: bool,
    /// Starts the decoder for each pass.
    pub(crate) decoder: Respawner,
}

impl AudioWorker {
    /// Runs until end of input (without looping), the video worker finishing,
    /// or a stop request.
    pub(crate) fn run(mut self, shared: &Shared) {
        let mut bytes = vec![0u8; self.clock.block_bytes()];
        let mut pass = 0;
        while let Ok(Some(decoder)) = self.decoder.start(&shared.children) {
            if !self.play_pass(shared, decoder, pass, &mut bytes)
                || !self.looping
                || shared.stopping()
            {
                break;
            }
            let Some(end) = shared.progress.wait_end(pass) else {
                break;
            };
            if !self.pad_to(shared, self.clock.frame_at(end)) {
                break;
            }
            pass += 1;
        }
        self.decoder.discard(&shared.children);
    }

    /// Plays pass `pass` from `decoder`. Returns `false` when stopped.
    fn play_pass(
        &mut self,
        shared: &Shared,
        decoder: Spawned,
        pass: usize,
        bytes: &mut [u8],
    ) -> bool {
        let Spawned { key, mut stdout } = decoder;
        let frame_bytes = self.clock.frame_bytes();
        let mut stopped = false;
        while let Ok(n) = read_full(&mut stdout, bytes) {
            let frames = n / frame_bytes;
            if frames == 0 {
                break;
            }
            let end = shared
                .progress
                .wait_clear(pass, self.clock.end_after(frames));
            let keep = self.clock.fit(frames, end.map(|e| self.clock.frame_at(e)));
            if keep > 0 && !self.emit(shared, decode_f32le(&bytes[..keep * frame_bytes])) {
                stopped = true;
                break;
            }
            // Stop at end of input, or when the video's pass ended first and
            // the rest of this pass is to be discarded.
            if keep < frames || n < bytes.len() {
                break;
            }
        }
        // Whether it finished, was trimmed or is stopping, this decoder is
        // done; its exit status says nothing the video worker cannot. It dies
        // before its pipe closes (see `crate::process`).
        shared.children.kill(key);
        drop(stdout);
        !stopped
    }

    /// Emits silence until the timeline reaches sample `limit`. Returns
    /// `false` when stopped.
    fn pad_to(&mut self, shared: &Shared, limit: u64) -> bool {
        let channels = self.clock.channels as usize;
        while self.clock.pos < limit {
            let frames = self.clock.fit(self.clock.block_frames(), Some(limit));
            let silence: Arc<[f32]> = std::iter::repeat_n(0.0, frames * channels).collect();
            if !self.emit(shared, silence) {
                return false;
            }
        }
        true
    }

    /// Waits until the block is due, then queues it. Returns `false` when
    /// stopped.
    fn emit(&mut self, shared: &Shared, samples: Arc<[f32]>) -> bool {
        let frames = samples.len() / self.clock.channels as usize;
        if !shared
            .timeline
            .wait_until(self.clock.end_after(frames), &shared.stop)
        {
            return false;
        }
        shared.audio.push(self.clock.block(&self.source, samples));
        true
    }
}

/// Decodes little-endian `f32` samples; a trailing partial sample is ignored.
pub(crate) fn decode_f32le(bytes: &[u8]) -> Arc<[f32]> {
    bytes
        .chunks_exact(SAMPLE_BYTES)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block of `n` samples tagged `tag` in its first sample.
    fn block(tag: f32, n: usize) -> AudioBlock {
        let mut samples = vec![0.0; n];
        samples[0] = tag;
        AudioBlock {
            source: SourceId("s".into()),
            pts: Duration::ZERO,
            sample_rate: 48_000,
            channels: 1,
            samples: Arc::from(samples),
        }
    }

    #[test]
    fn queue_drains_in_order() {
        let q = AudioQueue::new(4);
        q.push(block(1.0, 1));
        q.push(block(2.0, 1));
        let tags: Vec<f32> = q.drain().iter().map(|b| b.samples[0]).collect();
        assert_eq!(tags, [1.0, 2.0]);
        assert!(q.drain().is_empty());
        assert_eq!(q.dropped(), 0);
    }

    #[test]
    fn full_queue_drops_oldest_and_counts() {
        let q = AudioQueue::new(2);
        for tag in 1..=5 {
            q.push(block(tag as f32, 1));
        }
        let tags: Vec<f32> = q.drain().iter().map(|b| b.samples[0]).collect();
        assert_eq!(tags, [4.0, 5.0]);
        assert_eq!(q.dropped(), 3);
    }

    #[test]
    fn zero_capacity_still_keeps_the_newest_block() {
        let q = AudioQueue::new(0);
        q.push(block(1.0, 1));
        q.push(block(2.0, 1));
        assert_eq!(q.drain()[0].samples[0], 2.0);
        assert_eq!(q.dropped(), 1);
    }

    #[test]
    fn blocks_are_ten_milliseconds() {
        assert_eq!(SampleClock::new(48_000, 2).block_frames(), 480);
        assert_eq!(SampleClock::new(44_100, 1).block_frames(), 441);
        assert_eq!(SampleClock::new(22_050, 1).block_frames(), 220);
        assert_eq!(SampleClock::new(50, 1).block_frames(), 1);
        assert_eq!(SampleClock::new(48_000, 2).block_bytes(), 480 * 2 * 4);
        assert_eq!(SampleClock::new(0, 0).block_frames(), 1);
    }

    #[test]
    fn block_pts_are_contiguous_and_exact() {
        let mut c = SampleClock::new(44_100, 2);
        let src = SourceId("s".into());
        let mut end = Duration::ZERO;
        for _ in 0..100 {
            let b = c.block(&src, Arc::from(vec![0.0f32; 441 * 2]));
            assert_eq!(b.pts, end);
            assert_eq!(b.frames(), 441);
            end = c.time_of(c.pos);
        }
        assert_eq!(end, Duration::from_secs(1));
    }

    #[test]
    fn time_and_sample_conversions_round_trip() {
        let c = SampleClock::new(48_000, 1);
        assert_eq!(c.frame_at(Duration::from_secs(1)), 48_000);
        assert_eq!(c.frame_at(c.time_of(12_345)), 12_345);
        let ntsc_frame = Duration::from_nanos(33_366_666);
        assert_eq!(c.frame_at(ntsc_frame), 1602);
    }

    #[test]
    fn fit_trims_at_the_limit() {
        let mut c = SampleClock::new(1000, 1);
        assert_eq!(c.fit(10, None), 10);
        assert_eq!(c.fit(10, Some(4)), 4);
        c.block(&SourceId("s".into()), Arc::from(vec![0.0f32; 6]));
        assert_eq!(c.fit(10, Some(4)), 0);
        assert_eq!(c.end_after(4), Duration::from_millis(10));
    }

    #[test]
    fn decodes_little_endian_floats() {
        let mut bytes: Vec<u8> = [0.5f32, -1.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        bytes.push(0xff);
        assert_eq!(&decode_f32le(&bytes)[..], &[0.5, -1.0]);
        assert!(decode_f32le(&[]).is_empty());
    }
}
