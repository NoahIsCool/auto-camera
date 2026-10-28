//! True peak (ITU-R BS.1770-4 Annex 2): the peak of the signal between its
//! samples.
//!
//! A sampled signal's largest sample can be well below the largest value
//! of the continuous signal it stands for: a sine at a quarter of the
//! sample rate, sampled 45° off its crests, has every sample 3 dB below its
//! crests. A DAC or a lossy encoder rebuilds the crests and can clip on
//! them. BS.1770 estimates the continuous peak by oversampling 4× and
//! taking the largest absolute value. [`TruePeak`] does that with the
//! interpolation filter Annex 2 gives: a 48-tap low pass at the input's
//! Nyquist frequency, run as four 12-tap phases, each of which interpolates
//! one point per input sample.
//!
//! The filter is defined relative to the input rate, so the same table
//! serves every rate (at 96 kHz and above BS.1770 allows less
//! oversampling; 4× is then only more exact). Its four phases interpolate
//! at the odd eighths between samples (1/8, 3/8, 5/8 and 7/8 of the way),
//! never on a sample itself, so [`TruePeak`] also counts the sample: a true
//! peak is never below the sample peak, which a sine at a quarter of the
//! rate sampled on its crests would otherwise undercut by 0.2 dB. The
//! points come out six samples late, which does not matter for a maximum.
//! At 48 kHz the reading is within +0.25/−0.1 dB of the true crest for
//! sines up to 20 kHz, the ripple of the Annex 2 filter; Tech 3341 allows
//! +0.2/−0.4 dB on its test signals.

/// Taps per phase.
pub const TAPS: usize = 12;

/// Oversampling factor.
pub const FACTOR: usize = 4;

/// The phases of BS.1770-4 Annex 2, Table 1: phase `p`, tap `k` multiplies
/// the input `k` samples back. The values are exact multiples of 2⁻¹³;
/// phases 2 and 3 are phases 1 and 0 reversed.
pub const PHASES: [[f64; TAPS]; FACTOR] = [
    [
        0.0017089843750,
        0.0109863281250,
        -0.0196533203125,
        0.0332031250000,
        -0.0594482421875,
        0.1373291015625,
        0.9721679687500,
        -0.1022949218750,
        0.0476074218750,
        -0.0266113281250,
        0.0148925781250,
        -0.0083007812500,
    ],
    [
        -0.0291748046875,
        0.0292968750000,
        -0.0517578125000,
        0.0891113281250,
        -0.1665039062500,
        0.4650878906250,
        0.7797851562500,
        -0.2003173828125,
        0.1015625000000,
        -0.0582275390625,
        0.0330810546875,
        -0.0189208984375,
    ],
    [
        -0.0189208984375,
        0.0330810546875,
        -0.0582275390625,
        0.1015625000000,
        -0.2003173828125,
        0.7797851562500,
        0.4650878906250,
        -0.1665039062500,
        0.0891113281250,
        -0.0517578125000,
        0.0292968750000,
        -0.0291748046875,
    ],
    [
        -0.0083007812500,
        0.0148925781250,
        -0.0266113281250,
        0.0476074218750,
        -0.1022949218750,
        0.9721679687500,
        0.1373291015625,
        -0.0594482421875,
        0.0332031250000,
        -0.0196533203125,
        0.0109863281250,
        0.0017089843750,
    ],
];

/// The 4× interpolator of one channel; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct TruePeak {
    /// The last [`TAPS`] inputs, twice over, so that
    /// `history[pos..pos + TAPS]` is them newest first without wrapping.
    history: [f64; 2 * TAPS],
    /// Where the newest input is.
    pos: usize,
}

impl Default for TruePeak {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeak {
    /// An interpolator whose history is silence.
    pub fn new() -> Self {
        Self {
            history: [0.0; 2 * TAPS],
            pos: 0,
        }
    }

