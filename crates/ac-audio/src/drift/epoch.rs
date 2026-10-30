//! One epoch of the drift estimator: the running sums of its weighted
//! straight-line fit, cut into segments at steps, and the run of blocks
//! held out of the fit while the blocks after a stray one show whether
//! the PTS stepped; see [`super`] for the theory.
//!
//! # Invariants
//!
//! * The sums are about the latest block's first frame (`x = 0`), so they
//!   stay small over an epoch of any length. `slope` remains the fitted
//!   slope; `error` is its intercept except while settling carries a
//!   guarded prediction (without slope before the timing reference).
//! * A block held out of the fit enters no sum: it only moves the origin,
//!   and `error` along the prediction's current slope with it.
//! * A held run ends at the first block back within the threshold, at
//!   [`CONFIRM_BLOCKS`] blocks in a row at one level spanning
//!   [`CONFIRM_THRESHOLDS`] times the threshold of input time, or once it
//!   has lasted its limit, whichever comes first.

use std::time::Duration;

use super::{CONFIRM_BLOCKS, CONFIRM_THRESHOLDS};

/// One epoch's state: the running sums of its weighted fit, with input
/// time `x` measured from the latest block's first frame. The sums `w` to
/// `wwxx` are the current segment's (the blocks since the latest step);
/// `cxx`, `cxu` and `cww` hold the earlier segments'.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Epoch {
    /// PTS of its first block, from which `u` counts.
    pub(super) anchor: Duration,
    /// Input frames from its first block to the next block's start.
    pub(super) frames: u64,
    /// Frames of the latest block: the input time from its observation to
    /// the next one.
    pub(super) latest: u64,
    /// Blocks taken in.
    pub(super) blocks: u64,
    /// Sample frames whose timestamps were admitted to the fit, excluding
    /// held blocks and the settling interval after a step.
    pub(super) measured_frames: u64,
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
    /// line (from the third on, steps and held blocks left out).
    wrr: f64,
    /// `Σw` over the same blocks: the input time of the predictions
    /// scored, faded like the rest.
    wr: f64,
    /// `Σw·r²` over the residuals `r` of the blocks weighed against the
    /// level alone, early in the epoch (from the second block on, held
    /// blocks left out).
    lrr: f64,
    /// `Σw` over the same blocks.
    lr: f64,
    /// Fitted timing error `û` at the latest block, in seconds, from the
    /// epoch's first block.
    pub(super) error: f64,
    /// Fitted slope `v̂` of the timing error along input time.
    pub(super) slope: f64,
    /// Standard error of `v̂`; infinite until it can be told.
    pub(super) spread: f64,
    /// What the timing error is measured against: `error`, less
    /// `stepped`, after [`super::REFERENCE_AFTER`] of admitted input.
    pub(super) reference: Option<f64>,
    /// The sizes of the steps told before the reference, summed.
    pub(super) stepped: f64,
    /// Input time left before a confirmed step's new segment may learn
    /// from blocks within its threshold; a returning excursion must first
    /// be measured against the same line that made its forward edit.
    pub(super) settling: f64,
    /// Slope carried during settling; zero before the epoch's reference
    /// exists, when a handful of jittered timestamps cannot predict it.
    pub(super) settling_slope: f64,
    /// The blocks held out of the fit, while there are.
    pub(super) held: Option<Held>,
}

