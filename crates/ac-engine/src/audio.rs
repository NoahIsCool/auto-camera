//! Program audio pacing.
//!
//! The encoder timestamps audio by sample count and video by frame count, so
//! the program stays in sync exactly when, after `n` video frames at `fps`,
//! about `n · rate / fps` audio sample frames (the *target*) have been
//! pushed. Program audio follows one configured origin (a source's embedded
//! audio, or an audio input; [`crate::program_audio`]), which is paced by
//! the same clock as the video but arrives in 10 ms blocks with some
//! jitter, may stall, catches up in a burst after a stall, and (a device's
//! own sample clock) may drift. An audio input's drift is resampled away
//! before its blocks get here ([`crate::program_audio`]); drift beyond the
//! correction's limit is absorbed below like any shortfall or surplus.
//!
//! # What decides lip sync
//!
//! Video is always the newest picture (latest-frame mailboxes), so it never
//! runs late. Audio is a stream: every sample frame pushed moves the audio
//! timeline on by one, whatever it holds. Write the *lead* as
//! `L = pushed − target` (silence counts as pushed) and the *backlog* `B`
//! for audio the source produced but has not delivered yet. Relative to the
//! start of the session, audio then plays late by
//!
//! ```text
//! lag = silence inserted − source audio dropped = L + B
//! ```
//!
//! because silence pushes everything after it later and dropping source
//! audio pulls it earlier. Once a burst has been delivered (`B = 0`) the lag
//! *is* the lead. A pacer that only bounds the lead from above, keeping the
//! oldest audio of a burst, leaves the program late by up to that bound for
//! the rest of the show. This pacer drives the lag back to zero instead:
//!
//! * **Gaps.** When the audio at hand (pushed plus drained this tick) is more
//!   than `slack` short of the target (the source stalled or stopped),
//!   silence fills up to the exact target, *before* this tick's blocks
//!   (the gap is in the past), because ffmpeg interleaves by timestamp and
//!   would otherwise hold the video back. The silence is remembered as owed.
//! * **Resuming.** The first audio after inserted silence is aligned to the
//!   target: a deficit, however small, is filled, so a source that stopped
//!   and restarted fresh does not play up to `slack` early.
//! * **Recovery.** While silence is owed, audio that would put the lead
//!   more than [`RECOVERY_TOLERANCE`] above zero is the stall's backlog
//!   arriving late. Its oldest blocks are dropped, up to the amount owed,
//!   until the newest audio lands on the target, so the lag returns to
//!   about zero. The tolerance keeps block jitter from being cut. A debt
//!   that sees no backlog for [`RECOVERY_WINDOW`] is forgiven: the source
//!   restarted fresh, and there is nothing late to cut.
//! * **Bursts.** Otherwise, blocks that would put the lead above `max_lead`
//!   are dropped, oldest first, so the newest audio is always kept and no
//!   burst can leave audio more than `max_lead` late.
//! * Blocks in another format than the encoder was opened with are dropped.
//!
//! The slack keeps ordinary block jitter from inserting silence and
//! `max_lead` keeps it from dropping audio; both are several blocks and
//! ticks long. The video frame count must include ticks the engine skipped
//! after falling far behind (the encoder repeats a frame for each), or the
//! target falls behind the source's audio by the skipped time.

use std::time::Duration;

use ac_media::AudioBlock;

/// How long owed silence waits for a stall's backlog (after the last
/// silence or the last cut) before it is forgiven.
pub const RECOVERY_WINDOW: Duration = Duration::from_secs(1);

/// How far ahead of video audio may run, while silence is owed, before the
/// late backlog is cut: more than a block of jitter, well inside what
/// viewers notice as lip-sync error.
pub const RECOVERY_TOLERANCE: Duration = Duration::from_millis(20);

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
    /// Sample frames of source audio dropped: late backlog cut to repay
    /// silence, and bursts beyond the lead limit.
    pub dropped: u64,
    /// Blocks dropped because their format did not match.
    pub mismatched_blocks: u64,
    /// Audio pushed minus the video's share after the latest tick, in
    /// milliseconds: positive while audio runs ahead.
    pub lead_ms: i64,
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
    /// [`RECOVERY_WINDOW`] in sample frames.
    recovery: u64,
    /// [`RECOVERY_TOLERANCE`] in sample frames.
    tolerance: u64,
    /// Silence inserted and not yet repaid by cutting late backlog.
    owed: u64,
    /// Target after which the owed silence is forgiven.
    owed_until: u64,
    /// The latest tick inserted silence and had no audio: the next audio
    /// that arrives resumes the stream.
    starved: bool,
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
            recovery: frames(RECOVERY_WINDOW),
            tolerance: frames(RECOVERY_TOLERANCE),
            owed: 0,
            owed_until: 0,
            starved: false,
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

    /// Plans what to push after `video_frames` frames of video (skipped ticks
    /// included), given the source blocks drained this tick (oldest first).
    pub fn plan(&mut self, video_frames: u64, blocks: Vec<AudioBlock>) -> Vec<PacedAudio> {
        let target = self.target(video_frames);
        if target > self.owed_until {
            self.owed = 0;
        }
        let mut kept = Vec::with_capacity(blocks.len());
        for block in blocks {
            if block.sample_rate != self.rate || block.channels != self.channels {
                self.stats.mismatched_blocks += 1;
            } else if block.frames() > 0 {
                kept.push(block);
            }
        }
        let have = self.stats.pushed + self.stats.silence;
        let mut incoming: u64 = kept.iter().map(|b| b.frames() as u64).sum();
        let resuming = self.starved && incoming > 0;
        let tolerated = if resuming { 0 } else { self.slack };
        let mut out = Vec::with_capacity(kept.len() + 1);
        if have + incoming + tolerated < target {
            let gap = target - have - incoming;
            self.stats.silence += gap;
            self.owed = self.owed.saturating_add(gap);
            self.owed_until = target.saturating_add(self.recovery);
            self.starved = incoming == 0;
            out.push(PacedAudio::Silence(gap));
        } else {
            if incoming > 0 {
                self.starved = false;
            }
            // Oldest first: the blocks to cut, and how many sample frames
            // remain after cutting them.
            let mut cut = 0;
            if self.owed > 0 && have + incoming > target.saturating_add(self.tolerance) {
                while cut < kept.len() && self.owed > 0 && have + incoming > target {
                    let n = kept[cut].frames() as u64;
                    (incoming, cut) = (incoming - n, cut + 1);
                    self.stats.dropped += n;
                    self.owed = self.owed.saturating_sub(n);
                    self.owed_until = target.saturating_add(self.recovery);
                }
            }
            while cut < kept.len() && have + incoming > target.saturating_add(self.max_lead) {
                let n = kept[cut].frames() as u64;
                (incoming, cut) = (incoming - n, cut + 1);
                self.stats.dropped += n;
            }
            kept.drain(..cut);
        }
        for block in kept {
            self.stats.pushed += block.frames() as u64;
            out.push(PacedAudio::Block(block));
        }
        let lead = (self.stats.pushed + self.stats.silence) as i128 - i128::from(target);
        self.stats.lead_ms = i64::try_from(lead * 1000 / i128::from(self.rate.max(1))).unwrap_or(0);
        out
    }
}

#[cfg(test)]
mod tests;
