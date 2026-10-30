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
//! fitted `û`, less the steps told before then (below), is taken as the
//! reference, and [`DriftEstimator::error`] is `û` less it from then on
//! (zero until then). One block's error moves the fitted `û` at the far
//! end of the fit by about twice its share, two percent of it at a hundred
//! blocks. A second block already more than [`DriftSettings::step`] off
//! the first (there is no scatter yet to go by) says the first block's
//! PTS was off: the epoch starts again from the second block, with
//! nothing made up and nothing counted.
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
//! scatter about their predictions, may be a **step**: audio lost in a
//! stall or dropped by a full ring (the PTS step forward by its length), or
//! PTS that went back. It may also be a timestamp gone astray for a few
//! blocks (a host-time glitch; how few, below), which the blocks after it
//! put right. So the block is first *held* ([`Observation::Held`]): kept
//! out of the fit and of the scatter, the fit carried on along its line,
//! and nothing is told yet. A block back within the threshold of the fit
//! ends the hold: the held blocks strayed, nothing is made up, and the
//! estimates are as if they had never come. Residuals that stay at one
//! level (each within the threshold of the first of the run) for
//! [`CONFIRM_BLOCKS`] blocks in a row, the first included, and over at
//! least [`CONFIRM_THRESHOLDS`] times the threshold of input time, are a
//! step ([`Observation::Step`]), told at the block that confirms it with
//! that block's residual: a real loss is made up a few blocks late, at its
//! full size, once. Residuals that move from level to level start the run
//! again from each new level: the tail of an excursion whose PTS were held
//! from going back rises by a block's length a block, so however short the
//! blocks it leaves the threshold before it has lasted that long, and is
//! never taken for a level. Held blocks that have neither settled nor come
//! back after twice the discontinuity of input time are a break.
//!
//! A stray is therefore left alone only while it is shorter than what
//! confirms a step: it must come back within three blocks (the fourth at
//! its level would confirm it) or, with blocks shorter than the threshold,
//! before its run spans three thresholds of input time (6 ms at the default
//! 2 ms step) from its first block's start to a later block's start,
//! whichever is the longer. A stray that stays off at one level longer
//! cannot be told from a step and is told as one, at the block that
//! confirms it; the PTS coming back are then a step the other way, held and
//! confirmed in turn, the fourth block back (or the one completing the
//! span) told as a step of the opposite size. Made up by the corrector,
//! that is two edits of the stray's size (silence and a cut), a timing
//! error of that size for about as long as the stray lasted, and no lasting
//! offset.
//!
//! Steps are told from an epoch's third block on (a second block that is
//! off starts the epoch again, above), so that a loss or a stray timestamp
//! among its first blocks is held out of the fit like any later one,
//! rather than bending the line and leaving its predictions, and with them
//! the scatter and the threshold, far off for seconds. A slope fitted to a
//! few blocks of jittery PTS predicts worse than none, though (jitter
//! moves a block's PTS by up to milliseconds, drift the timing error by
//! microseconds in a tenth of a second), so over an epoch's first
//! [`SLOPE_AFTER`] of input a block's residual is taken against the
//! epoch's level alone (the weighted mean of its timing error since the
//! latest step), and the threshold counts the scatter of the blocks taken
//! in about that level (from the second block on) as well as about the
//! line, so that jitter as large as the first blocks' own is not held.
//! Until a residual was scored the threshold is the step alone. The
//! scatter about the line is always scored against the line, so that
//! exact PTS keep the exact slope's uncertainty.
//!
//! A step is timing error, not drift. The fit is cut there into *segments*
//! that share one slope and each have their own level: the segments
//! before the step go on giving the slope, as sums centred on their own
//! means, until they fade from its memory, and the level `û` is the newest
//! segment's. So the whole step lands in `û` and none of it in `v̂`, which
//! a straight line drawn through a step would read as hundreds of ppm. A
//! step told before the reference is taken is left out of the reference,
//! which is then the level the epoch had before it: the step shows in the
//! timing error as one after the reference does, and the silence or cut
//! that made it up is not taken back. The corrector makes up a step as
//! soon as it is told ([`crate::correct`]).
//!
//! # Invariants
//!
//! * Pure and deterministic: PTS and sample counts in, estimates out. There
//!   is no clock; time is PTS and sample counts.
//! * Every estimate is finite (the uncertainty is infinite until it can be
//!   told, never NaN); a block with a PTS the fit cannot use (a residual
//!   that is not finite) is a discontinuity, never a NaN.
//! * A step moves the timing error by its size and the slope not at all;
//!   held blocks that come back move neither.

