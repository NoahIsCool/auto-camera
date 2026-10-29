//! Band-limited fractional resampling for ratios near one: how the drift
//! corrector ([`crate::correct`]) takes a few samples per million out of an
//! input whose clock runs fast, or puts them into one that runs slow,
//! without a click and without dulling the music.
//!
//! # Theory
//!
//! Samples stand for the one band-limited signal that passes through them
//! (Shannon): with time `t` counted in input sample periods,
//! `x(t) = Σₙ x[n]·sinc(t − n)`. Resampling evaluates that signal at new
//! instants `t₀, t₀ + s, t₀ + 2s, …`, where the *step* `s` is input frames
//! per output frame: a step above one takes samples out, below one puts
//! them in. Interpolating linearly between neighbours instead is a low pass
//! that dulls the top octave and folds images of it back as distortion,
//! which is audible on music; here the sinc is kept, cut to the
//! [`TAPS`] input samples nearest the instant and shaped by a Kaiser window
//! (β = [`KAISER_BETA`]), the windowed-sinc interpolator of Smith and
//! Gossett.
//!
//! The kernel is tabulated at [`PHASES`] fractional offsets per input
//! sample (a polyphase table), and the coefficients for an instant between
//! two table rows are interpolated linearly between them (applied as the
//! same mix of the two rows' outputs). That is interpolation of the
//! *filter*, whose error falls with the square of the table's density, not
//! of the signal. Each row is scaled to a gain of
//! exactly one at DC, so a constant comes out constant at every phase.
//! Measured over every phase, the interpolator is within −105 dB of an
//! ideal delay up to 20 kHz at 48 kHz (18 kHz at 44.1 kHz); above that it
//! rolls off, to −6 dB at the Nyquist frequency.
//!
//! # Cutoff
//!
//! The cutoff is the input's Nyquist frequency, which serves both
//! directions while the step stays within [`MAX_STEP_DEVIATION`] of one:
//! the images of passband content that the window leaves lie above
//! `rate − 20 kHz`, and resampling by a ratio this close to one puts them
//! back within a few hertz of the content itself, below it by the stopband.
//! It is not a general sample-rate converter, and a step further from one
//! is held at that limit.
//!
//! # Latency and continuity
//!
//! An output needs [`HALF_TAPS`] input frames past its instant, so about
//! that many frames wait in the resampler between calls (half a
//! millisecond at 48 kHz). A resampler started with the frames that came
//! before its first input ([`Resampler::with_history`]) carries a stream on
//! exactly where passing it through stopped: phase zero is the identity
//! (the centre tap is one and every other tap zero), so its first output is
//! its first input frame, bit for bit, and the outputs after it change
//! only as the instants move away from the input's. The frames still
//! waiting can be let go as they are ([`Resampler::release`]), for a stream
//! that stops being resampled at a seam.
//!
//! # Ratio changes
//!
//! [`Resampler::process`] takes the step to reach by the end of the call
//! and ramps to it linearly from the previous call's across the call's
//! outputs, so the ratio never jumps. The read position and the step are
//! fixed point, in 2⁻⁴⁰ of a frame: the position moves by exact integer
//! sums, and the step is resolved far below any drift worth correcting.
//!
//! # Invariants
//!
//! * Pure and deterministic: samples and steps in, samples out, the same
//!   for the same inputs however they are cut into calls of equal steps.
//! * Bounded memory: between calls only the frames a later output needs
//!   are kept (fewer than [`TAPS`] plus one call's input).
//! * A non-finite input sample is taken as silence, so one bad sample
//!   cannot poison the outputs after it.
//! * Silence is cheap and exact: an output whose kernel reaches only
//!   digital silence is zero without arithmetic, the very value the sums
//!   give, so a quiet input costs a fraction of a loud one.
//! * The books are exact: [`Resampler::stretch`] is output frames minus the
//!   input frames the read position has passed, the time the resampling has
//!   added (negative: taken out) so far, from integer counts.

use std::f64::consts::PI;
use std::sync::OnceLock;

/// Input frames on each side of an output instant that the kernel reaches.
pub const HALF_TAPS: usize = 24;

