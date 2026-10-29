//! Media types shared by sources, vision, compositing and output.
//!
//! Every frame carries its provenance: the [`SourceId`] it came from, the
//! connection *epoch* it was captured in, a per-source sequence number that
//! increases by one per delivered frame, and a presentation timestamp on the
//! source's media clock. Downstream stages use these to drop stale work and
//! to align observations with the picture they were computed from.
//!
//! # Epochs
//!
//! A source that loses its input and is opened again (the supervisor in
//! `ac-sources` does this) starts a new epoch: epoch 0 is its first
//! connection, and each reconnection counts one up. The picture of a new
//! epoch may come from a camera that was moved, replugged or swapped, so
//! anything computed on a frame of an older epoch, such as a detection,
//! describes a picture that is gone. The epoch is plain data on the frame;
//! consumers only compare it. Across epochs the sequence number and the PTS
//! keep increasing, so "newer" is still a comparison of sequence numbers,
//! and the time a source was offline counts as elapsed media time.
//!
//! Pixels are packed RGBA8 in row-major order with no row padding. Audio is
//! interleaved `f32` in `-1.0..=1.0`.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use ac_core::SourceId;
use thiserror::Error;

/// A decoded video frame.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Source that produced the frame.
    pub source: SourceId,
    /// Connection epoch of the source: 0 for its first connection, one more
    /// for each reconnection (see the module docs).
    pub epoch: u64,
    /// Per-source sequence number, starting at 0; it keeps increasing across
    /// epochs.
    pub seq: u64,
    /// Presentation time on the source's media clock.
    pub pts: Duration,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Packed RGBA8 pixels, `width * height * 4` bytes.
    pub data: Arc<[u8]>,
}

impl VideoFrame {
    /// Creates a frame of epoch 0, checking that `data` matches the
    /// dimensions.
    pub fn new(
        source: SourceId,
        seq: u64,
        pts: Duration,
        width: u32,
        height: u32,
        data: Arc<[u8]>,
    ) -> Result<Self, MediaError> {
        let expected = width as usize * height as usize * 4;
        if width == 0 || height == 0 || data.len() != expected {
            return Err(MediaError::BadFrameSize {
                width,
                height,
                len: data.len(),
            });
        }
        Ok(Self {
            source,
            epoch: 0,
            seq,
            pts,
            width,
            height,
            data,
        })
    }

    /// The same frame tagged with connection epoch `epoch`.
    pub fn with_epoch(self, epoch: u64) -> Self {
        Self { epoch, ..self }
    }

    /// Width divided by height.
    pub fn aspect(&self) -> f64 {
        self.width as f64 / self.height as f64
    }

    /// RGBA value at pixel `(x, y)`; `None` outside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        Some([
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ])
    }
}

/// A block of interleaved audio samples.
#[derive(Debug, Clone)]
pub struct AudioBlock {
    /// Source that produced the audio.
    pub source: SourceId,
    /// Presentation time of the first sample.
    pub pts: Duration,
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
    /// Interleaved samples, a multiple of `channels` long.
    pub samples: Arc<[f32]>,
}

impl AudioBlock {
    /// Number of sample frames (samples per channel).
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    /// Duration of the block.
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.frames() as f64 / self.sample_rate as f64)
    }
}

/// Static description of a source.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceInfo {
    /// Source identity.
    pub id: SourceId,
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
    /// Nominal frame rate.
    pub fps: f64,
    /// Audio sample rate and channel count, when the source carries audio.
    pub audio: Option<(u32, u16)>,
}

/// Health of a running source.
///
/// A plain source only moves forward, `Starting -> Running -> Stopped`. A
/// supervised one (`ac-sources`) that loses its input goes `Offline` and
/// `Reconnecting` instead of stopping, and back to `Running` under a new
/// epoch once the input delivers again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// Starting up; no frame yet.
    Starting,
    /// Delivering frames.
    Running,
    /// Not delivering: the input could not be opened, or was lost, and the
    /// next attempt to open it waits out a backoff delay.
    Offline,
    /// Not delivering: an attempt to open the input again is in progress
    /// (it is being opened, or it is open and its first frame is awaited).
    Reconnecting,
    /// Stopped after end of input, an error or a stop request; a stopped
    /// source never delivers again.
    Stopped,
}

