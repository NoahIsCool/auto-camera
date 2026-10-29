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
//! epoch's first block on (so it has no start-up transient, and blocks of
//! any length are weighed by the time they cover) and kept as running sums
//! about the latest block (so their values stay small over a show of any
//! length). Jitter of a millisecond leaves a few tenths of a part per
//! million in `v̂`.
//!
//! The fit also reports how far its slope can be trusted
//! ([`DriftEstimator::drift_uncertainty_ppm`]): the standard error of `v̂`,
//! from the scatter of the blocks about their predictions. Early in an
//! epoch, or with noisy PTS, it is large, and the corrector does not take a
//! slope for drift until the slope stands clear of its dead band by several
//! of them; a file's exact PTS make it vanish.
//!
//! The estimator never looks at when a block *arrives*, only at its PTS and
//! its sample count, so a reader that drains once per tick, arrival jitter,
//! and the burst of late blocks after a stall do not enter it.
//!
//! # Discontinuities
//!
//! A block whose residual is larger than [`DriftSettings::discontinuity`]
//! (100 ms, the program pacer's slack) is not drift but a break: a
//! reconnection's new epoch (its PTS moved on by the time the input was
//! away), a stall whose audio was lost, blocks a full ring dropped. It
//! starts a new epoch, the estimates start again from zero, and the reset
//! is counted. Breaks that big are the program pacer's to fill (it inserts
//! silence for a gap beyond its slack); a smaller step is left in `u` as
//! timing error, which the corrector takes up slowly.
//!
//! # Invariants
//!
//! * Pure and deterministic: PTS and sample counts in, estimates out. There
//!   is no clock; time is PTS and sample counts.
//! * Every estimate is finite (the uncertainty is infinite until it can be
//!   told, never NaN); a block with a PTS the fit cannot use (a residual
//!   that is not finite) is a discontinuity, never a NaN.

use std::time::Duration;

use thiserror::Error;

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
    /// Checks that every setting is usable: durations above zero, a dead
    /// band of zero or more, and a correction limit and slew above zero
    /// and within the resampler's reach.
    pub fn check(&self) -> Result<(), DriftError> {
        let bad = |why: String| Err(DriftError::Settings(why));
        for (name, d) in [
            ("discontinuity", self.discontinuity),
            ("smoothing", self.smoothing),
            ("settle", self.settle),
        ] {
            if d.is_zero() {
                return bad(format!("{name} must be above zero"));
            }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// The first block: it starts the first epoch.
    Start,
    /// A block of the current epoch, which the estimates took in.
    Tracked,
    /// A break: the block starts a new epoch (counted as a reset).
    Discontinuity,
}

/// One epoch's state: the running sums of its weighted fit, with input
/// time `x` measured from the latest block's first frame.
#[derive(Debug, Clone, Copy, Default)]
struct Epoch {
    /// PTS of its first block.
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
    /// `Σw·r²` over the blocks' prediction residuals `r`.
    wrr: f64,
    /// `Σw` over the same blocks (all but the first).
    wr: f64,
    /// Fitted timing error `û` at the latest block, in seconds.
    error: f64,
    /// Fitted slope `v̂` of the timing error along input time.
    slope: f64,
    /// Standard error of `v̂`; infinite until it can be told.
    spread: f64,
}

impl Epoch {
    /// An epoch starting with a block at `pts` of `frames` frames.
    fn new(pts: Duration, frames: u64, rate: f64) -> Self {
        let mut epoch = Self {
            anchor: pts,
            spread: f64::INFINITY,
            ..Self::default()
        };
        epoch.take(0.0, 0.0, frames, rate);
        epoch.frames = frames;
        epoch
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
        self.wrr *= fade;
        self.wr *= fade;
    }

    /// Takes in a block of `frames` frames at the origin whose timing error
    /// measured `u` and was predicted `r` off, and fits the line again.
    fn take(&mut self, u: f64, r: f64, frames: u64, rate: f64) {
        let weight = frames as f64 / rate;
        self.w += weight;
        self.wu += weight * u;
        self.ww += weight * weight;
        if self.blocks > 0 {
            self.wrr += weight * r * r;
            self.wr += weight;
        }
        self.blocks += 1;
        // Weighted least squares about the origin (the latest block):
        // `S` is Σw·(x − x̄)².
        let s = self.wxx - self.wx * self.wx / self.w;
        if self.blocks >= 2 && s > 0.0 {
            self.slope = (self.wxu - self.wx * self.wu / self.w) / s;
            self.error = (self.wu - self.slope * self.wx) / self.w;
        } else if self.w > 0.0 {
            self.error = self.wu / self.w;
        }
        // Var(v̂) = σ²·Σw²·(x − x̄)² / S², σ² from the prediction residuals.
        let mean = self.wx / self.w;
        let spread = self.wwxx - 2.0 * mean * self.wwx + mean * mean * self.ww;
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
    /// [`DriftSettings::smoothing`] in seconds.
    smoothing: f64,
    /// The current epoch, once a block came.
    epoch: Option<Epoch>,
    /// Discontinuities seen.
    resets: u64,
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
            smoothing: settings.smoothing.as_secs_f64(),
            epoch: None,
            resets: 0,
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
        epoch.age(dx, self.smoothing);
        epoch.take(measured, residual, frames, self.rate);
        epoch.frames += frames;
        epoch.latest = frames;
        Observation::Tracked
    }

    /// Forgets the epoch, as at a break; the next block starts a new one.
    /// Counted as a reset when there was an epoch.
    pub fn reset(&mut self) {
        if self.epoch.take().is_some() {
            self.resets += 1;
        }
    }

    /// Estimated timing error `û` at the latest block, in seconds: above
    /// zero while the input has delivered more than the master clock's
    /// share since the epoch began.
    pub fn error(&self) -> f64 {
        self.epoch.map_or(0.0, |e| e.error)
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
}

#[cfg(test)]
mod tests;
