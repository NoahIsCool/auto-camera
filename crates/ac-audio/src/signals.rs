//! Test signals in the manner of EBU Tech 3341's test set: steady sines at
//! a stated level, and silence, as interleaved `f32`.
//!
//! A sine's level is its crest in dBFS (Tech 3341's convention: a sine
//! "at −23 dBFS" peaks at 10^(−23/20)), the same on every channel and in
//! phase. Samples are computed in `f64` from the sample index, so a long
//! signal does not drift.

/// `seconds` of a `freq` Hz sine at `dbfs` (its crest), starting `phase`
/// degrees into its cycle, on all `channels` of `rate` Hz audio.
pub fn sine(rate: u32, channels: u16, freq: f64, dbfs: f64, phase: f64, seconds: f64) -> Vec<f32> {
    let amplitude = 10f64.powf(dbfs / 20.0);
    let frames = (seconds * f64::from(rate)).round() as usize;
    let step = 2.0 * std::f64::consts::PI * freq / f64::from(rate);
    let start = phase.to_radians();
    let mut out = Vec::with_capacity(frames * usize::from(channels));
    for n in 0..frames {
        let x = (amplitude * (start + step * n as f64).sin()) as f32;
        out.extend(std::iter::repeat_n(x, usize::from(channels)));
    }
    out
}

/// `seconds` of digital silence on `channels` channels of `rate` Hz audio.
pub fn silence(rate: u32, channels: u16, seconds: f64) -> Vec<f32> {
    let frames = (seconds * f64::from(rate)).round() as usize;
    vec![0.0; frames * usize::from(channels)]
}

/// Segments of 1 kHz stereo sine at 48 kHz, each `(dbfs, seconds)`, one
/// after another; a level of `None` is silence.
pub fn segments(parts: &[(Option<f64>, f64)]) -> Vec<f32> {
    parts
        .iter()
        .flat_map(|&(level, seconds)| match level {
            Some(dbfs) => sine(48_000, 2, 1000.0, dbfs, 0.0, seconds),
            None => silence(48_000, 2, seconds),
        })
        .collect()
}