/// Taps of the kernel: input frames weighed for one output frame.
pub const TAPS: usize = 2 * HALF_TAPS;

/// Rows of the kernel table per input sample period.
pub const PHASES: usize = 512;

/// The Kaiser window's shape: about 105 dB of stopband for 48 taps.
pub const KAISER_BETA: f64 = 12.0;

/// How far from one the step may go (1 %, ten thousand ppm); see the
/// module docs.
pub const MAX_STEP_DEVIATION: f64 = 0.01;

/// Most channels a resampler takes.
pub const MAX_CHANNELS: usize = 8;

/// Partial sums of a dot product.
const LANES: usize = 8;

/// Fractional bits of the read position and the step, which are fixed
/// point: the position moves by exact integer sums, so the books balance
/// to the bit and the loop that moves it is short, and a step is resolved
/// to 2⁻⁴⁰ of a frame (a millionth of a ppm).
const FRAC_BITS: u32 = 40;

/// One input frame, in fixed point.
const ONE: u64 = 1 << FRAC_BITS;

/// Bits of the read position below a table row's: how far an instant is
/// from its row to the next.
const MIX_BITS: u32 = FRAC_BITS - PHASES.trailing_zeros();

/// The read position's bits below a table row's.
const MIX_MASK: u64 = (1 << MIX_BITS) - 1;

/// One table row's span of the read position, inverted.
const MIX_SCALE: f32 = 1.0 / (1u64 << MIX_BITS) as f32;

// A row index is the read position's top bits.
const _: () = assert!(PHASES.is_power_of_two() && PHASES.trailing_zeros() < FRAC_BITS);

/// The modified Bessel function of the first kind, order zero, by its power
/// series `Σₖ ((x/2)ᵏ / k!)²`, for the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..500 {
        let k = f64::from(k);
        term *= q / (k * k);
        sum += term;
        if term <= sum * 1e-17 {
            break;
        }
    }
    sum
}

/// The kernel at `x` input sample periods from an output instant: the
/// Kaiser-windowed sinc, exactly one at zero and exactly zero at every other
/// whole number and beyond [`HALF_TAPS`].
pub fn kernel(x: f64) -> f64 {
    let half = HALF_TAPS as f64;
    if x.is_nan() || x.abs() >= half {
        return 0.0;
    }
    if x == 0.0 {
        return 1.0;
    }
    if x.fract() == 0.0 {
        return 0.0;
    }
    let r = x / half;
    let window = bessel_i0(KAISER_BETA * (1.0 - r * r).sqrt()) / bessel_i0(KAISER_BETA);
    (PI * x).sin() / (PI * x) * window
}

/// The polyphase table: row `p` (of `PHASES + 1`), tap `t` weighs input
/// frame `t − (HALF_TAPS − 1)` from the frame at or before an instant
/// `p / PHASES` of a period after it, each row scaled to unit DC gain. Row
/// `PHASES` is row 0 one tap later, so every instant has a row on each
/// side. Built once, on first use.
fn table() -> &'static [f32] {
    static TABLE: OnceLock<Box<[f32]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Vec::with_capacity((PHASES + 1) * TAPS);
        for p in 0..=PHASES {
            let mu = p as f64 / PHASES as f64;
            let row: Vec<f64> = (0..TAPS)
                .map(|t| kernel(t as f64 - (HALF_TAPS - 1) as f64 - mu))
                .collect();
            let gain: f64 = row.iter().sum();
            table.extend(row.iter().map(|h| (h / gain) as f32));
        }
        table.into_boxed_slice()
    })
}

/// `step`, or one when it is not finite, held within
/// [`MAX_STEP_DEVIATION`] of one.
fn usable_step(step: f64) -> f64 {
    if step.is_finite() {
        step.clamp(1.0 - MAX_STEP_DEVIATION, 1.0 + MAX_STEP_DEVIATION)
    } else {
        1.0
    }
}

/// `step` held as [`usable_step`] holds it, in fixed point.
fn fixed(step: f64) -> u64 {
    (usable_step(step) * ONE as f64).round() as u64
}

