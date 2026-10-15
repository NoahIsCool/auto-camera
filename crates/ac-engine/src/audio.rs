//! Program audio pacing.
//!
//! The encoder timestamps audio by sample count and video by frame count, so
//! the program stays in sync exactly when, after `n` video frames at `fps`,
//! about `n · rate / fps` audio sample frames have been pushed. Program audio
//! follows one configured source (its embedded audio), which is paced by the
//! same clock as its video but arrives in 10 ms blocks with some jitter, may
//! stall, and may burst after a stall.
//!
//! [`AudioPacer`] turns the blocks drained on each tick into what to push:
//!
//! * a block is pushed unless it would put audio more than `max_lead` ahead
//!   of video, in which case it is dropped (a burst after a stall);
//! * when audio falls more than `slack` behind video (the source stalled or
//!   stopped), silence is pushed up to the exact target, because ffmpeg
//!   interleaves by timestamp and would otherwise hold the video back;
//! * blocks in another format than the encoder was opened with are dropped.
//!
//! The slack keeps ordinary block jitter from inserting silence (which would
//! turn into permanent audio latency), and the lead limit bounds how far a
//! burst can push audio late. Both are counted for the operator.

use std::time::Duration;

use ac_media::AudioBlock;

/// One item to push to the encoder.
#[derive(Debug, Clone)]
pub enum PacedAudio {
    /// Push this source block as is.
    Block(AudioBlock),
    /// Push this many sample frames of silence.
    Silence(u64),
}

/// Counters kept by an [`AudioPacer`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioPacerStats {
    /// Sample frames of source audio pushed.
    pub pushed: u64,
    /// Sample frames of silence pushed.
    pub silence: u64,
    /// Sample frames dropped because audio ran too far ahead.
    pub dropped: u64,
    /// Blocks dropped because their format did not match.
    pub mismatched_blocks: u64,
}

/// Keeps program audio in step with program video; see the module docs.
#[derive(Debug, Clone)]
pub struct AudioPacer {
    /// Encoder sample rate.
    rate: u32,
    /// Encoder channel count.
    channels: u16,
    /// Video frames per second.
    fps: u32,
    /// Largest deficit tolerated before silence is inserted, in sample frames.
    slack: u64,
    /// Largest lead over video, in sample frames.
    max_lead: u64,
    /// Counters.
    stats: AudioPacerStats,
}

impl AudioPacer {
    /// A pacer for `rate` Hz, `channels`-channel audio under `fps` video.
    pub fn new(rate: u32, channels: u16, fps: u32, slack: Duration, max_lead: Duration) -> Self {
        let frames = |d: Duration| (d.as_secs_f64() * f64::from(rate)).round() as u64;
        Self {
            rate,
            channels,
            fps: fps.max(1),
            slack: frames(slack),
            max_lead: frames(max_lead),
            stats: AudioPacerStats::default(),
        }
    }

    /// The counters.
    pub fn stats(&self) -> AudioPacerStats {
        self.stats
    }

    /// Sample frames that belong with `video_frames` frames of video.
    pub fn target(&self, video_frames: u64) -> u64 {
        let t = u128::from(video_frames) * u128::from(self.rate) / u128::from(self.fps);
        u64::try_from(t).unwrap_or(u64::MAX)
    }

    /// Plans what to push after `video_frames` frames of video, given the
    /// source blocks drained this tick (oldest first).
    pub fn plan(&mut self, video_frames: u64, blocks: Vec<AudioBlock>) -> Vec<PacedAudio> {
        let target = self.target(video_frames);
        let total = |s: &AudioPacerStats| s.pushed + s.silence;
        let mut out = Vec::new();
        for block in blocks {
            if block.sample_rate != self.rate || block.channels != self.channels {
                self.stats.mismatched_blocks += 1;
                continue;
            }
            let n = block.frames() as u64;
            if total(&self.stats) + n > target + self.max_lead {
                self.stats.dropped += n;
                continue;
            }
            self.stats.pushed += n;
            out.push(PacedAudio::Block(block));
        }
        let have = total(&self.stats);
        if have + self.slack < target {
            let gap = target - have;
            self.stats.silence += gap;
            out.push(PacedAudio::Silence(gap));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::SourceId;

    /// A block of `frames` sample frames.
    fn block(frames: usize, rate: u32, channels: u16) -> AudioBlock {
        AudioBlock {
            source: SourceId("a".into()),
            pts: Duration::ZERO,
            sample_rate: rate,
            channels,
            samples: vec![0.1f32; frames * channels as usize].into(),
        }
    }

    /// A 48 kHz stereo pacer for 30 fps video.
    fn pacer() -> AudioPacer {
        AudioPacer::new(
            48_000,
            2,
            30,
            Duration::from_millis(100),
            Duration::from_millis(500),
        )
    }

    #[test]
    fn steady_audio_passes_through_without_silence() {
        let mut p = pacer();
        for tick in 1..=300u64 {
            // 1600 frames per 30 fps tick, delivered as one block per tick.
            let out = p.plan(tick, vec![block(1600, 48_000, 2)]);
            assert_eq!(out.len(), 1);
            assert!(matches!(out[0], PacedAudio::Block(_)));
        }
        assert_eq!(p.stats().pushed, p.target(300));
        assert_eq!(p.stats().silence, 0);
    }

    #[test]
    fn a_stalled_source_is_filled_with_silence_to_the_target() {
        let mut p = pacer();
        // 100 ms of slack: three ticks (100 ms) of nothing is tolerated.
        assert!(p.plan(3, vec![]).is_empty());
        let out = p.plan(4, vec![]);
        assert!(matches!(out.as_slice(), [PacedAudio::Silence(6400)]));
        assert_eq!(p.stats().silence, p.target(4));
    }

    #[test]
    fn a_burst_beyond_the_lead_limit_is_dropped() {
        let mut p = pacer();
        let burst: Vec<AudioBlock> = (0..100).map(|_| block(480, 48_000, 2)).collect();
        p.plan(1, burst);
        let s = p.stats();
        // At most target + 500 ms may be pushed.
        assert!(s.pushed <= p.target(1) + 24_000);
        assert!(s.dropped > 0);
        assert_eq!(s.pushed + s.dropped, 48_000);
    }

    #[test]
    fn mismatched_formats_are_dropped_and_counted() {
        let mut p = pacer();
        let out = p.plan(1, vec![block(480, 44_100, 2), block(480, 48_000, 1)]);
        assert!(out.is_empty());
        assert_eq!(p.stats().mismatched_blocks, 2);
    }

    #[test]
    fn target_is_exact_for_long_runs() {
        let p = pacer();
        assert_eq!(p.target(0), 0);
        assert_eq!(p.target(30), 48_000);
        assert_eq!(p.target(30 * 3600 * 24), 48_000 * 3600 * 24);
        let odd = AudioPacer::new(44_100, 2, 30, Duration::ZERO, Duration::ZERO);
        assert_eq!(odd.target(1), 1470);
    }
}