/// Errors from media handling.
#[derive(Debug, Error)]
pub enum MediaError {
    /// Frame data does not match its dimensions.
    #[error("frame data of {len} bytes does not match {width}x{height} RGBA")]
    BadFrameSize {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Length of the data.
        len: usize,
    },
    /// The input could not be opened or probed.
    #[error("cannot open source: {0}")]
    Open(String),
    /// An I/O error while reading or writing media.
    #[error("media I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// A running video source.
///
/// Implementations decode on their own thread and publish into a
/// [`LatestFrame`] mailbox, so readers never wait and never see a backlog.
pub trait VideoSource: Send {
    /// Static description, fixed when the source started.
    fn info(&self) -> &SourceInfo;
    /// The description of what the source delivers now. A plain source's
    /// never changes, so this is [`VideoSource::info`]; a supervised one
    /// (`ac-sources`) describes its newest connection, which may differ from
    /// the description it started with, such as the caller's placeholder of
    /// an input that was not there yet.
    fn current_info(&self) -> SourceInfo {
        self.info().clone()
    }
    /// Current health.
    fn state(&self) -> SourceState;
    /// The newest decoded frame, if any.
    fn latest(&self) -> Option<VideoFrame>;
    /// Waits up to `timeout` (real time) for a frame newer than sequence
    /// number `after` (any frame when `None`) and returns the newest frame,
    /// or `None` when no newer one arrived in time.
    fn wait_newer(&self, after: Option<u64>, timeout: Duration) -> Option<VideoFrame>;
    /// Audio decoded since the previous call, oldest first.
    fn drain_audio(&self) -> Vec<AudioBlock>;
    /// The error behind the current state, if an error is the reason: what
    /// stopped the source, or what keeps a supervised source offline.
    fn error(&self) -> Option<String>;
    /// Stops decoding and releases the input. Idempotent.
    fn stop(&mut self);
}

/// Single-slot mailbox that always holds the newest frame.
///
/// Writers replace the slot; readers clone the `Arc`-backed frame. A slow
/// reader therefore skips frames instead of building a queue, which is the
/// correct behaviour for live video.
///
/// A reader that must pass frames on as soon as they arrive (a supervisor
/// forwarding a source's frames) waits in [`LatestFrame::wait_newer`]
/// instead of polling: every publish wakes it. That wait is in real time,
/// since it is a hand-off between threads, not media timing. The lock is
/// held only to swap or clone the frame, and a poisoned lock is recovered:
/// the slot holds plain data that is valid whatever a panicking holder was
/// doing.
#[derive(Debug, Default, Clone)]
pub struct LatestFrame {
    /// The newest frame and its publish signal, shared by the writer and
    /// every reader clone.
    slot: Arc<FrameSlot>,
}

/// Contents of a [`LatestFrame`].
#[derive(Debug, Default)]
struct FrameSlot {
    /// The newest frame.
    frame: Mutex<Option<VideoFrame>>,
    /// Signalled on every publish.
    published: Condvar,
}

impl LatestFrame {
    /// An empty mailbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the slot, recovering it if a holder panicked.
    fn lock(&self) -> MutexGuard<'_, Option<VideoFrame>> {
        self.slot.frame.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publishes `frame`, replacing any unread frame, and wakes waiters.
    pub fn publish(&self, frame: VideoFrame) {
        *self.lock() = Some(frame);
        self.slot.published.notify_all();
    }

    /// The newest frame, if one was published.
    pub fn get(&self) -> Option<VideoFrame> {
        self.lock().clone()
    }