/// `v`, or silence when it is not finite.
fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// A band-limited resampler of interleaved audio; see the module docs.
#[derive(Debug, Clone)]
pub struct Resampler {
    /// Each channel's input frames, from the oldest a later output still
    /// needs.
    planes: Vec<Vec<f32>>,
    /// Index in the planes of the input frame at or just before the next
    /// output instant (the read position's whole part); at least
    /// `HALF_TAPS − 1`, the frames before it that the kernel reaches.
    pos: usize,
    /// The read position's fractional part, in [`ONE`]ths of a frame:
    /// below [`ONE`].
    frac: u64,
    /// Input frames per output frame at the end of the latest call, in
    /// [`ONE`]ths of a frame.
    step: u64,
    /// Output frames made.
    produced: u64,
    /// Whole input frames the read position has passed.
    passed: u64,
    /// Index in the planes from which every frame is digital silence on
    /// every channel: an output whose kernel reaches only those is silence,
    /// with no arithmetic.
    silent_from: usize,
}

impl Resampler {
    /// A resampler of `channels` channels (1 to [`MAX_CHANNELS`]; held
    /// there), starting at `step` (held as [`Resampler::process`] holds
    /// it), whose first input follows `history`: interleaved frames that
    /// came before it, of which the last `HALF_TAPS − 1` are used (silence
    /// where there are fewer). A trailing partial frame is ignored.
    pub fn with_history(channels: u16, history: &[f32], step: f64) -> Self {
        let ch = usize::from(channels).clamp(1, MAX_CHANNELS);
        let keep = HALF_TAPS - 1;
        let frames = (history.len() / ch).min(keep);
        let tail = &history[(history.len() / ch - frames) * ch..][..frames * ch];
        let planes = (0..ch)
            .map(|c| {
                let mut plane = vec![0.0; keep - frames];
                plane.extend(tail.iter().skip(c).step_by(ch).map(|&v| finite(v)));
                plane
            })
            .collect();
        // Silence runs from the frame after the history's last sound.
        let silent_from = tail
            .iter()
            .rposition(|&v| finite(v) != 0.0)
            .map_or(0, |i| keep - frames + i / ch + 1);
        Self {
            planes,
            pos: keep,
            frac: 0,
            step: fixed(step),
            produced: 0,
            passed: 0,
            silent_from,
        }
    }

    /// Channels.
    pub fn channels(&self) -> usize {
        self.planes.len()
    }

    /// Input frames per output frame now.
    pub fn step(&self) -> f64 {
        self.step as f64 / ONE as f64
    }

    /// Output frames made minus whole and fractional input frames the read
    /// position has passed: the time resampling added, in frames (negative
    /// when it took time out).
    pub fn stretch(&self) -> f64 {
        self.produced as f64 - self.passed as f64 - self.frac as f64 / ONE as f64
    }

    /// Input frames taken in that no output has reached yet: the latency.
    pub fn waiting(&self) -> f64 {
        (self.planes[0].len() - self.pos) as f64 - self.frac as f64 / ONE as f64
    }

