//! Measuring how far an audio input's sample clock drifts from the master
//! clock, from its blocks' PTS: the half of drift correction that decides
//! how much to correct ([`crate::correct`] does the correcting).
//!
//! # What is measured
//!
//! An input on its own clock (a USB board's crystal) stamps each block with
//! the master-clock time of its first sample, its PTS, and delivers its
//! nominal `rate` frames per second of *its* clock. Over an *epoch*, a run
//! of blocks without a break, write `x` for the input time of a block's
//! first frame (frames since the epoch's first block over `rate`) and `T`
//! for its PTS since that block's. The **timing error** is
//!
//! ```text
//! u = x − T        (seconds; above zero while the input runs ahead)
//! ```
//!
//! A clock `ε` fast (`ε` = 100 ppm is 10⁻⁴) makes `T = x / (1 + ε)`, so `u`
//! grows along `x` with slope `v = ε / (1 + ε)`: the drift is `ε = v / (1 −
//! v)`, and the resampling ratio that puts the input back on the master
//! clock is `1 − v` output frames per input frame. A file played on the
//! master clock stamps its PTS from its sample count, so its `u` is zero
//! to the nanosecond and so is its drift.
//!
//! # Filtering jitter
//!
//! A device's PTS jitters by its driver's scheduling, a millisecond or so,
//! which is a lot against drift of 10⁻⁴ (a tenth of a millisecond per
//! second). The estimator fits a straight line `u ≈ û + v̂·(x − x_latest)`
//! to the epoch's blocks by least squares, each block weighed by the input
//! time it covers and by `e^(−age/τ)`, its age along input time over the
//! fit's memory `τ` ([`DriftSettings::smoothing`], 20 s): the
//! fading-memory polynomial filter of first order, solved exactly from the
//! epoch's first block on (so it has no start-up transient: exact PTS give
//! the exact slope from the second block, and blocks of any length are
//! weighed by the time they cover) and kept as running sums about the
//! latest block (so their values stay small over a show of any length).
//! Jitter of a millisecond leaves a few tenths of a part per million in
//! `v̂`.
//!
//! The fit also reports how far its slope can be trusted
//! ([`DriftEstimator::drift_uncertainty_ppm`]): the standard error of `v̂`,
//! from the scatter of the blocks about the predictions of the line fitted
//! before them (from the third block on: a prediction from one block has
//! no slope to go by). Early in an epoch, or with noisy PTS, it is large,
//! and the corrector does not take a slope for drift until the slope
//! stands clear of its dead band by several of them; a file's exact PTS
//! make it vanish.
//!
//! The estimator never looks at when a block *arrives*, only at its PTS and
//! its sample count, so a reader that drains once per tick, arrival jitter,
//! and the burst of late blocks after a stall do not enter it.
//!
//! # The timing reference
//!
//! `u` counts from the epoch's first block, whose PTS is one reading with
//! its own error, so that block cannot be what the timing error is
//! measured against: a first block stamped 30 ms off would read as 30 ms
//! of timing error for the whole epoch. Once the epoch has
//! [`REFERENCE_AFTER`] of input (a second, a hundred 10 ms blocks) the
//! fitted `û` is taken as the reference, and [`DriftEstimator::error`] is
//! `û` less it from then on (zero until then). One block's error moves the
//! fitted `û` at the far end of the fit by about twice its share, two
//! percent of it at a hundred blocks.
//!
//! # Breaks and steps
//!
//! A block whose residual (its `u` less where the fit put it) is larger
//! than [`DriftSettings::discontinuity`] (100 ms, the program pacer's
//! slack) is not drift but a break: a reconnection's new epoch (its PTS
//! moved on by the time the input was away), a long stall whose audio was
//! lost. It starts a new epoch, the estimates start again from zero, and
//! the reset is counted. A gap that big is the program pacer's to fill:
//! its slack runs out before the audio after the gap comes, and it inserts
//! silence up to the video.
//!
//! A smaller residual well clear of the PTS's jitter, beyond
//! [`DriftSettings::step`] (2 ms) and [`STEP_SIGMAS`] times the blocks'
//! scatter about their predictions, is a **step**
//! ([`Observation::Step`]): audio lost in a stall or dropped by a full
//! ring (the PTS step forward by its length), or PTS that went back. It is
//! timing error, not drift. The fit is cut there into *segments* that share
//! one slope and each have their own level: the segments before the step
//! go on giving the slope, as sums centred on their own means, until they
//! fade from its memory, and the level `û` is the newest segment's. So the
//! whole step lands in `û` and none of it in `v̂`, which a straight line
//! drawn through a step would read as hundreds of ppm. Steps are told only
//! once the reference is taken, against a level a hundred blocks set; the
//! first second's blocks all enter the fit. The corrector makes up a step
//! at once ([`crate::correct`]).
//!
//! # Invariants
//!
//! * Pure and deterministic: PTS and sample counts in, estimates out. There
//!   is no clock; time is PTS and sample counts.
//! * Every estimate is finite (the uncertainty is infinite until it can be
//!   told, never NaN); a block with a PTS the fit cannot use (a residual
//!   that is not finite) is a discontinuity, never a NaN.
//! * A step moves the timing error by its size and the slope not at all.

