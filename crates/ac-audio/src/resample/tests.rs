//! Tests of the band-limited resampler: the kernel and its table, phase
//! zero as the identity, a stream carried on from its history, accuracy on
//! sines against the signal they sample, the books, ramps, limits and bad
//! input.

use std::f64::consts::PI;

use super::*;

/// Sample rate of the tests.
const RATE: f64 = 48_000.0;

/// Amplitude of the test tones.
const AMPLITUDE: f64 = 0.5;

/// Channel `channel` of a `freq` Hz tone at `t` input sample periods: a
/// sine of [`AMPLITUDE`], a quarter cycle further on each channel.
fn tone_at(freq: f64, channel: usize, t: f64) -> f64 {
    AMPLITUDE * (2.0 * PI * freq * t / RATE + channel as f64 * PI / 2.0).sin()
}

/// Frames `first..first + frames` of the stereo `freq` Hz tone, interleaved.
fn tone(freq: f64, first: i64, frames: usize) -> Vec<f32> {
    (first..first + frames as i64)
        .flat_map(|n| (0..2).map(move |c| tone_at(freq, c, n as f64) as f32))
        .collect()
}

/// `frames` frames of `channels` channels of a fixed pseudo-random signal
/// in `-1.0..1.0` (never zero), interleaved.
fn noise(channels: usize, frames: usize) -> Vec<f32> {
    let mut state = 0x2545_f491_u32;
    (0..channels * frames)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (f64::from(state) / f64::from(u32::MAX) * 2.0 - 1.0) as f32 + 1e-3
        })
        .collect()
}

/// The bits of `samples`, to compare exactly.
fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|v| v.to_bits()).collect()
}

#[test]
fn the_kernel_is_one_at_zero_and_zero_at_other_whole_numbers() {
    assert_eq!(kernel(0.0), 1.0);
    for k in 1..=HALF_TAPS + 4 {
        let k = k as f64;
        assert_eq!((kernel(k), kernel(-k)), (0.0, 0.0), "at {k}");
    }
    for x in [0.25, 0.5, 3.7, 20.1] {
        assert_eq!(kernel(x), kernel(-x), "symmetric at {x}");
        assert!(kernel(x).abs() < 1.0);
    }
    assert!(kernel(0.5) > 0.6, "{}", kernel(0.5));
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(kernel(bad), 0.0);
    }
}

#[test]
fn every_table_row_has_unit_gain_and_row_zero_is_an_impulse() {
    let table = table();
    assert_eq!(table.len(), (PHASES + 1) * TAPS);
    for (p, row) in table.chunks_exact(TAPS).enumerate() {
        let gain: f64 = row.iter().map(|&h| f64::from(h)).sum();
        assert!((gain - 1.0).abs() < 1e-6, "row {p}: gain {gain}");
    }
    let impulse = |at: usize| (0..TAPS).map(move |t| f32::from(u8::from(t == at)));
    let first = &table[..TAPS];
    let last = &table[PHASES * TAPS..];
    assert!(first.iter().copied().eq(impulse(HALF_TAPS - 1)));
    assert!(last.iter().copied().eq(impulse(HALF_TAPS)));
    // Halfway between two samples the kernel is symmetric about them.
    let half = &table[PHASES / 2 * TAPS..][..TAPS];
    for t in 0..TAPS {
        assert_eq!(half[t], half[TAPS - 1 - t], "tap {t}");
    }
}

#[test]
fn at_step_one_the_output_is_the_input_bit_for_bit() {
    let input = noise(2, 1_000);
    let mut r = Resampler::with_history(2, &[], 1.0);
    let mut out = Vec::new();
    let made = r.process(&input, 1.0, &mut out);
    // The last HALF_TAPS frames wait for the frames the kernel reaches.
    assert_eq!(made, 1_000 - HALF_TAPS);
    assert_eq!(r.waiting(), HALF_TAPS as f64);
    assert_eq!(bits(&out), bits(&input[..made * 2]));
    // The next call carries on where this one stopped.
    let more = noise(2, 3_000);
    let made = r.process(&more[2_000..], 1.0, &mut out);
    assert_eq!(made, 2_000);
    let all: Vec<f32> = input.iter().chain(&more[2_000..]).copied().collect();
    assert_eq!(bits(&out), bits(&all[..out.len()]));
    assert_eq!(r.stretch(), 0.0);
}

#[test]
fn a_resampler_started_from_history_carries_the_stream_on() {
    // Passed through up to frame 500, then resampled at step one: not a
    // sample differs from passing the whole stream through.
    let signal = noise(2, 1_000);
    let (before, after) = signal.split_at(1_000);
    let mut r = Resampler::with_history(2, before, 1.0);
    let mut out = Vec::new();
    r.process(after, 1.0, &mut out);
    assert_eq!(bits(&out), bits(&after[..out.len()]));
    // At another step the first frame is still the next input frame, and
    // the ones after it move away from the input only gradually.
    let mut r = Resampler::with_history(2, before, 1.0);
    let mut out = Vec::new();
    r.process(after, 1.001, &mut out);
    assert_eq!(bits(&out[..2]), bits(&after[..2]));
}

