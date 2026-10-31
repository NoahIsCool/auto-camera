//! Drift correction: an audio input on its own sample clock put back on the
//! master clock by resampling it a few parts per million, so that what it
//! delivers matches what the master clock asks for and the program pacer
//! never inserts silence or cuts audio to absorb a crystal's error.
//!
//! # The loop
//!
//! [`crate::drift`] measures the input's timing error `û` (seconds ahead of
//! the master clock) and its slope `v̂` along input time, from the blocks'
//! PTS. A [`Resampler`] then makes `ρ = 1 + c` output frames per input
//! frame, with the correction
//!
//! ```text
//! c = −v̂ − E / τₛ        E = û + (Δ + s) / rate
//! ```
//!
//! held within ±[`DriftSettings::max_correction_ppm`] and changed by at
//! most [`DriftSettings::max_slew_ppm_per_s`] per second of audio. `Δ` is
//! the time the resampling has added so far, in frames
//! ([`Resampler::stretch`]), and `s` the frames of silence put in for
//! steps less the frames cut for them (see below), so `E` is the timing
//! error of what comes *out*, against the epoch's timing reference (its
//! first second, less any step in it, [`crate::drift`]). The first term
//! cancels the drift (`1 − v̂` is the ratio that does); the second takes up
//! the timing error that is left, over `τₛ` = [`DriftSettings::settle`]
//! (60 s): with the drift cancelled, `dE/dx = −E/τₛ`. Over a show the
//! output therefore keeps the timing it had a second into the epoch, and
//! the pacer's lead stays within block jitter. Between blocks the ratio
//! ramps, so it never jumps; at a 100 ppm drift it settles within a
//! minute, the timing error peaking at a couple of milliseconds on the
//! way.
//!
//! # Dead band
//!
//! Until the estimator has its timing reference, and then while the
//! measured drift is within ±[`DriftSettings::dead_band_ppm`], or not yet
//! [`CONFIDENCE`] standard errors clear of it
//! ([`DriftEstimator::drift_uncertainty_ppm`]: early in an epoch, or while
//! the PTS's jitter could account for it), and the timing error `E` is
//! within ±[`DriftSettings::dead_band_error`], blocks are not resampled. A
//! block nothing is done to is [`Corrected::Unchanged`]: the caller passes
//! on the very block it had, bit for bit. A file played on the master
//! clock, which counts its PTS from its samples, measures exactly zero and
//! is never resampled; nor is an input on the master clock whose PTS
//! merely jitter. Meanwhile the corrector keeps the last frames it passed
//! on; once either measure leaves its band, the resampler starts from
//! them, so its first output is the frame after the last one passed on and
//! the stream carries on without a seam (it holds back about
//! [`crate::resample::HALF_TAPS`] frames from then on). It stays on until
//! the epoch ends: going back to passing blocks through in the middle of
//! one would move the stream by a fraction of a sample.
//!
//! # Steps
//!
//! A step in the PTS within the discontinuity ([`Observation::Step`]) is
//! made up as soon as the estimator has confirmed it, not taken up slowly:
//! for audio lost (a step forward by `g`) the corrector puts `g` of silence
//! ahead of the block that confirmed it, through the resampler while it is
//! on, and for a step back it cuts as much from the start of the blocks
//! from that one on. Both count in `s`, so the step leaves `E` where it was
//! and moves nothing else; the estimator keeps it out of the drift. No
//! block is held back while the estimator waits for the blocks that confirm
//! a step ([`Observation::Held`]): they pass as they would have, a few
//! blocks of audio `g` early (or late), and a timestamp that strayed and
//! came back before the blocks confirmed it (within three blocks; see
//! [`crate::drift`]) is never made up at all: a glitch in the PTS is not
//! edited into the audio. One that stayed off long enough to be confirmed
//! is made up like a step, and its way back is a step the other way, made
//! up in turn once confirmed: two edits of its size that cancel. A step in
//! an epoch's first second is made up too, and the timing reference is net
//! of it, so `E` counts from the level before the step and the silence is
//! not taken back over the settle. Making up lost audio here, once the PTS
//! show it, is what keeps it from being made up twice. The program pacer
//! fills a gap only when its slack runs out before the audio after it
//! comes, which a loss close to the slack can do as well; its silence then
//! comes first, and the corrector's arrives after it like the late backlog
//! of a stall, from which the pacer cuts exactly the silence it owes. Every
//! step made up is counted ([`DriftStats::steps`]).
//!
//! # Breaks
//!
//! A discontinuity ([`Observation::Discontinuity`]: a reconnection, a long
//! stall whose audio was lost) ends the epoch. The frames the resampler
//! held are let go as they are, ahead of the block, the correction and `s`
//! return to zero, and the new epoch passes through until its own drift
//! leaves the dead band. The step is never taken for drift: the pacer,
//! whose slack such a gap is beyond, fills it.
//!
//! # Invariants
//!
//! * Pure and deterministic: PTS and samples in, samples out. No clock;
//!   time is PTS and sample counts.
//! * [`Corrected::Unchanged`] is returned only for a block the corrector did
//!   not change in any way; a block that is empty or not whole frames is
//!   left unchanged and not measured.
//! * A step moves the output by the step and leaves `E` where it was; a
//!   block the estimator holds is passed on like any other, never held
//!   back.
//! * The correction is finite, within its limit, and moves by at most its
//!   slew.

