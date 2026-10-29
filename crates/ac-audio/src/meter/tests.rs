//! The meter against EBU Tech 3341 style signals (generated, not read from
//! the EBU's files): steady 1 kHz sines whose loudness is known, signals
//! with silent or quiet parts for the gates, an inter-sample peak for the
//! true-peak meter, and every kind of input the meter must refuse or
//! survive.

use super::*;
use crate::signals::{segments, silence, sine};

/// Tech 3341's tolerance on a loudness reading, in LU.
const TOLERANCE: f64 = 0.1;

/// A stereo 48 kHz meter.
fn stereo() -> LoudnessMeter {
    LoudnessMeter::new(48_000, 2).unwrap()
}

/// The reading of a fresh `channels`-channel `rate` Hz meter after
/// `samples`.
fn measure(rate: u32, channels: u16, samples: &[f32]) -> LoudnessReading {
    let mut m = LoudnessMeter::new(rate, channels).unwrap();
    m.push(rate, channels, samples).unwrap();
    m.reading()
}

/// Asserts that `got` is a reading within `tol` of `want`.
fn near(what: &str, got: Option<f64>, want: f64, tol: f64) {
    let got = got.unwrap_or_else(|| panic!("{what}: no reading"));
    assert!((got - want).abs() <= tol, "{what}: {got} vs {want}");
}

#[test]
fn stereo_1khz_at_minus_23_dbfs_reads_minus_23_lufs() {
    // Tech 3341 test 1 (20 s there; a steady sine needs less).
    let r = measure(48_000, 2, &sine(48_000, 2, 1000.0, -23.0, 0.0, 10.0));
    near("momentary", r.momentary, -23.0, TOLERANCE);
    near("short-term", r.short_term, -23.0, TOLERANCE);
    near("integrated", r.integrated, -23.0, TOLERANCE);
    near("sample peak", r.sample_peak, -23.0, 0.01);
    near("true peak", r.true_peak, -23.0, 0.05);
    assert_eq!(r.measured, Duration::from_secs(10));
}

#[test]
fn stereo_1khz_at_minus_33_dbfs_reads_minus_33_lufs() {
    // Tech 3341 test 2.
    let r = measure(48_000, 2, &sine(48_000, 2, 1000.0, -33.0, 0.0, 10.0));
    near("momentary", r.momentary, -33.0, TOLERANCE);
    near("short-term", r.short_term, -33.0, TOLERANCE);
    near("integrated", r.integrated, -33.0, TOLERANCE);
}

#[test]
fn the_reading_does_not_depend_on_the_sample_rate() {
    // The filters are designed for each rate, not taken from the 48 kHz
    // table.
    for rate in [8_000, 11_025, 32_000, 44_100, 96_000, 192_000] {
        let r = measure(rate, 2, &sine(rate, 2, 1000.0, -23.0, 0.0, 4.0));
        near(&format!("{rate} Hz"), r.integrated, -23.0, TOLERANCE);
        near(&format!("{rate} Hz"), r.short_term, -23.0, TOLERANCE);
    }
}

#[test]
fn mono_reads_3_db_below_the_same_signal_on_a_stereo_pair() {
    let mono = measure(48_000, 1, &sine(48_000, 1, 1000.0, -23.0, 0.0, 4.0));
    near("mono", mono.integrated, -26.01, TOLERANCE);
    let stereo = measure(48_000, 2, &sine(48_000, 2, 1000.0, -23.0, 0.0, 4.0));
    let difference = stereo.integrated.unwrap() - mono.integrated.unwrap();
    assert!((difference - 3.0103).abs() < 0.001, "{difference}");
    // Peaks are per sample, whatever the channel count.
    assert_eq!(mono.sample_peak, stereo.sample_peak);
}

#[test]
fn one_silent_channel_halves_the_power() {
    // Left only: the right channel adds nothing, so 3 dB below both.
    let mut left_only = sine(48_000, 2, 1000.0, -23.0, 0.0, 4.0);
    for right in left_only.iter_mut().skip(1).step_by(2) {
        *right = 0.0;
    }
    let r = measure(48_000, 2, &left_only);
    near("integrated", r.integrated, -26.01, TOLERANCE);
}