    /// Takes interleaved `input` (a trailing partial frame is ignored) and
    /// appends to `out` every output frame it completes, interleaved, the
    /// step ramping from the latest call's to `step` (see the module docs);
    /// returns how many frames it appended.
    pub fn process(&mut self, input: &[f32], step: f64, out: &mut Vec<f32>) -> usize {
        let ch = self.channels();
        let input = &input[..input.len() / ch * ch];
        let before = self.planes[0].len();
        // Silence that ran up to the end goes on through new frames that
        // are silent too; a sound among them moves its start to the frame
        // after the last one.
        if let Some(i) = input.iter().rposition(|&v| finite(v) != 0.0) {
            self.silent_from = before + i / ch + 1;
            for (c, plane) in self.planes.iter_mut().enumerate() {
                plane.extend(input.iter().skip(c).step_by(ch).map(|&v| finite(v)));
            }
        } else {
            for plane in &mut self.planes {
                plane.resize(before + input.len() / ch, 0.0);
            }
        }
        let total = self.planes[0].len();
        let (from, to) = (self.step, fixed(step));
        // Outputs this call will make, for the ramp: the read distance left
        // over the mean step.
        let reach =
            (total as f64 - HALF_TAPS as f64 - self.pos as f64) * ONE as f64 - self.frac as f64;
        let expected = (reach / (0.5 * (from as f64 + to as f64))).max(1.0);
        // The step moves by `delta` per output until it reaches `to`, by at
        // least a unit so that it gets there.
        let delta = match ((to as f64 - from as f64) / expected).round() as i64 {
            0 => i64::from(to > from) - i64::from(to < from),
            d => d,
        };
        let table = table();
        let (mut made, mut silent) = (0usize, 0usize);
        out.reserve(((expected as usize) + 1) * ch);
        let mut s = from;
        while self.pos + HALF_TAPS < total {
            let first = self.pos + 1 - HALF_TAPS;
            if first >= self.silent_from {
                // Every frame the kernel reaches is silence, and so it is
                // for the rest of the call's outputs, which reach later
                // frames: they are counted and appended as zeros at the end,
                // the very values the sums below give for silence.
                silent += 1;
            } else {
                let p = (self.frac >> MIX_BITS) as usize;
                let f = (self.frac & MIX_MASK) as f32 * MIX_SCALE;
                let (a, b) = (row(table, p), row(table, p + 1));
                for plane in &self.planes {
                    // The kernel `f` of the way from row `a` to row `b`,
                    // applied as the same mix of the two rows' outputs.
                    let frames = taps(plane, first);
                    let (y0, y1) = (dot(a, frames), dot(b, frames));
                    out.push(y0 + f * (y1 - y0));
                }
            }
            made += 1;
            s = if delta >= 0 {
                s.saturating_add_signed(delta).min(to)
            } else {
                s.saturating_add_signed(delta).max(to)
            };
            let next = self.frac + s;
            let whole = (next >> FRAC_BITS) as usize;
            self.frac = next & (ONE - 1);
            self.pos += whole;
            self.passed += whole as u64;
        }
        out.resize(out.len() + silent * ch, 0.0);
        self.step = s;
        self.produced += made as u64;
        let drop = self.pos + 1 - HALF_TAPS;
        for plane in &mut self.planes {
            plane.drain(..drop);
        }
        self.pos -= drop;
        self.silent_from = self.silent_from.saturating_sub(drop);
        made
    }

    /// Ends the resampler, appending to `out`, interleaved and as they are,
    /// the input frames from the read position on (the next whole frame
    /// when it is between two): the stream passes on unresampled from
    /// there, moved by less than a frame. Returns how many frames it
    /// appended.
    pub fn release(self, out: &mut Vec<f32>) -> usize {
        let from = self.pos + usize::from(self.frac > 0);
        let total = self.planes[0].len();
        for i in from..total {
            out.extend(self.planes.iter().map(|plane| plane[i]));
        }
        total.saturating_sub(from)
    }
}

/// Row `p` of the kernel table.
fn row(table: &[f32], p: usize) -> &[f32; TAPS] {
    taps(table, p * TAPS)
}

/// The [`TAPS`] values of `values` from `first` on, as an array, so the
/// loops over them have no bounds to check.
fn taps(values: &[f32], first: usize) -> &[f32; TAPS] {
    values[first..first + TAPS]
        .try_into()
        .expect("a slice of TAPS values")
}

/// The dot product of a kernel row and the input frames under it, summed in
/// [`LANES`] partial sums and then pairwise across them: an order of
/// addition that is fixed (so deterministic) and that the compiler
/// vectorizes. Kept out of line, where it is vectorized whatever calls it.
#[inline(never)]
fn dot(row: &[f32; TAPS], frames: &[f32; TAPS]) -> f32 {
    let mut l = [0f32; LANES];
    for (k, x) in row.chunks_exact(LANES).zip(frames.chunks_exact(LANES)) {
        for i in 0..LANES {
            l[i] += k[i] * x[i];
        }
    }
    ((l[0] + l[4]) + (l[1] + l[5])) + ((l[2] + l[6]) + (l[3] + l[7]))
}

#[cfg(test)]
mod tests;