use std::time::Duration;

use thiserror::Error;

/// Input time of an epoch after which its timing reference is taken from
/// the fit and steps are told; see the module docs.
pub const REFERENCE_AFTER: Duration = Duration::from_secs(1);

/// Multiples of the blocks' scatter about their predictions beyond which a
/// residual is a step, not jitter; see the module docs.
pub const STEP_SIGMAS: f64 = 6.0;

/// Why drift settings or a corrector's format were refused.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DriftError {
    /// The sample rate is zero.
    #[error("a drift corrector needs a sample rate above zero")]
    Rate,
    /// The channel count is outside 1 to [`crate::resample::MAX_CHANNELS`].
    #[error("a drift corrector takes 1 to {max} channels, not {0}", max = crate::resample::MAX_CHANNELS)]
    Channels(u16),
    /// A setting is outside its range.
    #[error("drift settings: {0}")]
    Settings(String),
}

/// How drift is measured and corrected; see the module docs and
/// [`crate::correct`]. [`DriftSettings::default`] gives the values the
/// docs name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftSettings {
    /// A block further than this from where the estimates put it starts a
    /// new epoch.
    pub discontinuity: Duration,
    /// A block further than this (and than [`STEP_SIGMAS`] times the
    /// scatter) from where the estimates put it, but within
    /// `discontinuity`, is a step.
    pub step: Duration,
    /// The timing filter's memory `τ`.
    pub smoothing: Duration,
    /// Time over which the corrector takes up timing error.
    pub settle: Duration,
    /// Measured drift within this many ppm either way is not corrected.
    pub dead_band_ppm: f64,
    /// Timing error within this either way is not corrected (while the
    /// drift is in its dead band too).
    pub dead_band_error: Duration,
    /// Largest correction, in ppm either way.
    pub max_correction_ppm: f64,
    /// Fastest change of the correction, in ppm per second of audio.
    pub max_slew_ppm_per_s: f64,
}

impl Default for DriftSettings {
    fn default() -> Self {
        Self {
            discontinuity: Duration::from_millis(100),
            step: Duration::from_millis(2),
            smoothing: Duration::from_secs(20),
            settle: Duration::from_secs(60),
            dead_band_ppm: 2.0,
            dead_band_error: Duration::from_millis(10),
            max_correction_ppm: 1_000.0,
            max_slew_ppm_per_s: 50.0,
        }
    }
}