#[test]
fn gating_ignores_a_silent_half() {
    // Half the signal is digital silence, below the absolute gate: the
    // integrated loudness is the sine's alone, whichever half is silent.
    for parts in [
        [(Some(-23.0), 20.0), (None, 20.0)],
        [(None, 20.0), (Some(-23.0), 20.0)],
    ] {
        let r = measure(48_000, 2, &segments(&parts));
        near("integrated", r.integrated, -23.0, TOLERANCE);
        near("sample peak", r.sample_peak, -23.0, 0.01);
        assert_eq!(r.measured, Duration::from_secs(40));
    }
    // An ungated mean over the same signal would read 3 dB low.
    let r = measure(48_000, 2, &segments(&[(Some(-23.0), 20.0), (None, 20.0)]));
    assert_eq!(r.momentary, None, "the last 400 ms are silent");
    assert_eq!(r.short_term, None, "the last 3 s are silent");
}

#[test]
fn the_relative_gate_drops_quiet_passages() {
    // Tech 3341 test 3: −36, −23, −36 dBFS for 10, 60, 10 s.
    let r = measure(
        48_000,
        2,
        &segments(&[
            (Some(-36.0), 10.0),
            (Some(-23.0), 60.0),
            (Some(-36.0), 10.0),
        ]),
    );
    near("test 3", r.integrated, -23.0, TOLERANCE);
    // Tech 3341 test 4: −72, −36, −23, −36, −72 dBFS for 10, 10, 60, 10,
    // 10 s; the −72 parts fall below the absolute gate as well.
    let r = measure(
        48_000,
        2,
        &segments(&[
            (Some(-72.0), 10.0),
            (Some(-36.0), 10.0),
            (Some(-23.0), 60.0),
            (Some(-36.0), 10.0),
            (Some(-72.0), 10.0),
        ]),
    );
    near("test 4", r.integrated, -23.0, TOLERANCE);
}

#[test]
fn the_windows_follow_the_signal() {
    // Tech 3341 tests 7 and 8 in spirit: after a step down, momentary
    // follows within 400 ms and short-term within 3 s, while the
    // integrated value keeps the louder part (−20 is within 10 LU).
    let mut m = stereo();
    m.push(
        48_000,
        2,
        &segments(&[(Some(-20.0), 5.0), (Some(-30.0), 0.4)]),
    )
    .unwrap();
    near("momentary", m.reading().momentary, -30.0, TOLERANCE);
    m.push(48_000, 2, &segments(&[(Some(-30.0), 2.6)])).unwrap();
    let r = m.reading();
    near("short-term", r.short_term, -30.0, TOLERANCE);
    let want = crate::loudness((5.0 * crate::power_at(-20.0) + 3.0 * crate::power_at(-30.0)) / 8.0)
        .unwrap();
    near("integrated", r.integrated, want, TOLERANCE);
}

#[test]
fn true_peak_catches_an_inter_sample_peak_that_sample_peak_misses() {
    // A sine at a quarter of the rate, 45° off its crests: every sample is
    // 3 dB below the crest (−6.02 dBFS), the meter's true peak is not.
    let r = measure(48_000, 2, &sine(48_000, 2, 12_000.0, -6.0206, 45.0, 1.0));
    near("sample peak", r.sample_peak, -9.0309, 0.01);
    let tp = r.true_peak.unwrap();
    // Tech 3341's true-peak tolerance: +0.2 / −0.4 dB.
    assert!((-6.0206 - 0.4..=-6.0206 + 0.2).contains(&tp), "{tp}");
    assert!(tp > r.sample_peak.unwrap() + 2.5);
}

#[test]
fn true_peak_is_never_below_sample_peak() {
    // Crests on the samples: the interpolated points fall below them, but
    // for the onset's overshoot, so the true peak is the sample peak or a
    // little above.
    let r = measure(48_000, 1, &sine(48_000, 1, 12_000.0, -6.0206, 90.0, 1.0));
    let (tp, sp) = (r.true_peak.unwrap(), r.sample_peak.unwrap());
    assert!((0.0..0.2).contains(&(tp - sp)), "{tp} vs {sp}");
    // A full-scale square wave overshoots between its samples, by about
    // 2 dB: what a DAC would rebuild from it.
    let square: Vec<f32> = (0..48_000)
        .map(|n| if n % 48 < 24 { 1.0 } else { -1.0 })
        .collect();
    let r = measure(48_000, 1, &square);
    assert_eq!(r.sample_peak, Some(0.0));
    assert!(r.true_peak.unwrap() > 1.5, "{:?}", r.true_peak);
}