#[test]
fn sines_come_out_as_the_signal_they_sample() {
    // Measured against the tone itself at each output's instant, over every
    // phase the instants pass through: within −100 dB of full scale up to
    // 20 kHz at 48 kHz, for corrections of 100 ppm and of 0.3 % either way.
    let limit = 10f64.powf(-100.0 / 20.0);
    for freq in [50.0, 1_000.0, 7_000.0, 15_000.0, 20_000.0] {
        for step in [1.0 + 1e-4, 1.0 - 1e-4, 1.003, 0.997] {
            let history = tone(freq, -(HALF_TAPS as i64), HALF_TAPS);
            let mut r = Resampler::with_history(2, &history, step);
            let mut out = Vec::new();
            r.process(&tone(freq, 0, 9_600), step, &mut out);
            let mut worst = 0f64;
            for (i, frame) in out.chunks_exact(2).enumerate() {
                let t = i as f64 * step;
                for (c, &v) in frame.iter().enumerate() {
                    worst = worst.max((f64::from(v) - tone_at(freq, c, t)).abs());
                }
            }
            assert!(
                worst < limit,
                "{freq} Hz at step {step}: error {:.1} dB",
                20.0 * worst.log10()
            );
        }
    }
}

#[test]
fn the_books_balance_over_a_long_run() {
    // Two minutes at 100 ppm fast: one frame in ten thousand taken out,
    // blocks of sound and of silence in turn.
    let step = 1.0 + 1e-4;
    let (sound, silence) = (noise(2, 480), vec![0.0; 960]);
    let mut r = Resampler::with_history(2, &[], step);
    let (mut made, mut out) = (0u64, Vec::new());
    let blocks = 12_000u64;
    for k in 0..blocks {
        out.clear();
        let block = if k % 3 == 2 { &silence } else { &sound };
        made += r.process(block, step, &mut out) as u64;
        assert_eq!(out.len() % 2, 0);
    }
    let fed = (blocks * 480) as f64;
    let passed = fed - r.waiting();
    assert!((r.stretch() - (made as f64 - passed)).abs() < 1e-6);
    assert!(
        (made as f64 - passed / step).abs() <= 1.0,
        "{made} for {passed}"
    );
    assert!(
        (r.stretch() + passed * (1.0 - 1.0 / step)).abs() <= 1.0,
        "{}",
        r.stretch()
    );
    // What waits stays bounded by the kernel's reach.
    assert!(r.waiting() < (HALF_TAPS + 2) as f64, "{}", r.waiting());
}

#[test]
fn the_step_ramps_across_a_call_and_never_jumps() {
    let mut r = Resampler::with_history(1, &[], 1.0);
    let mut out = Vec::new();
    r.process(&[0.25; 4_800], 1.001, &mut out);
    assert!((r.step() - 1.001).abs() < 1e-6, "{}", r.step());
    // Ramping from 1 to 1.001 takes out about half what 1.001 would.
    let steady = 4_800.0 * (1.0 - 1.0 / 1.001);
    assert!(
        (-r.stretch() - steady / 2.0).abs() < 0.5,
        "stretch {} against {steady} at the full step",
        r.stretch()
    );
    // A call with nothing to make keeps the step.
    let before = r.step();
    r.process(&[], 0.999, &mut out);
    assert_eq!(r.step(), before);
}

#[test]
fn steps_beyond_the_limit_are_held_and_a_nan_step_is_one() {
    let mut r = Resampler::with_history(1, &[], 2.0);
    assert!((r.step() - (1.0 + MAX_STEP_DEVIATION)).abs() < 1e-12);
    let mut out = Vec::new();
    r.process(&[0.0; 4_800], 0.1, &mut out);
    assert!((r.step() - (1.0 - MAX_STEP_DEVIATION)).abs() < 1e-5);
    r.process(&[0.0; 48_000], f64::NAN, &mut out);
    assert!((r.step() - 1.0).abs() < 1e-6, "{}", r.step());
    assert_eq!(Resampler::with_history(1, &[], f64::INFINITY).step(), 1.0);
}

#[test]
fn a_non_finite_sample_is_taken_as_silence() {
    let mut clean = noise(2, 600);
    let mut dirty = clean.clone();
    for (i, bad) in [
        (101, f32::NAN),
        (250, f32::INFINITY),
        (400, f32::NEG_INFINITY),
    ] {
        dirty[i] = bad;
        clean[i] = 0.0;
    }
    let run = |input: &[f32]| {
        let mut r = Resampler::with_history(2, &[f32::NAN; 4], 1.0003);
        let mut out = Vec::new();
        r.process(input, 1.0003, &mut out);
        out
    };
    let (a, b) = (run(&dirty), run(&clean));
    assert!(a.iter().all(|v| v.is_finite()));
    assert_eq!(bits(&a), bits(&b));
}