    /// Takes the next input sample and returns the largest absolute value
    /// among it and the four points interpolated with it.
    pub fn push(&mut self, x: f64) -> f64 {
        self.pos = self.pos.checked_sub(1).unwrap_or(TAPS - 1);
        self.history[self.pos] = x;
        self.history[self.pos + TAPS] = x;
        let recent = &self.history[self.pos..self.pos + TAPS];
        PHASES.iter().fold(x.abs(), |peak, phase| {
            let y: f64 = phase.iter().zip(recent).map(|(c, s)| c * s).sum();
            peak.max(y.abs())
        })
    }

    /// Forgets the history.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::sine;

    /// The largest value [`TruePeak`] reports over `samples`, in dB,
    /// leaving out the first `skip` (the onset of a signal that starts
    /// abruptly overshoots, as any band-limited step does).
    fn peak_db(samples: &[f32], skip: usize) -> f64 {
        let mut tp = TruePeak::new();
        let peak = samples
            .iter()
            .map(|&s| tp.push(f64::from(s)))
            .skip(skip)
            .fold(0.0, f64::max);
        20.0 * peak.log10()
    }

    /// The largest absolute sample, in dB.
    fn sample_db(samples: &[f32]) -> f64 {
        let peak = samples
            .iter()
            .map(|s| f64::from(s.abs()))
            .fold(0.0, f64::max);
        20.0 * peak.log10()
    }

    #[test]
    fn the_table_is_symmetric_and_close_to_unity_gain() {
        for p in 0..FACTOR {
            let mut reversed = PHASES[FACTOR - 1 - p];
            reversed.reverse();
            assert_eq!(PHASES[p], reversed, "phase {p}");
            for c in PHASES[p] {
                assert_eq!((c * 8192.0).fract(), 0.0, "{c} is a multiple of 2^-13");
            }
            // A constant input comes out within a quarter dB.
            let dc: f64 = PHASES[p].iter().sum();
            assert!((20.0 * dc.log10()).abs() < 0.25, "phase {p}: {dc}");
        }
    }

    #[test]
    fn an_inter_sample_crest_is_found() {
        // A quarter of the rate, 45° off the crests: every sample is at
        // −3 dB of the crest, −9.03 dBFS for a −6.02 dBFS sine.
        let x = sine(48_000, 1, 12_000.0, -6.0206, 45.0, 0.1);
        assert!((sample_db(&x) + 9.0309).abs() < 0.01, "{}", sample_db(&x));
        let tp = peak_db(&x, 0);
        assert!((-6.0206 - 0.4..=-6.0206 + 0.2).contains(&tp), "{tp}");
    }

    #[test]
    fn crests_on_samples_read_the_sample_peak() {
        let x = sine(48_000, 1, 12_000.0, -6.0206, 90.0, 0.1);
        assert!((peak_db(&x, 2 * TAPS) - sample_db(&x)).abs() < 1e-9);
    }

    #[test]
    fn sines_read_their_crest_across_the_band() {
        for freq in [100.0, 997.0, 5_000.0, 12_050.0, 15_000.0, 19_000.0] {
            for phase in [0.0, 30.0, 60.0] {
                let x = sine(48_000, 1, freq, -1.0, phase, 0.2);
                let tp = peak_db(&x, 2 * TAPS);
                assert!((-1.1..=-0.75).contains(&tp), "{freq} Hz {phase}°: {tp}");
            }
        }
    }

    #[test]
    fn an_impulse_overshoots_and_silence_reads_zero() {
        let mut tp = TruePeak::new();
        assert_eq!(tp.push(0.0), 0.0);
        let mut peak = tp.push(1.0);
        for _ in 0..TAPS {
            peak = peak.max(tp.push(0.0));
        }
        // The sample itself is counted.
        assert!(peak >= 1.0, "{peak}");
        for _ in 0..TAPS {
            assert_eq!(tp.push(0.0), 0.0);
        }
        tp.push(0.5);
        tp.reset();
        assert_eq!(tp, TruePeak::new());
    }
}