#[test]
fn silence_reads_no_value_rather_than_minus_infinity() {
    let fresh = stereo().reading();
    assert_eq!(fresh, LoudnessReading::default());
    let mut m = stereo();
    m.push_silence(48_000 * 5);
    m.push(48_000, 2, &silence(48_000, 2, 1.0)).unwrap();
    let r = m.reading();
    assert_eq!(
        r,
        LoudnessReading {
            measured: Duration::from_secs(6),
            ..LoudnessReading::default()
        }
    );
    // After audio, silence clears the windows (the filters come to rest)
    // but not the integrated value or the peaks.
    m.push(48_000, 2, &sine(48_000, 2, 1000.0, -23.0, 0.0, 20.0))
        .unwrap();
    m.push_silence(48_000 * 4);
    let r = m.reading();
    assert_eq!((r.momentary, r.short_term), (None, None));
    near("integrated", r.integrated, -23.0, TOLERANCE);
    assert!(r.true_peak.is_some_and(f64::is_finite));
}

#[test]
fn silence_without_a_buffer_reads_as_zeros_do() {
    let tone = sine(48_000, 2, 440.0, -18.0, 0.0, 1.3);
    let mut pushed = stereo();
    let mut inserted = stereo();
    for m in [&mut pushed, &mut inserted] {
        m.push(48_000, 2, &tone).unwrap();
    }
    pushed.push(48_000, 2, &silence(48_000, 2, 2.7)).unwrap();
    inserted.push_silence(48_000 * 27 / 10);
    for m in [&mut pushed, &mut inserted] {
        m.push(48_000, 2, &tone).unwrap();
    }
    assert_eq!(pushed, inserted);
}

#[test]
fn how_the_audio_is_cut_into_blocks_does_not_matter() {
    let signal = segments(&[(Some(-20.0), 2.0), (None, 0.5), (Some(-30.0), 1.7)]);
    let whole = measure(48_000, 2, &signal);
    for frames in [1, 441, 480, 4_800, 7_777] {
        let mut m = stereo();
        for block in signal.chunks(frames * 2) {
            m.push(48_000, 2, block).unwrap();
        }
        assert_eq!(m.reading(), whole, "{frames}-frame blocks");
    }
}

#[test]
fn readings_appear_when_their_window_has_filled() {
    let mut m = stereo();
    let tone = sine(48_000, 2, 1000.0, -23.0, 0.0, 3.0);
    // 400 ms is 19 200 frames: one short, nothing yet.
    m.push(48_000, 2, &tone[..2 * 19_199]).unwrap();
    assert_eq!(m.reading().momentary, None);
    assert_eq!(m.reading().integrated, None);
    m.push(48_000, 2, &tone[2 * 19_199..2 * 19_200]).unwrap();
    near("momentary", m.reading().momentary, -23.0, TOLERANCE);
    near("integrated", m.reading().integrated, -23.0, TOLERANCE);
    assert_eq!(m.reading().short_term, None);
    m.push(48_000, 2, &tone[2 * 19_200..]).unwrap();
    near("short-term", m.reading().short_term, -23.0, TOLERANCE);
    // Peaks need no window.
    let mut m = stereo();
    m.push(48_000, 2, &[0.5, -0.25]).unwrap();
    near("sample peak", m.reading().sample_peak, -6.0206, 1e-4);
}

#[test]
fn hops_average_100_ms_where_a_tenth_of_the_rate_is_not_whole() {
    // At 11 025 Hz the first four hops end after ⌊4 · 1102.5⌋ = 4 410
    // frames.
    let mut m = LoudnessMeter::new(11_025, 1).unwrap();
    let tone = sine(11_025, 1, 1000.0, -20.0, 0.0, 1.0);
    m.push(11_025, 1, &tone[..4_409]).unwrap();
    assert_eq!(m.reading().momentary, None);
    m.push(11_025, 1, &tone[4_409..4_410]).unwrap();
    assert!(m.reading().momentary.is_some());
    assert_eq!((0..10).map(|k| hop_length(k, 11_025)).sum::<u64>(), 11_025);
    assert_eq!(hop_length(0, 48_000), 4_800);
    m.push(11_025, 1, &tone[4_410..]).unwrap();
    assert_eq!(m.reading().measured, Duration::from_secs(1));
}