impl Epoch {
    /// An epoch starting with a block at `pts` of `frames` frames.
    pub(super) fn new(pts: Duration, frames: u64, rate: f64) -> Self {
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
    pub(super) fn scatter(&self) -> f64 {
        if self.wr > 0.0 {
            (self.wrr / self.wr).sqrt()
        } else {
            0.0
        }
    }

    /// The blocks' scatter about the epoch's level, in seconds, over the
    /// blocks weighed against it: zero until one was.
    pub(super) fn level_scatter(&self) -> f64 {
        if self.lr > 0.0 {
            (self.lrr / self.lr).sqrt()
        } else {
            0.0
        }
    }

    /// Scores the residual `r` against the level alone of a block of
    /// `frames` frames taken in early in the epoch.
    pub(super) fn score_level(&mut self, r: f64, frames: u64, rate: f64) {
        let weight = frames as f64 / rate;
        self.lrr += weight * r * r;
        self.lr += weight;
    }

    /// Ages the sums by `dx` seconds of input time (memory `smoothing`),
    /// moving their origin to the new block's first frame.
    pub(super) fn age(&mut self, dx: f64, smoothing: f64) {
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
        self.lrr *= fade;
        self.lr *= fade;
    }

    /// The current segment's level: the weighted mean of its blocks'
    /// timing error, in seconds, what the fit predicts with no slope.
    pub(super) fn level(&self) -> f64 {
        if self.w > 0.0 {
            self.wu / self.w
        } else {
            self.error
        }
    }

    /// Carries the prediction to an origin `dx` seconds on for a block
    /// held out of the fit (the sums were aged already), using the guarded
    /// slope while a confirmed step's new level is settling.
    pub(super) fn pass(&mut self, dx: f64) {
        self.error += self.carried_slope() * dx;
    }

    /// The slope used to predict blocks while a step's level settles.
    pub(super) fn carried_slope(&self) -> f64 {
        if self.settling > 0.0 {
            self.settling_slope
        } else {
            self.slope
        }
    }

    /// Ends the current segment at a step: its sums, centred on its own
    /// means, join the earlier segments', which go on giving the slope but
    /// not the level, and the next block starts a new segment.
    pub(super) fn split(&mut self) {
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
    pub(super) fn take(&mut self, u: f64, r: Option<f64>, frames: u64, rate: f64) {
        let weight = frames as f64 / rate;
        self.w += weight;
        self.wu += weight * u;
        self.ww += weight * weight;
        if let Some(r) = r {
            self.wrr += weight * r * r;
            self.wr += weight;
        }
        self.blocks += 1;
        self.measured_frames = self.measured_frames.saturating_add(frames);
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

/// The blocks held out of the fit since one strayed beyond the step
/// threshold, while the blocks after it show what it was: a step being
/// confirmed, or a stray timestamp being waited out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Held {
    /// Residual of the current run's first block: the level the PTS may
    /// have stepped to, in seconds.
    level: f64,
    /// Blocks of the current run, its first included.
    run: u64,
    /// Input time from the current run's first block to the latest, in
    /// seconds.
    span: f64,
    /// Input time from the first held block to the latest, in seconds.
    since: f64,
}

/// What becomes of a block, held or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    /// The block is taken in: it is within the threshold of the fit (back
    /// on it, when blocks were held: they strayed).
    Back,
    /// The block is held: it starts a run or the run goes on.
    Held,
    /// The block makes [`CONFIRM_BLOCKS`] in a row at one level, over
    /// [`CONFIRM_THRESHOLDS`] times the threshold of input time: a step.
    Step,
    /// The run lasted its limit without settling or coming back.
    Lost,
}

impl Held {
    /// A run that starts with a block whose residual is `residual`.
    pub(super) fn new(residual: f64) -> Self {
        Self {
            level: residual,
            run: 1,
            span: 0.0,
            since: 0.0,
        }
    }

    /// Weighs a block whose residual is `residual`, `dx` seconds of input
    /// after the previous block, against `threshold`; the run may last
    /// `limit` seconds of input. A block within the threshold of the run's
    /// level adds to the run; one further off starts a new run at its own.
    pub(super) fn weigh(&mut self, residual: f64, dx: f64, threshold: f64, limit: f64) -> Verdict {
        if residual.abs() <= threshold {
            return Verdict::Back;
        }
        self.since += dx;
        if (residual - self.level).abs() <= threshold {
            self.run += 1;
            self.span += dx;
        } else {
            (self.level, self.run, self.span) = (residual, 1, 0.0);
        }
        if self.run >= CONFIRM_BLOCKS && self.span >= CONFIRM_THRESHOLDS * threshold {
            Verdict::Step
        } else if self.since > limit {
            Verdict::Lost
        } else {
            Verdict::Held
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_comes_back_settles_moves_on_or_is_lost() {
        let (step, limit) = (0.002, 0.2);
        // Back within the threshold of the fit.
        let mut held = Held::new(-0.03);
        assert_eq!(held.weigh(0.001, 0.01, step, limit), Verdict::Back);
        // At one level, within the threshold: confirmed at the last block.
        let mut held = Held::new(-0.03);
        for _ in 2..CONFIRM_BLOCKS {
            assert_eq!(held.weigh(-0.0315, 0.01, step, limit), Verdict::Held);
        }
        assert_eq!(held.weigh(-0.0285, 0.01, step, limit), Verdict::Step);
        // Moving from level to level, a block on each time, the run starts
        // again at every block and never settles.
        let mut held = Held::new(-0.03);
        for k in 1..10 {
            let r = -0.03 + 0.0025 * f64::from(k);
            assert_eq!(held.weigh(r, 0.01, step, limit), Verdict::Held, "{k}");
            assert_eq!(held.run, 1);
        }
        // Wandering for longer than the limit.
        let mut held = Held::new(-0.01);
        let mut seen = Verdict::Held;
        for k in 0..30 {
            seen = held.weigh(if k % 2 == 0 { -0.02 } else { -0.01 }, 0.01, step, limit);
            if seen != Verdict::Held {
                break;
            }
        }
        assert_eq!(seen, Verdict::Lost);
        assert!(held.since > limit);
    }

    #[test]
    fn a_run_of_short_blocks_confirms_a_level_only_over_its_span() {
        // 32-frame blocks at 48 kHz, 0.67 ms of input each, beyond a 2 ms
        // threshold: four in a row at one level are not yet a step, a run
        // spanning three thresholds of input time is.
        let (step, limit, dx) = (0.002, 0.2, 32.0 / 48_000.0);
        let mut held = Held::new(-0.015);
        let blocks = (CONFIRM_THRESHOLDS * step / dx).ceil() as u64 + 1;
        for k in 2..blocks {
            assert_eq!(held.weigh(-0.015, dx, step, limit), Verdict::Held, "{k}");
        }
        assert!(blocks > CONFIRM_BLOCKS);
        assert_eq!(held.weigh(-0.015, dx, step, limit), Verdict::Step);
        // Residuals rising by a block's length a block (PTS held from going
        // back after a glitch ahead) never stay at one level that long.
        let mut held = Held::new(-0.015);
        for k in 1..19 {
            let r = -0.015 + dx * f64::from(k);
            assert_eq!(held.weigh(r, dx, step, limit), Verdict::Held, "{k}");
        }
        assert_eq!(held.weigh(-0.0015, dx, step, limit), Verdict::Back);
    }
}