use std::time::Duration;

use crate::drift::{DriftError, DriftEstimator, DriftSettings, Observation};
use crate::resample::{HALF_TAPS, MAX_CHANNELS, Resampler};

/// Standard errors by which a measured drift must stand clear of its dead
/// band before it is corrected; see the module docs.
pub const CONFIDENCE: f64 = 3.0;

/// What became of a block.
#[derive(Debug, Clone, PartialEq)]
pub enum Corrected {
    /// Pass the block on as it is.
    Unchanged,
    /// Pass these samples on instead.
    Resampled(Resampled),
}

/// Samples to pass on in place of a block.
#[derive(Debug, Clone, PartialEq)]
pub struct Resampled {
    /// Interleaved, in the block's format; possibly empty.
    pub samples: Vec<f32>,
    /// The first of them stands for the input this many frames before the
    /// block's first frame: the frames the resampler held, or let go at a
    /// break, and the silence put in ahead of the block for a step; below
    /// zero when the start of the block was cut for a step back. The
    /// samples' PTS is the block's less this many frames.
    pub behind: f64,
}

/// What a corrector reports.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DriftStats {
    /// Measured drift of the input's clock, in ppm: above zero while it
    /// runs fast.
    pub drift_ppm: f64,
    /// The correction applied, in ppm of the input's rate: below zero while
    /// samples are taken out (a fast clock), zero while blocks pass
    /// through.
    pub correction_ppm: f64,
    /// Breaks that reset the measurement.
    pub resets: u64,
    /// Steps made up with silence or a cut.
    pub steps: u64,
    /// The input is being resampled.
    pub resampling: bool,
}

/// Puts one input's audio on the master clock; see the module docs.
#[derive(Debug, Clone)]
pub struct DriftCorrector {
    /// Nominal sample rate.
    rate: u32,
    /// Interleaved channels.
    channels: u16,
    /// Limits and time constants.
    settings: DriftSettings,
    /// Measures the drift.
    estimator: DriftEstimator,
    /// Resamples, once the epoch's drift left the dead band.
    resampler: Option<Resampler>,
    /// The last frames passed on unresampled (up to `HALF_TAPS − 1`), for
    /// the resampler to start from.
    history: Vec<f32>,
    /// The correction `c` applied now.
    correction: f64,
    /// `s`: frames of silence put in for steps less frames cut for them,
    /// this epoch.
    shifted: i64,
    /// Frames of a step back still to cut from the blocks to come.
    cut: u64,
}

impl DriftCorrector {
    /// A corrector for interleaved audio of nominal `rate` Hz and `channels`
    /// channels (1 to [`MAX_CHANNELS`]), with `settings`.
    pub fn new(rate: u32, channels: u16, settings: DriftSettings) -> Result<Self, DriftError> {
        if channels == 0 || usize::from(channels) > MAX_CHANNELS {
            return Err(DriftError::Channels(channels));
        }
        let estimator = DriftEstimator::new(rate, &settings)?;
        Ok(Self {
            rate,
            channels,
            settings,
            estimator,
            resampler: None,
            history: Vec::with_capacity((HALF_TAPS - 1) * usize::from(channels)),
            correction: 0.0,
            shifted: 0,
            cut: 0,
        })
    }

    /// The sample rate and channel count it corrects.
    pub fn format(&self) -> (u32, u16) {
        (self.rate, self.channels)
    }

    /// What it reports now.
    pub fn stats(&self) -> DriftStats {
        DriftStats {
            drift_ppm: self.estimator.drift_ppm(),
            correction_ppm: self.correction * 1e6,
            resets: self.estimator.resets(),
            steps: self.estimator.steps(),
            resampling: self.resampler.is_some(),
        }
    }

