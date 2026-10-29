//! Program loudness and peak metering after ITU-R BS.1770-4 and EBU R 128,
//! with the meter behaviour of EBU Tech 3341 (spec M12, "peak/LUFS meter"),
//! and drift correction for audio inputs on their own sample clock (spec
//! C.3, A/V sync drift-free over a show; see [Drift
//! correction](#drift-correction)).
//!
//! # What is measured
//!
//! Loudness is the mean square of the **K-weighted** signal, summed over the
//! channels with their weights `Gᵢ` and written in LUFS:
//!
//! ```text
//! L = −0.691 + 10·log10(Σᵢ Gᵢ·zᵢ)      zᵢ = mean of yᵢ², yᵢ the K-weighted channel i
//! ```
//!
//! K-weighting ([`kweighting`]) is a high shelf (+4 dB above about 1.7 kHz,
//! the acoustic effect of the head) followed by a high pass (below about
//! 38 Hz); the −0.691 offset cancels its gain at 1 kHz, so a full-scale
//! 1 kHz sine on both channels of a stereo pair reads 0 LUFS, and one at
//! −23 dBFS reads −23 LUFS. [`LoudnessMeter`] cuts the audio into 100 ms
//! *hops* and keeps the K-weighted energy of each:
//!
//! * **momentary** loudness is the power mean of the last 400 ms (4 hops),
//!   **short-term** loudness that of the last 3 s (30 hops); both are
//!   ungated and refreshed at every hop (10 Hz);
//! * **integrated** loudness is gated ([`gating`]): each 400 ms block (one
//!   per hop, so blocks overlap by 75 %) counts if it is above the absolute
//!   gate of −70 LUFS, then blocks more than 10 LU below the power mean of
//!   those are dropped as pauses (the relative gate), and the power mean of
//!   the rest is the programme's loudness, so silence and quiet passages do
//!   not pull it down;
//! * **sample peak** is the largest absolute sample since the start, and
//!   **true peak** ([`true_peak`]) the largest absolute value of the signal
//!   oversampled 4× with the interpolation filter of BS.1770-4 Annex 2,
//!   which finds the crests between samples that a DAC or a lossy encoder
//!   rebuilds (and can clip on).
//!
//! Every channel is weighted 1.0, BS.1770's weight for left, right and
//! centre. Program audio is one stereo input (spec M12); surround layouts,
//! with their 1.41 weights and an excluded LFE, are not modelled. A mono
//! signal is measured as one channel, as BS.1770 defines it: it reads 3 dB
//! below the same signal on both channels of a stereo pair.
//!
//! # Invariants
//!
//! * Pure and deterministic: samples and their sample rate in, readings
//!   out. There is no I/O and no clock; the meter's time is its sample
//!   count, and the same samples give bit-identical readings however they
//!   are cut into blocks.
//! * A reading never holds NaN or an infinity. "No reading" is explicit:
//!   `None` while a window has not filled yet, while it holds digital
//!   silence (whose level is −∞) and while no block has passed the gates.
//! * Input is measured whole or refused whole with a [`MeterError`], and a
//!   NaN or infinite sample is measured as silence, so no input can poison
//!   the filters' state for the rest of a show.
//! * Memory is fixed for a show of any length: integrated loudness keeps a
//!   histogram of block loudness ([`gating::GatedLoudness`]), not every
//!   block.
//!
//! # Drift correction
//!
//! A soundboard's USB interface samples on its own crystal, a few tens of
//! parts per million off the clock the engine runs on: over three hours,
//! 100 ppm is a second. [`drift`] measures that drift from the blocks' PTS
//! (master-clock times) against their sample counts, filtering the PTS's
//! jitter and never taking a step in them for drift: a small one (audio
//! lost) is timing error, a large one (a reconnection) a break, and a
//! timestamp that strays for a block or a few and comes back is neither.
//! [`correct`] cancels the drift by resampling the input a few ppm with
//! [`resample`]'s band-limited windowed-sinc resampler, whose ratio ramps
//! and never jumps, makes up a small step with silence or a cut once the
//! blocks after it confirm it, and leaves the blocks bit for bit as they
//! are while the drift is within a dead band, as it always is for a file
//! played on the engine's clock. Like the meter, all of it is pure:
//! samples, PTS and sample counts in, samples and estimates out.

pub mod correct;
pub mod drift;
pub mod gating;
pub mod kweighting;
pub mod meter;
pub mod resample;
pub mod true_peak;

#[cfg(test)]
mod signals;

pub use correct::{Corrected, DriftCorrector, DriftStats, Resampled};
pub use drift::{DriftError, DriftEstimator, DriftSettings, Observation};
pub use gating::GatedLoudness;
pub use kweighting::KWeighting;
pub use meter::{
    LoudnessMeter, LoudnessReading, MAX_CHANNELS, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE, MeterError,
};
pub use resample::Resampler;
pub use true_peak::TruePeak;

/// BS.1770's offset from `10·log10` of the weighted mean square to LUFS.
pub const LOUDNESS_OFFSET: f64 = -0.691;

/// The loudness in LUFS of a channel-weighted mean square `power`, or
/// `None` when `power` is zero (digital silence, −∞ LUFS), negative or not
/// finite.
pub fn loudness(power: f64) -> Option<f64> {
    (power > 0.0 && power.is_finite()).then(|| LOUDNESS_OFFSET + 10.0 * power.log10())
}

/// The channel-weighted mean square whose loudness is `lufs`: the inverse
/// of [`loudness`].
pub fn power_at(lufs: f64) -> f64 {
    10f64.powf((lufs - LOUDNESS_OFFSET) / 10.0)
}

/// The level in dB (dBFS for a sample, dBTP for a true peak) of a linear
/// `amplitude` (1.0 is full scale), or `None` when it is zero, negative or
/// not finite.
pub fn decibels(amplitude: f64) -> Option<f64> {
    (amplitude > 0.0 && amplitude.is_finite()).then(|| 20.0 * amplitude.log10())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudness_and_power_are_inverses() {
        for lufs in [-70.0, -23.0, -0.691, 0.0, 12.5] {
            let back = loudness(power_at(lufs)).unwrap();
            assert!((back - lufs).abs() < 1e-12, "{lufs} -> {back}");
        }
        assert!((loudness(1.0).unwrap() - LOUDNESS_OFFSET).abs() < 1e-15);
    }

    #[test]
    fn no_level_is_explicit_not_infinite() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(loudness(bad), None, "{bad}");
            assert_eq!(decibels(bad), None, "{bad}");
        }
        assert_eq!(decibels(1.0), Some(0.0));
        assert!((decibels(0.5).unwrap() + 6.0206).abs() < 1e-4);
        // Tiny but positive levels are real readings, and finite.
        assert!(loudness(f64::MIN_POSITIVE).unwrap().is_finite());
    }
}
