//! A fake audio input that drifts, stalls and gets unplugged on a plan, for
//! testing what reads a live input.
//!
//! A file paced on the engine's clock delivers exactly its nominal rate. A
//! real device has three habits a file does not, and a [`FakeAudio`] plan
//! gives the fake each of them, on the clock it is paced by, measured from
//! the moment it starts:
//!
//! * **Drift.** Its sample clock runs `drift_ppm` parts per million fast
//!   (or slow, when negative) against the pacing clock: after `t` seconds
//!   it has captured `t · rate · (1 + ppm · 10⁻⁶)` sample frames. Block `k`
//!   (sample frames `k·B` to `(k+1)·B`) is complete when its last frame is
//!   captured, and its PTS is the clock time of its first frame, which is
//!   what a device that stamps capture times on that clock reports. Its
//!   `sample_rate` stays the nominal one, as a device claims.
//! * **Stalls** ([`Gap`]). Blocks completed during a gap are not delivered
//!   then: a *held* gap delivers them at its end in one burst, as a driver
//!   flushing its buffer does, with their own PTS; a *lost* gap never
//!   delivers them, as an input that restarts fresh, so their sequence
//!   numbers and their time are missing from the stream.
//! * **Unplugging** (`fail_at`). The input stops with an error, so a
//!   supervisor opens it again; `end_at` stops it cleanly instead, as at
//!   the end of a file. Blocks delivered before either time are kept.
//!
//! Every sample is [`fake_sample`] of its channel and its frame index, a
//! value different on every channel of a frame and exact in 16-bit PCM,
//! so a test can tell which channels and which samples reached it, bit for
//! bit, and the same signal written to a file ([`crate::audio_input::wav`])
//! can be checked the same way.
//!
//! [`FakeFeed`] is the plan as a pure schedule stepped with clock readings,
//! for tests that drive time themselves; [`FakeAudioInput`] runs one on a
//! thread paced by a [`Clock`], as a device's capture thread would, into an
//! [`AudioRing`]. On a [`crate::VirtualClock`] it runs unpaced.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_core::SourceId;
use ac_media::{AudioBlock, AudioInput, AudioInputInfo, AudioRing, MediaError, SourceState};

use crate::clock::{Clock, sleep_until};
use crate::file::DEFAULT_AUDIO_QUEUE_BLOCKS;
use crate::status::Status;

/// The value of sample frame `frame` on channel `channel`: a multiple of
/// 2⁻¹⁵ in `-1.0..1.0` (exact in 16-bit PCM), different on every channel of
/// a frame and on every frame of a channel within 65 536 frames, after
/// which it repeats.
pub fn fake_sample(channel: u16, frame: u64) -> f32 {
    let step = (u64::from(channel) * 4099 + frame) % 65_536;
    (step as f32 - 32_768.0) / 32_768.0
}

/// A span of clock time, `from` inclusive to `until` exclusive, in which a
/// fake input delivers nothing; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// When the stall begins, since the input started.
    pub from: Duration,
    /// When it ends.
    pub until: Duration,
    /// Blocks completed meanwhile arrive at `until` (true) or never
    /// (false).
    pub held: bool,
}

impl Gap {
    /// True when the gap holds clock time `t`.
    pub fn covers(&self, t: Duration) -> bool {
        self.from <= t && t < self.until
    }
}

/// What a fake input does; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeAudio {
    /// Nominal samples per second per channel; above zero.
    pub sample_rate: u32,
    /// Interleaved channels; above zero.
    pub channels: u16,
    /// Sample frames per block; above zero.
    pub block_frames: usize,
    /// How fast its sample clock runs against the pacing clock, in parts
    /// per million; at most [`FakeAudio::MAX_DRIFT_PPM`] either way.
    pub drift_ppm: f64,
    /// Stalls.
    pub gaps: Vec<Gap>,
    /// When it is unplugged, since it started.
    pub fail_at: Option<Duration>,
    /// When it ends cleanly, since it started.
    pub end_at: Option<Duration>,
}

impl FakeAudio {
    /// Most drift a plan may have, either way: a tenth, far beyond any
    /// crystal.
    pub const MAX_DRIFT_PPM: f64 = 100_000.0;

