//! Media types shared by sources, vision, compositing and output.
//!
//! Every frame carries its provenance: the [`SourceId`] it came from, a
//! per-source sequence number that increases by one per delivered frame, and a
//! presentation timestamp on the source's media clock. Downstream stages use
//! these to drop stale work and to align observations with the picture they
//! were computed from.
//!
//! Pixels are packed RGBA8 in row-major order with no row padding. Audio is
//! interleaved `f32` in `-1.0..=1.0`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ac_core::SourceId;
use thiserror::Error;

/// A decoded video frame.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Source that produced the frame.
    pub source: SourceId,
    /// Per-source sequence number, starting at 0.
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
    /// Creates a frame, checking that `data` matches the dimensions.
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
            seq,
            pts,
            width,
            height,
            data,
        })
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// Starting up; no frame yet.
    Starting,
    /// Delivering frames.
    Running,
    /// Stopped after end of input or an error.
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
    /// Static description.
    fn info(&self) -> &SourceInfo;
    /// Current health.
    fn state(&self) -> SourceState;
    /// The newest decoded frame, if any.
    fn latest(&self) -> Option<VideoFrame>;
    /// Audio decoded since the previous call, oldest first.
    fn drain_audio(&self) -> Vec<AudioBlock>;
    /// Stops decoding and releases the input. Idempotent.
    fn stop(&mut self);
}

/// Single-slot mailbox that always holds the newest frame.
///
/// Writers replace the slot; readers clone the `Arc`-backed frame. A slow
/// reader therefore skips frames instead of building a queue, which is the
/// correct behaviour for live video.
#[derive(Debug, Default, Clone)]
pub struct LatestFrame {
    slot: Arc<Mutex<Option<VideoFrame>>>,
}

impl LatestFrame {
    /// An empty mailbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes `frame`, replacing any unread frame.
    pub fn publish(&self, frame: VideoFrame) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
    }

    /// The newest frame, if one was published.
    pub fn get(&self) -> Option<VideoFrame> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