impl DriftSettings {
    /// Checks that every setting is usable: durations above zero, a step
    /// below the discontinuity, a dead band of zero or more, and a
    /// correction limit and slew above zero and within the resampler's
    /// reach.
    pub fn check(&self) -> Result<(), DriftError> {
        let bad = |why: String| Err(DriftError::Settings(why));
        for (name, d) in [
            ("discontinuity", self.discontinuity),
            ("step", self.step),
            ("smoothing", self.smoothing),
            ("settle", self.settle),
        ] {
            if d.is_zero() {
                return bad(format!("{name} must be above zero"));
            }
        }
        if self.step >= self.discontinuity {
            return bad("a step must be shorter than a discontinuity".into());
        }
        let reach = crate::resample::MAX_STEP_DEVIATION * 1e6 / 2.0;
        if !(self.dead_band_ppm >= 0.0 && self.dead_band_ppm.is_finite()) {
            return bad(format!("a dead band of {} ppm", self.dead_band_ppm));
        }
        if !(self.max_correction_ppm > 0.0 && self.max_correction_ppm <= reach) {
            return bad(format!(
                "a largest correction of {} ppm is outside 0 to {reach}",
                self.max_correction_ppm
            ));
        }
        if !(self.max_slew_ppm_per_s > 0.0 && self.max_slew_ppm_per_s.is_finite()) {
            return bad(format!("a slew of {} ppm/s", self.max_slew_ppm_per_s));
        }
        Ok(())
    }
}

/// What a block was to the estimator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Observation {
    /// The first block: it starts the first epoch.
    Start,
    /// A block of the current epoch, which the estimates took in.
    Tracked,
    /// A step: the block's timing error is this many seconds from where
    /// the estimates put it, below zero when audio was lost (its PTS
    /// stepped forward). The estimates took it in, its level and not its
    /// slope; see the module docs.
    Step(f64),
    /// A break: the block starts a new epoch (counted as a reset).
    Discontinuity,
}

/// One epoch's state: the running sums of its weighted fit, with input
/// time `x` measured from the latest block's first frame. The sums `w` to
/// `wwxx` are the current segment's (the blocks since the latest step);
/// `cxx`, `cxu` and `cww` hold the earlier segments'.
#[derive(Debug, Clone, Copy, Default)]
struct Epoch {
    /// PTS of its first block, from which `u` counts.
    anchor: Duration,
    /// Input frames from its first block to the next block's start.
    frames: u64,
    /// Frames of the latest block: the input time from its observation to
    /// the next one.
    latest: u64,
    /// Blocks taken in.
    blocks: u64,
    /// `Σw`: the blocks' weights.
    w: f64,
    /// `Σw·x`.
    wx: f64,
    /// `Σw·x²`.
    wxx: f64,
    /// `Σw·u`.
    wu: f64,
    /// `Σw·x·u`.
    wxu: f64,
    /// `Σw²`, for the slope's standard error.
    ww: f64,
    /// `Σw²·x`.
    wwx: f64,
    /// `Σw²·x²`.
    wwxx: f64,
    /// `Σw·(x − x̄)²` of the earlier segments, each about its own mean.
    cxx: f64,
    /// `Σw·(x − x̄)·(u − ū)` of the earlier segments.
    cxu: f64,
    /// `Σw²·(x − x̄)²` of the earlier segments.
    cww: f64,
    /// `Σw·r²` over the residuals `r` of the blocks predicted by a fitted
    /// line (from the third on, steps left out).
    wrr: f64,
    /// `Σw` over the same blocks.
    wr: f64,
    /// Fitted timing error `û` at the latest block, in seconds, from the
    /// epoch's first block.
    error: f64,
    /// Fitted slope `v̂` of the timing error along input time.
    slope: f64,
    /// Standard error of `v̂`; infinite until it can be told.
    spread: f64,
    /// `error` when the epoch had [`REFERENCE_AFTER`] of input: what the
    /// timing error is measured against.
    reference: Option<f64>,
}

impl Epoch {
    /// An epoch starting with a block at `pts` of `frames` frames.
    fn new(pts: Duration, frames: u64, rate: f64) -> Self {
        let mut epoch = Self {
            anchor: pts,
            spread: f64::INFINITY,
            ..Self::default()
        };
        epoch.take(0.0, None, frames, rate);
        epoch.frames = frames;
        epoch.latest = frames;
        epoch
    }

    /// The blocks' scatter about their predictions, in seconds: zero until
    /// a prediction was scored.
    fn scatter(&self) -> f64 {
        if self.wr > 0.0 {
            (self.wrr / self.wr).sqrt()
        } else {
            0.0
        }
    }