    /// Waits up to `timeout` for a frame whose sequence number is above
    /// `after` (any frame when `None`) and returns the newest frame, or
    /// `None` when none such was published in time.
    pub fn wait_newer(&self, after: Option<u64>, timeout: Duration) -> Option<VideoFrame> {
        let newer = |slot: &Option<VideoFrame>| {
            slot.as_ref()
                .is_some_and(|f| after.is_none_or(|seq| f.seq > seq))
        };
        let guard = self.lock();
        let (guard, _) = self
            .slot
            .published
            .wait_timeout_while(guard, timeout, |slot| !newer(slot))
            .unwrap_or_else(|e| e.into_inner());
        guard.clone().filter(|_| newer(&guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64) -> VideoFrame {
        VideoFrame::new(
            SourceId("cam".into()),
            seq,
            Duration::ZERO,
            2,
            1,
            Arc::from(vec![0u8; 8]),
        )
        .unwrap()
    }

    #[test]
    fn frame_rejects_mismatched_data() {
        let bad = VideoFrame::new(
            SourceId("c".into()),
            0,
            Duration::ZERO,
            2,
            2,
            Arc::from(vec![0u8; 8]),
        );
        assert!(matches!(bad, Err(MediaError::BadFrameSize { .. })));
    }

    #[test]
    fn pixel_reads_rgba_and_bounds() {
        let data: Vec<u8> = (0..8).collect();
        let f = VideoFrame::new(
            SourceId("c".into()),
            0,
            Duration::ZERO,
            2,
            1,
            Arc::from(data),
        )
        .unwrap();
        assert_eq!(f.pixel(1, 0), Some([4, 5, 6, 7]));
        assert_eq!(f.pixel(2, 0), None);
    }

    #[test]
    fn mailbox_keeps_only_newest() {
        let m = LatestFrame::new();
        assert!(m.get().is_none());
        m.publish(frame(1));
        m.publish(frame(2));
        assert_eq!(m.get().unwrap().seq, 2);
    }

    #[test]
    fn frames_start_in_epoch_zero_and_can_be_retagged() {
        let f = frame(4);
        assert_eq!(f.epoch, 0);
        let g = f.clone().with_epoch(3);
        assert_eq!((g.epoch, g.seq, g.pts), (3, f.seq, f.pts));
        assert!(Arc::ptr_eq(&g.data, &f.data), "pixels are shared");
    }

    #[test]
    fn wait_newer_returns_a_newer_frame_at_once() {
        let m = LatestFrame::new();
        m.publish(frame(5));
        let zero = Duration::ZERO;
        assert_eq!(m.wait_newer(None, zero).unwrap().seq, 5);
        assert_eq!(m.wait_newer(Some(4), zero).unwrap().seq, 5);
        assert!(m.wait_newer(Some(5), zero).is_none(), "not newer");
        assert!(m.wait_newer(Some(9), zero).is_none(), "older");
        assert!(LatestFrame::new().wait_newer(None, zero).is_none(), "empty");
    }

    #[test]
    fn wait_newer_wakes_on_publish_and_times_out_without_one() {
        let m = LatestFrame::new();
        m.publish(frame(1));
        let started = std::time::Instant::now();
        assert!(m.wait_newer(Some(1), Duration::from_millis(30)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(30));
        let writer = m.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            writer.publish(frame(2));
        });
        let got = m.wait_newer(Some(1), Duration::from_secs(30));
        assert_eq!(got.map(|f| f.seq), Some(2));
        t.join().unwrap();
    }

    #[test]
    fn audio_block_duration() {
        let b = AudioBlock {
            source: SourceId("c".into()),
            pts: Duration::ZERO,
            sample_rate: 48_000,
            channels: 2,
            samples: Arc::from(vec![0.0f32; 96_000]),
        };
        assert_eq!(b.frames(), 48_000);
        assert_eq!(b.duration(), Duration::from_secs(1));
    }
}