    /// Corrects a block of interleaved `samples` whose first frame is at
    /// `pts`; see the module docs.
    pub fn process(&mut self, pts: Duration, samples: &[f32]) -> Corrected {
        let ch = usize::from(self.channels);
        if samples.is_empty() || !samples.len().is_multiple_of(ch) {
            return Corrected::Unchanged;
        }
        let frames = samples.len() / ch;
        let rate = f64::from(self.rate);
        let mut out = Vec::new();
        // Frames of silence to put ahead of the block, for audio lost.
        let mut fill = 0;
        match self.estimator.observe(pts, frames as u64) {
            Observation::Discontinuity => {
                if let Some(resampler) = self.resampler.take() {
                    resampler.release(&mut out);
                }
                (self.correction, self.shifted, self.cut) = (0.0, 0, 0);
            }
            Observation::Step(by) => {
                // Within the discontinuity: 4,800 frames at 48 kHz by default.
                let n = (by.abs() * rate).round() as u64;
                if by < 0.0 {
                    fill = n as usize;
                    self.shifted += n as i64;
                } else {
                    self.cut += n;
                    self.shifted -= n as i64;
                }
            }
            Observation::Start | Observation::Tracked | Observation::Held => {}
        }
        let released = out.len() / ch;
        let skip = frames.min(usize::try_from(self.cut).unwrap_or(usize::MAX));
        self.cut -= skip as u64;
        let kept = &samples[skip * ch..];
        if self.resampler.is_none() && self.outside_dead_band() {
            self.resampler = Some(Resampler::with_history(self.channels, &self.history, 1.0));
        }
        let error = self.timing_error();
        if let Some(resampler) = &mut self.resampler {
            let behind = (released + fill) as f64 + resampler.waiting() - skip as f64;
            let max = self.settings.max_correction_ppm * 1e-6;
            let target = (-self.estimator.slope() - error / self.settings.settle.as_secs_f64())
                .clamp(-max, max);
            let slew = self.settings.max_slew_ppm_per_s * 1e-6 * frames as f64 / rate;
            self.correction += (target - self.correction).clamp(-slew, slew);
            let step = 1.0 / (1.0 + self.correction);
            out.reserve((fill + 1) * ch + kept.len());
            if fill > 0 {
                resampler.process(&vec![0.0; fill * ch], step, &mut out);
            }
            resampler.process(kept, step, &mut out);
            return Corrected::Resampled(Resampled {
                samples: out,
                behind,
            });
        }
        if released + fill + skip == 0 {
            self.remember(samples);
            return Corrected::Unchanged;
        }
        out.resize(out.len() + fill * ch, 0.0);
        out.extend_from_slice(kept);
        self.remember(&out);
        Corrected::Resampled(Resampled {
            samples: out,
            behind: (released + fill) as f64 - skip as f64,
        })
    }

    /// The timing error `E` of what comes out, in seconds: the input's,
    /// with the time the resampling added and the silence put in for steps
    /// less what was cut for them; see the module docs.
    fn timing_error(&self) -> f64 {
        let stretch = self.resampler.as_ref().map_or(0.0, Resampler::stretch);
        self.estimator.error() + (stretch + self.shifted as f64) / f64::from(self.rate)
    }

    /// True once the estimator has its timing reference and the measured
    /// drift (less [`CONFIDENCE`] standard errors) or the timing error `E`
    /// is outside its dead band.
    fn outside_dead_band(&self) -> bool {
        let sure = CONFIDENCE * self.estimator.drift_uncertainty_ppm();
        self.estimator.has_reference()
            && (self.estimator.drift_ppm().abs() - sure > self.settings.dead_band_ppm
                || self.timing_error().abs() > self.settings.dead_band_error.as_secs_f64())
    }

    /// Keeps the last `HALF_TAPS − 1` frames of what was passed on, ending
    /// with `samples` (whole frames).
    fn remember(&mut self, samples: &[f32]) {
        let keep = (HALF_TAPS - 1) * usize::from(self.channels);
        if samples.len() >= keep {
            self.history.clear();
            self.history
                .extend_from_slice(&samples[samples.len() - keep..]);
        } else {
            self.history.extend_from_slice(samples);
            let excess = self.history.len().saturating_sub(keep);
            self.history.drain(..excess);
        }
    }
}

#[cfg(test)]
mod tests;