    /// Ages the sums by `dx` seconds of input time (memory `smoothing`),
    /// moving their origin to the new block's first frame.
    fn age(&mut self, dx: f64, smoothing: f64) {
        let fade = (-dx / smoothing).exp();
        let fade2 = fade * fade;
        // With x' = x − dx: Σw·x' = Σw·x − dx·Σw, and so on.
        self.wxx = fade * (self.wxx - 2.0 * dx * self.wx + dx * dx * self.w);
        self.wx = fade * (self.wx - dx * self.w);
        self.wxu = fade * (self.wxu - dx * self.wu);
        self.w *= fade;
        self.wu *= fade;
        self.wwxx = fade2 * (self.wwxx - 2.0 * dx * self.wwx + dx * dx * self.ww);
        self.wwx = fade2 * (self.wwx - dx * self.ww);
        self.ww *= fade2;
        // Sums about a segment's own mean do not move with the origin.
        self.cxx *= fade;
        self.cxu *= fade;
        self.cww *= fade2;
        self.wrr *= fade;
        self.wr *= fade;
    }

    /// Ends the current segment at a step: its sums, centred on its own
    /// means, join the earlier segments', which go on giving the slope but
    /// not the level, and the next block starts a new segment.
    fn split(&mut self) {
        if self.w > 0.0 {
            let mean = self.wx / self.w;
            self.cxx += self.wxx - mean * self.wx;
            self.cxu += self.wxu - mean * self.wu;
            self.cww += self.wwxx - 2.0 * mean * self.wwx + mean * mean * self.ww;
        }
        (self.w, self.wx, self.wxx, self.wu, self.wxu) = (0.0, 0.0, 0.0, 0.0, 0.0);
        (self.ww, self.wwx, self.wwxx) = (0.0, 0.0, 0.0);
    }

    /// Takes in a block of `frames` frames at the origin whose timing error
    /// measured `u`, with the residual `r` of a fitted line's prediction of
    /// it when it is scored, and fits the line again.
    fn take(&mut self, u: f64, r: Option<f64>, frames: u64, rate: f64) {
        let weight = frames as f64 / rate;
        self.w += weight;
        self.wu += weight * u;
        self.ww += weight * weight;
        if let Some(r) = r {
            self.wrr += weight * r * r;
            self.wr += weight;
        }
        self.blocks += 1;
        // Weighted least squares about the origin (the latest block), one
        // slope through every segment and a level for the current one:
        // `S` is Σw·(x − x̄)², each segment about its own mean.
        let s = self.cxx + self.wxx - self.wx * self.wx / self.w;
        if self.blocks >= 2 && s > 0.0 {
            self.slope = (self.cxu + self.wxu - self.wx * self.wu / self.w) / s;
        }
        self.error = (self.wu - self.slope * self.wx) / self.w;
        // Var(v̂) = σ²·Σw²·(x − x̄)² / S², σ² from the prediction residuals.
        let mean = self.wx / self.w;
        let spread = self.cww + self.wwxx - 2.0 * mean * self.wwx + mean * mean * self.ww;
        let sigma2 = self.wrr / self.wr;
        let se = (sigma2 * spread).sqrt() / s;
        self.spread = if self.blocks >= 3 && se.is_finite() {
            se
        } else {
            f64::INFINITY
        };
    }
}

/// `a − b` in seconds, below zero when `a` is earlier.
fn seconds_between(a: Duration, b: Duration) -> f64 {
    if a >= b {
        (a - b).as_secs_f64()
    } else {
        -(b - a).as_secs_f64()
    }
}

/// Estimates an input's drift from its blocks' PTS; see the module docs.
#[derive(Debug, Clone)]
pub struct DriftEstimator {
    /// Nominal frames per second.
    rate: f64,
    /// [`DriftSettings::discontinuity`] in seconds.
    discontinuity: f64,
    /// [`DriftSettings::step`] in seconds.
    step: f64,
    /// [`DriftSettings::smoothing`] in seconds.
    smoothing: f64,
    /// The current epoch, once a block came.
    epoch: Option<Epoch>,
    /// Discontinuities seen.
    resets: u64,
    /// Steps seen.
    steps: u64,
}