    /// A steady input: 10 ms blocks, no drift, no gaps, never unplugged.
    pub fn steady(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
            block_frames: (sample_rate as usize / 100).max(1),
            drift_ppm: 0.0,
            gaps: Vec::new(),
            fail_at: None,
            end_at: None,
        }
    }

    /// Checks that the plan can be played: a rate, channels and a block
    /// size above zero, a finite drift within the limit, and gaps that end
    /// after they begin.
    pub fn check(&self) -> Result<(), MediaError> {
        let bad = |why: String| Err(MediaError::Open(format!("fake audio: {why}")));
        if self.sample_rate == 0 || self.channels == 0 || self.block_frames == 0 {
            return bad(format!(
                "{} Hz, {} channels and {}-frame blocks: each must be above zero",
                self.sample_rate, self.channels, self.block_frames
            ));
        }
        if !self.drift_ppm.is_finite() || self.drift_ppm.abs() > Self::MAX_DRIFT_PPM {
            return bad(format!(
                "drift of {} ppm is outside ±{}",
                self.drift_ppm,
                Self::MAX_DRIFT_PPM
            ));
        }
        if let Some(g) = self.gaps.iter().find(|g| g.until <= g.from) {
            return bad(format!("gap {g:?} ends before it begins"));
        }
        Ok(())
    }
}

/// Where a [`FakeFeed`] stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedState {
    /// Delivering.
    Live,
    /// Ended cleanly (`end_at`).
    Ended,
    /// Unplugged (`fail_at`).
    Unplugged,
}

/// A [`FakeAudio`] plan as a pure schedule; see the module docs.
#[derive(Debug, Clone)]
pub struct FakeFeed {
    /// Tagged on every block.
    id: SourceId,
    /// What it does.
    plan: FakeAudio,
    /// The first block neither delivered nor lost yet.
    next: u64,
    /// Where it stands.
    state: FeedState,
}

impl FakeFeed {
    /// A feed of `plan` as input `id`, at its start.
    pub fn new(id: SourceId, plan: FakeAudio) -> Result<Self, MediaError> {
        plan.check()?;
        Ok(Self {
            id,
            plan,
            next: 0,
            state: FeedState::Live,
        })
    }

    /// The plan.
    pub fn plan(&self) -> &FakeAudio {
        &self.plan
    }

    /// Where it stands.
    pub fn state(&self) -> FeedState {
        self.state
    }

    /// Clock time at which sample frame `frame` is captured.
    pub fn time_of(&self, frame: u64) -> Duration {
        let rate = f64::from(self.plan.sample_rate) * (1.0 + self.plan.drift_ppm * 1e-6);
        Duration::from_secs_f64(frame as f64 / rate)
    }

    /// Clock time at which block `k` is complete.
    fn completes(&self, k: u64) -> Duration {
        self.time_of((k + 1) * self.plan.block_frames as u64)
    }

    /// When block `k` reaches the reader: when it is complete, or at the
    /// end of the held gap it completed in; `None` when a lost gap takes it.
    fn delivery(&self, k: u64) -> Option<Duration> {
        let mut at = self.completes(k);
        while let Some(gap) = self.plan.gaps.iter().find(|g| g.covers(at)) {
            if !gap.held {
                return None;
            }
            at = gap.until;
        }
        Some(at)
    }

    /// When the plan stops the input, if it does, and how.
    fn stop(&self) -> Option<(Duration, FeedState)> {
        let fail = self.plan.fail_at.map(|t| (t, FeedState::Unplugged));
        let end = self.plan.end_at.map(|t| (t, FeedState::Ended));
        match (fail, end) {
            (Some(f), Some(e)) => Some(if f.0 <= e.0 { f } else { e }),
            (f, e) => f.or(e),
        }
    }

    /// Clock time at which the feed next changes: its next block's
    /// delivery, or its stop when that comes first; `None` once stopped.
    pub fn next_due(&self) -> Option<Duration> {
        if self.state != FeedState::Live {
            return None;
        }
        let mut k = self.next;
        let block = loop {
            match self.delivery(k) {
                Some(at) => break at,
                None => k += 1,
            }
        };
        Some(match self.stop() {
            Some((at, _)) if at <= block => at,
            _ => block,
        })
    }