use std::time::Duration;

use thiserror::Error;

mod epoch;

use epoch::{Epoch, Held, Verdict};

/// Input time of an epoch after which its timing reference is taken from
/// the fit; see the module docs.
pub const REFERENCE_AFTER: Duration = Duration::from_secs(1);

/// Input time of an epoch before which a block is weighed against the
/// epoch's level alone, not its fitted slope; see the module docs.
pub const SLOPE_AFTER: Duration = Duration::from_millis(100);

/// Blocks in a row at one level beyond the step threshold, the first
/// included, that confirm a step; see the module docs.
pub const CONFIRM_BLOCKS: u64 = 4;

/// Input time a run of blocks at one level must span to confirm a step, in
/// multiples of the step threshold: residuals that rise as fast as input
/// time leave the threshold within it; see the module docs.
pub const CONFIRM_THRESHOLDS: f64 = 3.0;

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
    /// new epoch, as do held blocks that neither settle nor come back
    /// within twice this of input time.
    pub discontinuity: Duration,
    /// A block further than this (and than [`STEP_SIGMAS`] times the
    /// scatter) from where the estimates put it, but within
    /// `discontinuity`, is held, and a step once [`CONFIRM_BLOCKS`] in a
    /// row agree.
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
    /// A block that starts an epoch without a break: the first block, or
    /// the second of an epoch when it is more than a step off the first,
    /// whose PTS was then off (see the module docs).
    Start,
    /// A block of the current epoch, which the estimates took in.
    Tracked,
    /// A block further from where the estimates put it than a step, held
    /// out of them until the blocks after it show whether the PTS stepped
    /// or strayed; see the module docs.
    Held,
    /// A step, confirmed at this block: its timing error is this many
    /// seconds from where the estimates put it before the step, below
    /// zero when audio was lost (the PTS stepped forward). The estimates
    /// took it in, its level and not its slope; see the module docs.
    Step(f64),
    /// A break: the block starts a new epoch (counted as a reset).
    Discontinuity,
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
        // Where the fitted line puts the block, and where the estimates
        // do: the line, or early in the epoch its level alone.
        let line = epoch.error + epoch.slope * dx;
        let early = (epoch.frames as f64 / self.rate) < SLOPE_AFTER.as_secs_f64();
        let residual = measured - if early { epoch.level() } else { line };
        if residual.is_nan() || residual.abs() > self.discontinuity {
            self.resets += 1;
            self.epoch = Some(Epoch::new(pts, frames, self.rate));
            return Observation::Discontinuity;
        }
        let scatter = match early {
            true => epoch.scatter().max(epoch.level_scatter()),
            false => epoch.scatter(),
        };
        let threshold = self.step.max(STEP_SIGMAS * scatter);
        if epoch.blocks == 1 && residual.abs() > threshold {
            // The first block's PTS was off: start again from this one.
            self.epoch = Some(Epoch::new(pts, frames, self.rate));
            return Observation::Start;
        }
        let limit = 2.0 * self.discontinuity;
        let verdict = match epoch.held.as_mut() {
            Some(held) => held.weigh(residual, dx, threshold, limit),
            None if residual.abs() > threshold => {
                epoch.held = Some(Held::new(residual));
                Verdict::Held
            }
            None => Verdict::Back,
        };
        epoch.age(dx, self.smoothing);
        let seen = match verdict {
            Verdict::Held => {
                epoch.pass(dx);
                Observation::Held
            }
            Verdict::Back => {
                epoch.held = None;
                let scored = epoch.blocks >= 2;
                if early {
                    epoch.score_level(residual, frames, self.rate);
                }
                let off_line = measured - line;
                epoch.take(measured, scored.then_some(off_line), frames, self.rate);
                Observation::Tracked
            }
            Verdict::Step => {
                epoch.held = None;
                epoch.split();
                epoch.take(measured, None, frames, self.rate);
                if epoch.reference.is_none() {
                    epoch.stepped += residual;
                }
                self.steps += 1;
                Observation::Step(residual)
            }
            Verdict::Lost => {
                self.resets += 1;
                self.epoch = Some(Epoch::new(pts, frames, self.rate));
                return Observation::Discontinuity;
            }
        };
        epoch.frames += frames;
        epoch.latest = frames;
        if epoch.reference.is_none()
            && epoch.frames as f64 / self.rate >= REFERENCE_AFTER.as_secs_f64()
        {
            epoch.reference = Some(epoch.error - epoch.stepped);
        }
        seen
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