#[test]
fn invalid_formats_are_refused() {
    assert_eq!(
        LoudnessMeter::new(0, 2).unwrap_err(),
        MeterError::SampleRate(0)
    );
    assert_eq!(
        LoudnessMeter::new(MIN_SAMPLE_RATE - 1, 2).unwrap_err(),
        MeterError::SampleRate(MIN_SAMPLE_RATE - 1)
    );
    assert_eq!(
        LoudnessMeter::new(MAX_SAMPLE_RATE + 1, 2).unwrap_err(),
        MeterError::SampleRate(MAX_SAMPLE_RATE + 1)
    );
    assert_eq!(
        LoudnessMeter::new(48_000, 0).unwrap_err(),
        MeterError::Channels(0)
    );
    assert_eq!(
        LoudnessMeter::new(48_000, MAX_CHANNELS + 1).unwrap_err(),
        MeterError::Channels(MAX_CHANNELS + 1)
    );
    let m = LoudnessMeter::new(MAX_SAMPLE_RATE, MAX_CHANNELS).unwrap();
    assert_eq!(
        (m.sample_rate(), m.channels()),
        (MAX_SAMPLE_RATE, MAX_CHANNELS)
    );
    let err = LoudnessMeter::new(0, 2).unwrap_err().to_string();
    assert!(err.contains("8000..=384000"), "{err}");
}

#[test]
fn mismatched_input_is_refused_whole() {
    let mut m = stereo();
    let tone = sine(48_000, 2, 1000.0, -23.0, 0.0, 0.5);
    assert_eq!(
        m.push(48_000, 1, &tone),
        Err(MeterError::ChannelMismatch {
            expected: 2,
            actual: 1
        })
    );
    assert_eq!(
        m.push(44_100, 2, &tone),
        Err(MeterError::RateMismatch {
            expected: 48_000,
            actual: 44_100
        })
    );
    assert_eq!(
        m.push(48_000, 2, &tone[..tone.len() - 1]),
        Err(MeterError::PartialFrame {
            len: tone.len() - 1,
            channels: 2
        })
    );
    assert_eq!(m, stereo(), "nothing was measured");
    assert!(m.push(48_000, 2, &[]).is_ok());
    assert_eq!(m, stereo());
}

#[test]
fn non_finite_samples_are_measured_as_silence() {
    let clean = sine(48_000, 2, 1000.0, -23.0, 0.0, 4.0);
    let mut dirty = clean.clone();
    let mut zeroed = clean.clone();
    for (i, bad) in [
        (1_000, f32::NAN),
        (50_001, f32::INFINITY),
        (90_000, f32::NEG_INFINITY),
    ] {
        dirty[i] = bad;
        zeroed[i] = 0.0;
    }
    let r = measure(48_000, 2, &dirty);
    assert_eq!(r, measure(48_000, 2, &zeroed));
    near("integrated", r.integrated, -23.0, TOLERANCE);
}

#[test]
fn input_beyond_full_scale_reads_above_zero_and_stays_finite() {
    let r = measure(48_000, 2, &sine(48_000, 2, 1000.0, 6.0, 0.0, 1.0));
    near("integrated", r.integrated, 6.0, TOLERANCE);
    near("sample peak", r.sample_peak, 6.0, 0.01);
    // Near the largest f32, far above the gating histogram's top bin.
    let r = measure(48_000, 2, &sine(48_000, 2, 1000.0, 760.0, 0.0, 1.0));
    for value in [r.momentary, r.integrated, r.sample_peak, r.true_peak] {
        assert!(value.is_some_and(f64::is_finite), "{r:?}");
    }
    near("integrated", r.integrated, 760.0, TOLERANCE);
}

#[test]
fn reset_starts_over() {
    let mut m = stereo();
    m.push(48_000, 2, &sine(48_000, 2, 1000.0, -23.0, 0.0, 3.3))
        .unwrap();
    m.reset();
    assert_eq!(m, stereo());
    assert_eq!(m.reading(), LoudnessReading::default());
}