    /// The blocks delivered by clock time `elapsed`, oldest first; from the
    /// stop on, nothing.
    pub fn until(&mut self, elapsed: Duration) -> Vec<AudioBlock> {
        let mut out = Vec::new();
        if self.state != FeedState::Live {
            return out;
        }
        let stop = self.stop();
        loop {
            let k = self.next;
            let delivered = self.delivery(k);
            let at = delivered.unwrap_or_else(|| self.completes(k));
            if at > elapsed || stop.is_some_and(|(t, _)| t <= at) {
                break;
            }
            if delivered.is_some() {
                out.push(self.block(k));
            }
            self.next += 1;
        }
        if let Some((t, state)) = stop
            && t <= elapsed
        {
            self.state = state;
        }
        out
    }

    /// Block `k`.
    fn block(&self, k: u64) -> AudioBlock {
        let frames = self.plan.block_frames as u64;
        let channels = self.plan.channels;
        let first = k * frames;
        let samples: Arc<[f32]> = (first..first + frames)
            .flat_map(|n| (0..channels).map(move |c| fake_sample(c, n)))
            .collect();
        AudioBlock {
            source: self.id.clone(),
            seq: k,
            pts: self.time_of(first),
            sample_rate: self.plan.sample_rate,
            channels,
            samples,
        }
    }
}

/// State shared by a [`FakeAudioInput`] and its thread.
struct Shared {
    /// Raised once by `stop`.
    stop: AtomicBool,
    /// Health and the stopping error.
    status: Status,
    /// Blocks awaiting `drain`.
    ring: AudioRing,
}

/// A [`FakeFeed`] played on its own thread as a live input; see the module
/// docs.
pub struct FakeAudioInput {
    /// What it delivers.
    info: AudioInputInfo,
    /// State shared with the thread.
    shared: Arc<Shared>,
    /// The thread, joined by `stop`.
    worker: Option<JoinHandle<()>>,
}

impl fmt::Debug for FakeAudioInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeAudioInput")
            .field("info", &self.info)
            .field("state", &self.shared.status.get())
            .finish_non_exhaustive()
    }
}

impl FakeAudioInput {
    /// Starts playing `plan` as input `id`, paced by `clock` from now.
    pub fn start(id: SourceId, plan: FakeAudio, clock: Arc<dyn Clock>) -> Result<Self, MediaError> {
        let mut feed = FakeFeed::new(id.clone(), plan)?;
        let info = AudioInputInfo {
            id: id.clone(),
            format: Some((feed.plan.sample_rate, feed.plan.channels)),
        };
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            status: Status::new(),
            ring: AudioRing::new(DEFAULT_AUDIO_QUEUE_BLOCKS),
        });
        let state = shared.clone();
        let worker = std::thread::Builder::new()
            .name(format!("ac-fake-audio-{}", id.0))
            .spawn(move || {
                let origin = clock.now();
                while let Some(due) = feed.next_due() {
                    if !sleep_until(&*clock, origin.saturating_add(due), &state.stop) {
                        break;
                    }
                    for block in feed.until(clock.now().saturating_sub(origin)) {
                        state.ring.push(block);
                        state.status.advance(SourceState::Running);
                    }
                }
                if feed.state() == FeedState::Unplugged {
                    state.status.fail("the fake input was unplugged");
                }
                state.status.advance(SourceState::Stopped);
            })?;
        Ok(Self {
            info,
            shared,
            worker: Some(worker),
        })
    }
}

impl AudioInput for FakeAudioInput {
    fn info(&self) -> &AudioInputInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        self.shared.status.get()
    }

    fn drain(&self) -> Vec<AudioBlock> {
        self.shared.ring.drain()
    }

    fn wait(&self, timeout: Duration) -> bool {
        self.shared.ring.wait(timeout)
    }

    fn dropped(&self) -> u64 {
        self.shared.ring.dropped()
    }

    fn error(&self) -> Option<String> {
        self.shared.status.error()
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.shared.status.fail("the fake input's thread panicked");
        }
        self.shared.status.advance(SourceState::Stopped);
    }
}

impl Drop for FakeAudioInput {
    fn drop(&mut self) {
        self.stop();
    }
}
