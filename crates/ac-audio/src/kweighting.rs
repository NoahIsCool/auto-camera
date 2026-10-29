//! K-weighting (ITU-R BS.1770-4 §2.1): the filter every loudness
//! measurement applies before taking the mean square.
//!
//! K-weighting is two biquads in series: a **high shelf** of about +4 dB
//! above 1.7 kHz, modelling the head as a rigid sphere, and the **RLB high
//! pass** (revised low-frequency B-curve), which takes out what is below
//! about 38 Hz. BS.1770 gives both as coefficients for 48 kHz only. Here
//! they are designed for the actual sample rate from the analogue
//! prototypes those 48 kHz values come from (shelf at 1681.97 Hz, +4.0 dB,
//! Q 0.7072; high pass at 38.135 Hz, Q 0.5003; the parametrisation
//! libebur128 uses), through the bilinear transform prewarped at each
//! corner (`K = tan(π·f₀/fs)`). At 48 kHz this reproduces the BS.1770
//! table to rounding; at other rates the response follows the same
//! analogue curve, so a 1 kHz sine reads the same loudness at 44.1 kHz as
//! at 48 kHz instead of being filtered with coefficients meant for another
//! rate.
//!
//! The filters run in transposed direct form II in `f64`. Fed digital
//! silence, an IIR filter's state decays towards zero for ever and becomes
//! subnormal after a few seconds, and subnormal arithmetic is very slow on
//! most CPUs. A section whose two state variables are both smaller than
//! [`FLUSH`] is therefore set to rest (both at once: zeroing one alone
//! changes the recursion, which can then grow back and cycle just above
//! the threshold). That is some 600 dB below full scale, so it changes no
//! reading, and silence then costs no more than audio and reads exactly
//! zero.

/// A section whose state is below this is set to rest (see the module
/// docs).
pub const FLUSH: f64 = 1e-30;

/// Corner frequency of the high shelf, in Hz.
const SHELF_HZ: f64 = 1681.974450955533;
/// Gain of the high shelf, in dB.
const SHELF_GAIN_DB: f64 = 3.999843853973347;
/// Quality factor of the high shelf.
const SHELF_Q: f64 = 0.7071752369554196;
/// Exponent relating the shelf's band gain to its high-frequency gain.
const SHELF_BAND_EXPONENT: f64 = 0.4996667741545416;
/// Corner frequency of the RLB high pass, in Hz.
const HIGH_PASS_HZ: f64 = 38.13547087602444;
/// Quality factor of the RLB high pass.
const HIGH_PASS_Q: f64 = 0.5003270373238773;

/// One second-order IIR section, `H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) /
/// (1 + a1·z⁻¹ + a2·z⁻²)`, in transposed direct form II.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    /// Feed-forward coefficients `b0, b1, b2`.
    b: [f64; 3],
    /// Feedback coefficients `a1, a2` (`a0` is 1).
    a: [f64; 2],
    /// The two state variables.
    s: [f64; 2],
}

impl Biquad {
    /// A section with feed-forward `b` and feedback `a` (`a0` = 1), at rest.
    pub fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, s: [0.0; 2] }
    }

    /// The coefficients: `([b0, b1, b2], [a1, a2])`.
    pub fn coefficients(&self) -> ([f64; 3], [f64; 2]) {
        (self.b, self.a)
    }

    /// Filters one sample.
    pub fn process(&mut self, x: f64) -> f64 {
        let [b0, b1, b2] = self.b;
        let [a1, a2] = self.a;
        let y = b0 * x + self.s[0];
        let s = [b1 * x - a1 * y + self.s[1], b2 * x - a2 * y];
        // Both or neither: flushing one alone changes the recursion, which
        // can then grow back and cycle just above the flush level.
        self.s = if s[0].abs() < FLUSH && s[1].abs() < FLUSH {
            [0.0; 2]
        } else {
            s
        };
        y
    }

    /// Returns the section to rest.
    pub fn reset(&mut self) {
        self.s = [0.0; 2];
    }

    /// True when the section is at rest (its output for silence is zero).
    pub fn is_at_rest(&self) -> bool {
        self.s == [0.0; 2]
    }
}

/// The K-weighting filter of one channel; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KWeighting {
    /// Stage 1: the high shelf.
    shelf: Biquad,
    /// Stage 2: the RLB high pass.
    high_pass: Biquad,
}