impl DriftEstimator {
    /// An estimator for an input of nominal `rate` Hz with `settings`.
    pub fn new(rate: u32, settings: &DriftSettings) -> Result<Self, DriftError> {
        if rate == 0 {
            return Err(DriftError::Rate);
        }
        settings.check()?;
        Ok(Self {
            rate: f64::from(rate),
            discontinuity: settings.discontinuity.as_secs_f64(),
            step: settings.step.as_secs_f64(),
            smoothing: settings.smoothing.as_secs_f64(),
            epoch: None,
            resets: 0,
            steps: 0,
        })
    }

    /// Takes in a block whose first frame is at `pts` and which holds
    /// `frames` frames; see the module docs.
    pub fn observe(&mut self, pts: Duration, frames: u64) -> Observation {
        let Some(epoch) = &mut self.epoch else {
            self.epoch = Some(Epoch::new(pts, frames, self.rate));
            return Observation::Start;
        };
        let measured = epoch.frames as f64 / self.rate - seconds_between(pts, epoch.anchor);
        let dx = epoch.latest as f64 / self.rate;
        let residual = measured - (epoch.error + epoch.slope * dx);
        if residual.is_nan() || residual.abs() > self.discontinuity {
            self.resets += 1;
            self.epoch = Some(Epoch::new(pts, frames, self.rate));
            return Observation::Discontinuity;
        }
        let step = epoch.reference.is_some()
            && residual.abs() > self.step.max(STEP_SIGMAS * epoch.scatter());
        epoch.age(dx, self.smoothing);
        if step {
            epoch.split();
            self.steps += 1;
        }
        let scored = epoch.blocks >= 2 && !step;
        epoch.take(measured, scored.then_some(residual), frames, self.rate);
        epoch.frames += frames;
        epoch.latest = frames;
        if epoch.reference.is_none()
            && epoch.frames as f64 / self.rate >= REFERENCE_AFTER.as_secs_f64()
        {
            epoch.reference = Some(epoch.error);
        }
        if step {
            Observation::Step(residual)
        } else {
            Observation::Tracked
        }
    }

    /// Forgets the epoch, as at a break; the next block starts a new one.
    /// Counted as a reset when there was an epoch.
    pub fn reset(&mut self) {
        if self.epoch.take().is_some() {
            self.resets += 1;
        }
    }

    /// Estimated timing error `û` at the latest block against the epoch's
    /// reference, in seconds: above zero while the input has delivered
    /// more than the master clock's share since then (a step forward,
    /// audio lost, counts below zero). Zero until the reference is taken;
    /// see the module docs.
    pub fn error(&self) -> f64 {
        self.epoch
            .and_then(|e| Some(e.error - e.reference?))
            .unwrap_or(0.0)
    }

    /// True once the epoch's timing reference is taken
    /// ([`REFERENCE_AFTER`] of input into it); see the module docs.
    pub fn has_reference(&self) -> bool {
        self.epoch.is_some_and(|e| e.reference.is_some())
    }

    /// Estimated slope `v̂` of the timing error along input time: one minus
    /// the ratio of output to input frames that cancels the drift.
    pub fn slope(&self) -> f64 {
        self.epoch.map_or(0.0, |e| e.slope)
    }

    /// Estimated drift `ε` in ppm: above zero while the input's clock runs
    /// fast.
    pub fn drift_ppm(&self) -> f64 {
        let v = self.slope();
        v / (1.0 - v) * 1e6
    }

    /// The standard error of [`DriftEstimator::drift_ppm`], in ppm: how far
    /// the drift may be off by the PTS's scatter. Infinite until three
    /// blocks of an epoch came, and while it cannot be told.
    pub fn drift_uncertainty_ppm(&self) -> f64 {
        self.epoch.map_or(f64::INFINITY, |e| e.spread * 1e6)
    }

    /// Input time of the current epoch so far, in seconds.
    pub fn epoch_seconds(&self) -> f64 {
        self.epoch.map_or(0.0, |e| e.frames as f64 / self.rate)
    }

    /// Discontinuities seen (and explicit resets of an epoch).
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Steps seen.
    pub fn steps(&self) -> u64 {
        self.steps
    }
}

#[cfg(test)]
mod tests;