#[test]
fn partial_frames_and_empty_input_make_nothing_more() {
    let mut r = Resampler::with_history(2, &[0.5; 5], 1.0);
    let mut out = Vec::new();
    assert_eq!(r.process(&[], 1.0, &mut out), 0);
    // Three samples of a stereo stream: one frame, the half frame left.
    r.process(&[0.1, 0.2, 0.3], 1.0, &mut out);
    assert!(out.is_empty());
    assert_eq!(r.waiting(), 1.0);
    // Channels are held to 1..=MAX_CHANNELS.
    assert_eq!(Resampler::with_history(0, &[], 1.0).channels(), 1);
    assert_eq!(
        Resampler::with_history(64, &[], 1.0).channels(),
        MAX_CHANNELS
    );
}

#[test]
fn release_lets_the_waiting_frames_go_as_they_are() {
    let input = noise(2, 200);
    let mut r = Resampler::with_history(2, &[], 1.0);
    let mut out = Vec::new();
    let made = r.process(&input, 1.0, &mut out);
    let released = r.release(&mut out);
    assert_eq!(made + released, 200);
    assert_eq!(bits(&out), bits(&input));
    // Between two frames, the next whole frame is the first let go.
    let mut r = Resampler::with_history(2, &[], 1.0 + 1e-3);
    let mut out = Vec::new();
    r.process(&input, 1.0 + 1e-3, &mut out);
    let waiting = r.waiting();
    let mut rest = Vec::new();
    let released = r.release(&mut rest);
    assert_eq!(released, waiting.floor() as usize);
    assert_eq!(bits(&rest), bits(&input[(200 - released) * 2..]));
}

#[test]
fn channels_are_kept_apart() {
    // Eight channels, each a different constant: every output keeps each
    // channel's own value, at any phase.
    let frame: Vec<f32> = (0..8).map(|c| c as f32 / 8.0 - 0.4).collect();
    let input: Vec<f32> = frame.iter().copied().cycle().take(8 * 2_000).collect();
    let mut r = Resampler::with_history(8, &input[..8 * 30], 0.9993);
    let mut out = Vec::new();
    r.process(&input, 0.9993, &mut out);
    assert!(!out.is_empty());
    for (i, got) in out.chunks_exact(8).enumerate() {
        for (c, (&got, &want)) in got.iter().zip(&frame).enumerate() {
            assert!((got - want).abs() < 1e-6, "frame {i} channel {c}: {got}");
        }
    }
}

#[test]
fn the_same_input_cut_differently_gives_the_same_output() {
    let input = noise(2, 4_000);
    let step = 1.0 - 2.5e-4;
    let whole = {
        let mut r = Resampler::with_history(2, &[], step);
        let mut out = Vec::new();
        r.process(&input, step, &mut out);
        out
    };
    let mut r = Resampler::with_history(2, &[], step);
    let mut pieces = Vec::new();
    for chunk in input.chunks(2 * 37) {
        r.process(chunk, step, &mut pieces);
    }
    assert_eq!(bits(&whole), bits(&pieces));
}

#[test]
fn an_impulse_comes_out_as_the_kernel_and_silence_as_zeros() {
    // Silence, one impulse at frame 1_000, silence, fed in uneven pieces:
    // the outputs whose kernel reaches the impulse are the kernel, offset
    // by their instant, and every other output is exactly zero (positive,
    // as the sums give), however the silence fell across calls.
    let step = 1.0 + 3.7e-3;
    let mut input = vec![0.0f32; 2_000];
    input[1_000] = 1.0;
    let mut r = Resampler::with_history(1, &[], step);
    let mut out = Vec::new();
    for chunk in input.chunks(37) {
        r.process(chunk, step, &mut out);
    }
    let mut t = 0f64;
    let mut reached = 0;
    for (k, &y) in out.iter().enumerate() {
        let offset = 1_000.0 - t;
        if offset.abs() < HALF_TAPS as f64 {
            reached += 1;
            let want = kernel(offset);
            assert!(
                (f64::from(y) - want).abs() < 1e-4,
                "output {k}: {y} for {want}"
            );
        } else {
            assert_eq!(y.to_bits(), 0, "output {k} at {t}");
        }
        t += step;
    }
    assert!(
        (2 * HALF_TAPS - 2..=2 * HALF_TAPS).contains(&reached),
        "{reached}"
    );
    // Silence after a history of sound: the outputs that still reach the
    // history are worked out, not zeroed (the first is at phase zero, the
    // first input frame itself), and those past it are zeros.
    let history = noise(1, HALF_TAPS);
    let mut r = Resampler::with_history(1, &history, 1.0 + 1e-3);
    let mut out = Vec::new();
    r.process(&[0.0; 200], 1.0 + 1e-3, &mut out);
    assert_eq!(out[0].to_bits(), 0);
    assert!(out[1..HALF_TAPS - 2].iter().all(|&y| y != 0.0), "{out:?}");
    assert!(out[HALF_TAPS - 1..].iter().all(|&y| y.to_bits() == 0));
}