impl KWeighting {
    /// The filter for `sample_rate` Hz, at rest. The rate must be above
    /// twice the shelf's corner (about 3.4 kHz); the meter only builds
    /// filters for [`MIN_SAMPLE_RATE`](crate::MIN_SAMPLE_RATE) and up.
    pub fn new(sample_rate: u32) -> Self {
        let fs = f64::from(sample_rate);
        // Stage 1: high shelf.
        let k = (std::f64::consts::PI * SHELF_HZ / fs).tan();
        let vh = 10f64.powf(SHELF_GAIN_DB / 20.0);
        let vb = vh.powf(SHELF_BAND_EXPONENT);
        let a0 = 1.0 + k / SHELF_Q + k * k;
        let shelf = Biquad::new(
            [
                (vh + vb * k / SHELF_Q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / SHELF_Q + k * k) / a0,
            ],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / SHELF_Q + k * k) / a0],
        );
        // Stage 2: high pass; BS.1770 leaves its numerator unnormalised.
        let k = (std::f64::consts::PI * HIGH_PASS_HZ / fs).tan();
        let a0 = 1.0 + k / HIGH_PASS_Q + k * k;
        let high_pass = Biquad::new(
            [1.0, -2.0, 1.0],
            [
                2.0 * (k * k - 1.0) / a0,
                (1.0 - k / HIGH_PASS_Q + k * k) / a0,
            ],
        );
        Self { shelf, high_pass }
    }

    /// The two stages: high shelf, then high pass.
    pub fn stages(&self) -> [Biquad; 2] {
        [self.shelf, self.high_pass]
    }

    /// Filters one sample.
    pub fn process(&mut self, x: f64) -> f64 {
        self.high_pass.process(self.shelf.process(x))
    }

    /// Returns the filter to rest.
    pub fn reset(&mut self) {
        self.shelf.reset();
        self.high_pass.reset();
    }

    /// True when both stages are at rest.
    pub fn is_at_rest(&self) -> bool {
        self.shelf.is_at_rest() && self.high_pass.is_at_rest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gain in dB of `filter` at `freq` Hz, sampled at `rate` Hz,
    /// from its transfer function.
    fn gain_db(filter: &KWeighting, freq: f64, rate: u32) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / f64::from(rate);
        let mut magnitude = 1.0;
        for stage in filter.stages() {
            let (b, a) = stage.coefficients();
            // |Σ cₖ·e^{-jwk}| for k = 0, 1, 2.
            let mag = |c: [f64; 3]| {
                let re = c[0] + c[1] * w.cos() + c[2] * (2.0 * w).cos();
                let im = -c[1] * w.sin() - c[2] * (2.0 * w).sin();
                re.hypot(im)
            };
            magnitude *= mag(b) / mag([1.0, a[0], a[1]]);
        }
        20.0 * magnitude.log10()
    }

    #[test]
    fn at_48_khz_the_coefficients_are_the_bs1770_table() {
        let [shelf, high_pass] = KWeighting::new(48_000).stages();
        let table = [
            (
                shelf,
                [1.53512485958697, -2.69169618940638, 1.19839281085285],
                [-1.69065929318241, 0.73248077421585],
            ),
            (
                high_pass,
                [1.0, -2.0, 1.0],
                [-1.99004745483398, 0.99007225036621],
            ),
        ];
        for (stage, b, a) in table {
            let (got_b, got_a) = stage.coefficients();
            for (got, want) in got_b.iter().chain(&got_a).zip(b.iter().chain(&a)) {
                assert!((got - want).abs() < 1e-12, "{got} vs {want}");
            }
        }
    }

    #[test]
    fn the_curve_is_the_same_at_every_rate() {
        let reference = KWeighting::new(48_000);
        // About +0.69 dB at 1 kHz: what the −0.691 offset cancels.
        let at_1k = gain_db(&reference, 1000.0, 48_000);
        assert!((at_1k - 0.691).abs() < 0.01, "{at_1k}");
        for rate in [8_000, 11_025, 22_050, 44_100, 96_000, 192_000, 384_000] {
            let f = KWeighting::new(rate);
            let g = gain_db(&f, 1000.0, rate);
            assert!((g - at_1k).abs() < 0.05, "{rate} Hz: {g} dB at 1 kHz");
            // The high pass: well down at 10 Hz, gone at DC.
            assert!(gain_db(&f, 10.0, rate) < -10.0, "{rate} Hz");
            assert!(gain_db(&f, 1e-6, rate) < -100.0, "{rate} Hz");
        }
        // The shelf: about +4 dB well above its corner.
        for rate in [44_100, 48_000, 96_000] {
            let g = gain_db(&KWeighting::new(rate), 10_000.0, rate);
            assert!((g - 4.0).abs() < 0.3, "{rate} Hz: {g} dB at 10 kHz");
        }
    }

    #[test]
    fn a_sine_through_the_filter_gains_what_the_curve_says() {
        let rate = 44_100;
        let mut f = KWeighting::new(rate);
        let (mut energy_in, mut energy_out) = (0.0, 0.0);
        for n in 0..rate {
            let x = (2.0 * std::f64::consts::PI * 1000.0 * f64::from(n) / f64::from(rate)).sin();
            let y = f.process(x);
            if n >= rate / 10 {
                energy_in += x * x;
                energy_out += y * y;
            }
        }
        let measured = 10.0 * (energy_out / energy_in).log10();
        let expected = gain_db(&KWeighting::new(rate), 1000.0, rate);
        assert!(
            (measured - expected).abs() < 0.01,
            "{measured} vs {expected}"
        );
    }

    #[test]
    fn silence_brings_the_state_to_exactly_zero() {
        let mut f = KWeighting::new(48_000);
        assert!(f.is_at_rest());
        f.process(1.0);
        assert!(!f.is_at_rest());
        // The high pass's slowest pole decays by about 0.5 % a sample: about
        // 14 000 samples to fall below the flush level, never subnormal.
        let mut last = 1.0;
        for _ in 0..48_000 {
            last = f.process(0.0);
            assert!(last == 0.0 || last.is_normal(), "{last:e}");
        }
        assert_eq!(last, 0.0);
        assert!(f.is_at_rest());
        f.process(0.25);
        f.reset();
        assert_eq!(f, KWeighting::new(48_000));
    }
}
