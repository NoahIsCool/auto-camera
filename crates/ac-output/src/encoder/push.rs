//! Submission: queuing program frames and audio without ever blocking the
//! caller's tick.
//!
//! Each input has a bounded queue to its writer thread (`VIDEO_QUEUE_FRAMES`
//! frames, `AUDIO_QUEUE_BLOCKS` blocks). A push that finds its queue full
//! drops the input, counts it and adds it to the writer's debt, which the
//! writer repays with repeats of its last frame or with silence (see the
//! private `transport` module), so frame and sample counts, and with them
//! A/V sync, survive an encoder hiccup. Debts are capped at
//! `MAX_DEBT_SECONDS` of media. Input of the wrong size or format is
//! refused with an error and counts nowhere; input after the encoder
//! stopped is counted as dropped and answered with
//! [`OutputError::Stopped`].

use std::sync::atomic::Ordering;
use std::sync::mpsc::TrySendError;

use ac_media::{AudioBlock, VideoFrame};

use super::{ProgramEncoder, Push};
use crate::OutputError;
use crate::config::AudioFormat;
use crate::transport::{self, MAX_DEBT_SECONDS, VIDEO_QUEUE_FRAMES};

impl ProgramEncoder {
    /// Queues one RGBA frame of exactly `width * height * 4` bytes.
    ///
    /// Call once per program tick. Never blocks; see [`Push`].
    pub fn push_video(&self, rgba: &[u8]) -> Result<Push, OutputError> {
        let expected = self.config.frame_bytes();
        if rgba.len() != expected {
            return Err(OutputError::FrameSize {
                expected,
                actual: rgba.len(),
            });
        }
        let s = &self.shared;
        s.frames_in.fetch_add(1, Ordering::Relaxed);
        let tx = match &self.video_tx {
            Some(tx) if !s.video_closed.load(Ordering::SeqCst) => tx,
            _ => {
                s.frames_dropped.fetch_add(1, Ordering::Relaxed);
                return Err(OutputError::Stopped);
            }
        };
        // Cheap early drop: skip the copy when the queue is known full.
        if s.video_queued.load(Ordering::SeqCst) >= VIDEO_QUEUE_FRAMES {
            return Ok(self.drop_video());
        }
        let mut buf = s.take_buffer();
        buf.clear();
        buf.extend_from_slice(rgba);
        // Counted before sending so the writer's decrement cannot underflow.
        s.video_queued.fetch_add(1, Ordering::SeqCst);
        match tx.try_send(buf) {
            Ok(()) => Ok(Push::Queued),
            Err(TrySendError::Full(buf)) => {
                s.video_queued.fetch_sub(1, Ordering::SeqCst);
                s.recycle(buf);
                Ok(self.drop_video())
            }
            Err(TrySendError::Disconnected(_)) => {
                s.video_queued.fetch_sub(1, Ordering::SeqCst);
                s.frames_dropped.fetch_add(1, Ordering::Relaxed);
                Err(OutputError::Stopped)
            }
        }
    }

    /// Counts a dropped frame and owes a repeat for it.
    fn drop_video(&self) -> Push {
        let s = &self.shared;
        s.frames_dropped.fetch_add(1, Ordering::Relaxed);
        let cap = MAX_DEBT_SECONDS * u64::from(self.config.fps);
        transport::add_debt(&s.video_debt, 1, cap);
        Push::Dropped
    }

    /// Owes `n` repeats of the newest frame for program ticks the caller
    /// skipped (it fell far behind its clock), so the encoded timeline keeps
    /// pace with the caller's clock instead of falling behind it for good.
    ///
    /// The video writer emits the repeats (of the frame it wrote last) just
    /// before the next frame it takes from the queue, exactly like the
    /// repeats that repay dropped frames; they count
    /// in [`EncoderStats::frames_repeated`](super::EncoderStats::frames_repeated)
    /// and `frames_written`, not as
    /// drops. The debt is capped at `MAX_DEBT_SECONDS` of video (ten seconds
    /// at the configured rate), so a long suspension is not replayed in full.
    /// Returns how many repeats were owed: `n` unless the cap or a closed
    /// encoder limited it.
    pub fn repeat_frames(&self, n: u64) -> u64 {
        let s = &self.shared;
        if n == 0 || self.video_tx.is_none() || s.video_closed.load(Ordering::SeqCst) {
            return 0;
        }
        let cap = MAX_DEBT_SECONDS * u64::from(self.config.fps);
        let before = s
            .video_debt
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |d| {
                Some(d.saturating_add(n).min(cap))
            })
            .unwrap_or_else(|d| d);
        before.saturating_add(n).min(cap).saturating_sub(before)
    }

    /// Queues a composed program frame; it must match the output size.
    pub fn push_frame(&self, frame: &VideoFrame) -> Result<Push, OutputError> {
        let expected = (self.config.width, self.config.height);
        let actual = (frame.width, frame.height);
        if expected != actual {
            return Err(OutputError::FrameDimensions { expected, actual });
        }
        self.push_video(&frame.data)
    }

    /// Queues interleaved samples in the configured [`AudioFormat`].
    ///
    /// The length must be a whole number of sample frames. Never blocks.
    pub fn push_audio(&self, samples: &[f32]) -> Result<Push, OutputError> {
        let Some(format) = self.config.audio else {
            return Err(OutputError::AudioNotConfigured);
        };
        let channels = usize::from(format.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(OutputError::AudioLength {
                len: samples.len(),
                channels: format.channels,
            });
        }
        if samples.is_empty() {
            return Ok(Push::Queued);
        }
        let s = &self.shared;
        let n = samples.len() as u64;
        s.audio_samples_in.fetch_add(n, Ordering::Relaxed);
        let tx = match &self.audio_tx {
            Some(tx) if !s.audio_closed.load(Ordering::SeqCst) => tx,
            _ => {
                s.audio_samples_dropped.fetch_add(n, Ordering::Relaxed);
                return Err(OutputError::Stopped);
            }
        };
        match tx.try_send(samples.to_vec()) {
            Ok(()) => Ok(Push::Queued),
            Err(TrySendError::Full(_)) => {
                s.audio_samples_dropped.fetch_add(n, Ordering::Relaxed);
                let per_second = u64::from(format.sample_rate) * u64::from(format.channels);
                transport::add_debt(&s.audio_debt, n, MAX_DEBT_SECONDS * per_second);
                Ok(Push::Dropped)
            }
            Err(TrySendError::Disconnected(_)) => {
                s.audio_samples_dropped.fetch_add(n, Ordering::Relaxed);
                Err(OutputError::Stopped)
            }
        }
    }

    /// Queues an audio block; its rate and channels must match the config.
    pub fn push_audio_block(&self, block: &AudioBlock) -> Result<Push, OutputError> {
        let Some(expected) = self.config.audio else {
            return Err(OutputError::AudioNotConfigured);
        };
        let actual = AudioFormat {
            sample_rate: block.sample_rate,
            channels: block.channels,
        };
        if actual != expected {
            return Err(OutputError::AudioFormatMismatch { expected, actual });
        }
        self.push_audio(&block.samples)
    }
}
