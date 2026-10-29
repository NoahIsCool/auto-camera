//! Tests of the drift estimator: an input on the master clock, fast and
//! slow clocks, exact and jittery PTS and irregular blocks, the timing
//! reference, steps in the PTS taken for steps (confirmed a few blocks on,
//! before the reference too) or breaks and never for drift, timestamps
//! that stray and come back held and left out, and settings it refuses.

use super::*;

/// Sample rate of the tests.
const RATE: u32 = 48_000;

/// Frames per block (10 ms).
const BLOCK: u64 = 480;

/// The PTS of frame `frame` of an input whose clock runs `ppm` fast, read
/// on the master clock and rounded down to the nanosecond, as a device
/// that stamps capture times would; `jitter` (seconds, either way) is
/// added.
fn pts(frame: u64, ppm: f64, jitter: f64) -> Duration {
    let t = frame as f64 / (f64::from(RATE) * (1.0 + ppm * 1e-6)) + jitter;
    Duration::from_nanos((t.max(0.0) * 1e9) as u64)
}

/// A fixed sequence of jitter in `-amount..amount` seconds.
fn jitter(amount: f64) -> impl FnMut() -> f64 {
    let mut state = 0x9e37_79b9_u32;
    move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (f64::from(state) / f64::from(u32::MAX) * 2.0 - 1.0) * amount
    }
}

/// An estimator with the default settings.
fn estimator() -> DriftEstimator {
    DriftEstimator::new(RATE, &DriftSettings::default()).unwrap()
}

/// Feeds `seconds` of 10 ms blocks of an input `ppm` fast, with PTS
/// jitter of up to `amount` seconds, from frame `first`; returns the frame
/// after the last.
fn feed(e: &mut DriftEstimator, first: u64, seconds: u64, ppm: f64, amount: f64) -> u64 {
    let mut noise = jitter(amount);
    let blocks = seconds * u64::from(RATE) / BLOCK;
    for k in 0..blocks {
        let frame = first + k * BLOCK;
        let seen = e.observe(pts(frame, ppm, noise()), BLOCK);
        assert_ne!(seen, Observation::Discontinuity, "at frame {frame}");
    }
    first + blocks * BLOCK
}

#[test]
fn an_input_on_the_master_clock_measures_no_drift() {
    // PTS counted from the samples, as a file played on the engine's clock
    // stamps them: an hour without a part per billion of drift.
    let mut e = estimator();
    assert_eq!(e.observe(Duration::ZERO, BLOCK), Observation::Start);
    let mut frame = BLOCK;
    for _ in 0..360_000 {
        let at = Duration::from_nanos(frame * 1_000_000_000 / u64::from(RATE));
        assert_eq!(e.observe(at, BLOCK), Observation::Tracked);
        frame += BLOCK;
    }
    assert!(e.drift_ppm().abs() < 1e-3, "{}", e.drift_ppm());
    assert!(e.error().abs() < 1e-8, "{}", e.error());
    assert_eq!(e.resets(), 0);
    assert!((e.epoch_seconds() - 3_600.01).abs() < 1e-9);
}

#[test]
fn fast_and_slow_clocks_measure_their_drift() {
    for ppm in [100.0, -100.0, 20.0, -500.0] {
        let mut e = estimator();
        feed(&mut e, 0, 300, ppm, 0.0);
        assert!(
            (e.drift_ppm() - ppm).abs() < 0.05,
            "{ppm} ppm read as {}",
            e.drift_ppm()
        );
        // The timing error is what the drift built up since the reference,
        // a second in: ppm × elapsed.
        let built = ppm * 1e-6 * 299.0;
        assert!(
            (e.error() - built).abs() < 2e-4,
            "{} for {built}",
            e.error()
        );
    }
}

#[test]
fn jitter_in_the_pts_is_filtered_out() {
    // ±2 ms on every block, against a drift of a tenth of a millisecond per
    // second: the estimate stays within a couple of ppm, and nothing is
    // taken for a break.
    let mut e = estimator();
    let mut frame = feed(&mut e, 0, 120, 100.0, 0.002);
    let mut worst = 0f64;
    for _ in 0..30 {
        frame = feed(&mut e, frame, 10, 100.0, 0.002);
        worst = worst.max((e.drift_ppm() - 100.0).abs());
    }
    assert!(worst < 2.0, "off by {worst} ppm");
    assert_eq!((e.resets(), e.steps()), (0, 0));
}

#[test]
fn blocks_of_any_length_are_weighed_by_the_time_they_cover() {
    // A driver that hands over 256, 512 and 441 frames in turn measures the
    // same drift as one of steady 10 ms blocks.
    let mut e = estimator();
    let mut frame = 0;
    for k in 0..30_000 {
        let n = [256, 512, 441][k % 3];
        e.observe(pts(frame, -60.0, 0.0), n);
        frame += n;
    }
    assert!((e.drift_ppm() + 60.0).abs() < 0.05, "{}", e.drift_ppm());
}

#[test]
fn a_step_in_the_pts_is_a_break_not_drift() {
    for step in [0.5, -0.5, 2.0] {
        let mut e = estimator();
        let frame = feed(&mut e, 0, 60, 0.0, 0.0);
        // A reconnection half a second later (or a PTS that went back): a
        // new epoch, and no drift read into it.
        let at = pts(frame, 0.0, 0.0).as_secs_f64() + step;
        let seen = e.observe(Duration::from_secs_f64(at), BLOCK);
        assert_eq!(seen, Observation::Discontinuity, "step {step}");
        assert_eq!(e.resets(), 1);
        assert_eq!((e.drift_ppm(), e.error()), (0.0, 0.0));
        for k in 1..6_000 {
            let t = at + (k * BLOCK) as f64 / f64::from(RATE);
            assert_eq!(
                e.observe(Duration::from_secs_f64(t), BLOCK),
                Observation::Tracked
            );
        }
        assert!(e.drift_ppm().abs() < 0.01, "{}", e.drift_ppm());
        assert_eq!(e.resets(), 1);
    }
}

#[test]
fn a_step_below_the_threshold_is_timing_error() {
    // 40 ms of audio lost from a drift-free input: not a break but a step,
    // all of it timing error and none of it drift. The blocks after the
    // loss are held out of the fit until enough of them agree, and the
    // step is told at the last of those.
    let mut e = estimator();
    let frame = feed(&mut e, 0, 60, 0.0, 0.0);
    let lost = frame + 4 * 480;
    let confirmed = CONFIRM_BLOCKS - 1;
    for k in 0..2_000 {
        let f = lost + k * BLOCK;
        let seen = e.observe(pts(f, 0.0, 0.0), BLOCK);
        let error = if k < confirmed {
            assert_eq!(seen, Observation::Held, "block {k}");
            0.0
        } else if k == confirmed {
            let Observation::Step(by) = seen else {
                panic!("block {k}: {seen:?}");
            };
            assert!((by + 0.04).abs() < 1e-6, "{by}");
            -0.04
        } else {
            assert_eq!(seen, Observation::Tracked, "block {k}");
            -0.04
        };
        assert!(e.drift_ppm().abs() < 0.01, "block {k}: {}", e.drift_ppm());
        assert!((e.error() - error).abs() < 1e-6, "block {k}: {}", e.error());
    }
    assert_eq!((e.resets(), e.steps()), (0, 1));
}

#[test]
fn steps_through_jitter_move_the_timing_error_and_not_the_drift() {
    // ±0.5 ms of jitter on a clock drift-free or 100 ppm fast, and a step
    // of 20 or 50 ms forward (audio lost) or 30 ms back at a minute: one
    // step each, told a few blocks on, the timing error moved by it, the
    // drift as it was.
    for ppm in [0.0, 100.0] {
        for step in [-0.02, -0.05, 0.03] {
            let mut e = estimator();
            let frame = feed(&mut e, 0, 60, ppm, 0.000_5);
            let before = e.error();
            let mut noise = jitter(0.000_5);
            let (mut worst, mut seen_step, mut held) = (0f64, None, 0);
            for k in 0..3_000 {
                let f = frame + k * BLOCK;
                let at = pts(f, ppm, noise()).as_secs_f64() - step;
                match e.observe(Duration::from_secs_f64(at), BLOCK) {
                    Observation::Step(by) => seen_step = Some(by),
                    Observation::Held => held += 1,
                    seen => assert_eq!(seen, Observation::Tracked, "{ppm}, {step}: {k}"),
                }
                worst = worst.max((e.drift_ppm() - ppm).abs());
            }
            let by = seen_step.expect("a step");
            assert!((by - step).abs() < 0.0015, "{ppm}, {step}: {by}");
            assert_eq!(held, CONFIRM_BLOCKS - 1, "{ppm}, {step}");
            assert!(worst < 2.0, "{ppm}, {step}: off by {worst} ppm");
            // What the drift built up over 30 s, and the step.
            let moved = e.error() - before - ppm * 1e-6 * 30.0;
            assert!((moved - step).abs() < 0.001, "{ppm}, {step}: {moved}");
            assert_eq!((e.resets(), e.steps()), (0, 1), "{ppm}, {step}");
        }
    }
}

/// Feeds an estimator and a twin `seconds` of `block`-frame blocks of an
/// input `ppm` fast with PTS jitter of up to 20 µs (a CoreAudio host
/// time's), from frame `first`: the twin as they are, the estimator with
/// the PTS of the `k`th block from the start moved by `off(k)` seconds
/// (below zero earlier); returns what the estimator made of each block and
/// the frame after the last.
fn twins(
    (e, twin): (&mut DriftEstimator, &mut DriftEstimator),
    (first, block, seconds, ppm): (u64, u64, f64, f64),
    off: impl Fn(u64) -> f64,
) -> (Vec<Observation>, u64) {
    let mut noise = jitter(0.000_02);
    let blocks = (seconds * f64::from(RATE) / block as f64) as u64;
    let mut seen = Vec::new();
    for k in 0..blocks {
        let (frame, j) = (first + k * block, noise());
        seen.push(e.observe(pts(frame, ppm, j + off(k)), block));
        twin.observe(pts(frame, ppm, j), block);
    }
    (seen, first + blocks * block)
}

/// What the estimator made of the blocks after `seen`'s first `n`, all
/// tracked or not.
fn tracked_after(seen: &[Observation], n: usize) -> bool {
    seen[n..].iter().all(|&o| o == Observation::Tracked)
}

#[test]
fn a_stray_timestamp_that_comes_back_is_not_a_step() {
    // A minute of a board 100 ppm fast, then one to three blocks whose PTS
    // are 3 ms or 30 ms early or late, then back: the strays are held and
    // left out, nothing is told, and the estimates are those of a twin
    // that never saw them, within a part per billion and a microsecond.
    for (block, strays) in [(480, 1), (480, 3), (128, 1), (1_024, 2)] {
        for off in [0.003, -0.003, 0.03, -0.03] {
            let (mut e, mut twin) = (estimator(), estimator());
            let what = format!("{block} frames, {strays} off by {off}");
            let (_, frame) = twins((&mut e, &mut twin), (0, block, 60.0, 100.0), |_| 0.0);
            let stray = |k| if k < strays { off } else { 0.0 };
            let (seen, _) = twins((&mut e, &mut twin), (frame, block, 60.0, 100.0), stray);
            let held = seen.iter().take_while(|&&o| o == Observation::Held).count();
            assert_eq!(held as u64, strays, "{what}: {:?}", &seen[..6]);
            assert!(tracked_after(&seen, held), "{what}");
            assert_eq!((e.resets(), e.steps()), (0, 0), "{what}");
            assert!((e.drift_ppm() - twin.drift_ppm()).abs() < 1e-3, "{what}");
            assert!((e.error() - twin.error()).abs() < 1e-6, "{what}");
        }
    }
}

#[test]
fn an_excursion_held_from_going_back_is_not_a_step() {
    // One block's host time 30 ms late, and the blocks after it stamped no
    // earlier than it until the true times catch up, as a device input
    // keeps its PTS from going back: their residuals come back a block's
    // length at a time, never settling at one level, so nothing is told.
    for block in [128, 480, 1_024] {
        let (mut e, mut twin) = (estimator(), estimator());
        let (_, frame) = twins((&mut e, &mut twin), (0, block, 30.0, 0.0), |_| 0.0);
        let length = block as f64 / f64::from(RATE);
        let tail = |k: u64| (0.03 - k as f64 * length).max(0.0);
        let (seen, _) = twins((&mut e, &mut twin), (frame, block, 30.0, 0.0), tail);
        let held = seen.iter().take_while(|&&o| o == Observation::Held).count();
        assert!(held >= 1, "{block}: {:?}", &seen[..4]);
        assert!(tracked_after(&seen, held), "{block}");
        assert_eq!((e.resets(), e.steps()), (0, 0), "{block}");
        assert!((e.error() - twin.error()).abs() < 1e-6, "{block}");
    }
}

#[test]
fn a_lasting_step_is_confirmed_a_few_blocks_on_whatever_their_length() {
    // The same 30 ms lost at blocks of 128 to 1,024 frames: told at the
    // fourth block after it, at its full size, the estimates then those
    // of a twin less the step (the drift within the PTS's jitter).
    for block in [128, 480, 1_024] {
        let (mut e, mut twin) = (estimator(), estimator());
        let (_, frame) = twins((&mut e, &mut twin), (0, block, 30.0, 37.0), |_| 0.0);
        let (seen, _) = twins((&mut e, &mut twin), (frame, block, 30.0, 37.0), |_| 0.03);
        let confirmed = (CONFIRM_BLOCKS - 1) as usize;
        assert!(seen[..confirmed].iter().all(|&o| o == Observation::Held));
        let Observation::Step(by) = seen[confirmed] else {
            panic!("{block}: {:?}", &seen[..6]);
        };
        assert!((by + 0.03).abs() < 1e-4, "{block}: {by}");
        assert!(tracked_after(&seen, confirmed + 1), "{block}");
        assert_eq!((e.resets(), e.steps()), (0, 1), "{block}");
        assert!((e.drift_ppm() - twin.drift_ppm()).abs() < 0.05, "{block}");
        assert!((e.error() - twin.error() + 0.03).abs() < 1e-4, "{block}");
    }
}

#[test]
fn held_blocks_that_never_settle_are_a_break() {
    // PTS that jump between 10 and 20 ms late from block to block, never
    // back and never at one level: a break once they have been held for
    // twice the discontinuity.
    let mut e = estimator();
    let frame = feed(&mut e, 0, 30, 0.0, 0.0);
    let limit = 2.0 * DriftSettings::default().discontinuity.as_secs_f64();
    let mut held = 0;
    for k in 0..100 {
        let late = if k % 2 == 0 { 0.01 } else { 0.02 };
        match e.observe(pts(frame + k * BLOCK, 0.0, late), BLOCK) {
            Observation::Held => held += 1,
            Observation::Discontinuity => break,
            seen => panic!("block {k}: {seen:?}"),
        }
    }
    let blocks = limit * f64::from(RATE) / BLOCK as f64;
    assert!((f64::from(held) - blocks).abs() <= 1.0, "{held} held");
    assert_eq!((e.resets(), e.steps()), (1, 0));
}

#[test]
fn reset_forgets_the_epoch() {
    let mut e = estimator();
    e.reset();
    assert_eq!(e.resets(), 0, "nothing to forget");
    feed(&mut e, 0, 30, 300.0, 0.0);
    assert!(e.drift_ppm() > 100.0);
    e.reset();
    assert_eq!(
        (e.resets(), e.drift_ppm(), e.epoch_seconds()),
        (1, 0.0, 0.0)
    );
    assert_eq!(e.observe(Duration::from_secs(9), BLOCK), Observation::Start);
}

#[test]
fn bad_settings_and_a_zero_rate_are_refused() {
    let ok = DriftSettings::default();
    assert_eq!(ok.check(), Ok(()));
    assert_eq!(DriftEstimator::new(0, &ok).unwrap_err(), DriftError::Rate);
    let bad = [
        DriftSettings {
            discontinuity: Duration::ZERO,
            ..ok
        },
        DriftSettings {
            step: Duration::ZERO,
            ..ok
        },
        DriftSettings {
            step: ok.discontinuity,
            ..ok
        },
        DriftSettings {
            smoothing: Duration::ZERO,
            ..ok
        },
        DriftSettings {
            settle: Duration::ZERO,
            ..ok
        },
        DriftSettings {
            dead_band_ppm: -1.0,
            ..ok
        },
        DriftSettings {
            dead_band_ppm: f64::NAN,
            ..ok
        },
        DriftSettings {
            max_correction_ppm: 0.0,
            ..ok
        },
        DriftSettings {
            max_correction_ppm: 6_000.0,
            ..ok
        },
        DriftSettings {
            max_slew_ppm_per_s: f64::INFINITY,
            ..ok
        },
    ];
    for settings in bad {
        let err = DriftEstimator::new(RATE, &settings).unwrap_err();
        assert!(matches!(err, DriftError::Settings(_)), "{settings:?}");
    }
}

#[test]
fn a_pts_the_filter_cannot_use_is_a_break_and_nothing_goes_non_finite() {
    let mut e = estimator();
    e.observe(Duration::MAX, BLOCK);
    assert_eq!(e.observe(Duration::ZERO, BLOCK), Observation::Discontinuity);
    e.observe(Duration::from_millis(10), BLOCK);
    assert!(e.drift_ppm().is_finite() && e.error().is_finite());
}

#[test]
fn exact_pts_give_the_exact_slope_from_the_second_block_on() {
    // Blocks of 480 frames stamped exactly 9.99 ms apart: a clock
    // 1001.001… ppm fast, which the fit reads to the ppb as soon as it has
    // two blocks, and whose PTS have no scatter to doubt it by.
    let truth = (1e7 / 9_990_000.0 - 1.0) * 1e6;
    let mut e = estimator();
    for k in 0..300u64 {
        e.observe(Duration::from_nanos(k * 9_990_000), BLOCK);
        if k >= 1 {
            assert!(
                (e.drift_ppm() - truth).abs() < 0.01,
                "block {k}: {} ppm",
                e.drift_ppm()
            );
        }
        if k >= 2 {
            let doubt = e.drift_uncertainty_ppm();
            assert!(doubt < 0.01, "block {k}: ±{doubt} ppm");
        }
    }
}

#[test]
fn the_timing_reference_is_not_one_blocks_pts() {
    // The first block stamped off by tens of milliseconds, the rest
    // exact: the second block, that far off it, starts the epoch again
    // (nothing told, nothing counted), and the timing error is measured
    // against the fit from there.
    for off in [0.015, -0.03, 0.06] {
        let mut e = estimator();
        let first = Duration::from_secs(10);
        let at = |frame: u64| first + Duration::from_nanos(frame * 1_000_000_000 / 48_000);
        e.observe(Duration::from_secs_f64(first.as_secs_f64() + off), BLOCK);
        assert_eq!(e.observe(at(BLOCK), BLOCK), Observation::Start, "{off}");
        for k in 2..6_000 {
            assert_eq!(e.has_reference(), k >= 101, "block {k}");
            assert_eq!(e.observe(at(k * BLOCK), BLOCK), Observation::Tracked);
        }
        assert!(e.error().abs() < 1e-8, "{off}: {}", e.error());
        assert!(e.drift_ppm().abs() < 0.01, "{off}: {}", e.drift_ppm());
        assert_eq!((e.resets(), e.steps()), (0, 0), "{off}");
    }
    // A second block within a step of the first keeps the epoch: jitter
    // starts nothing again.
    let mut e = estimator();
    assert_eq!(e.observe(Duration::ZERO, BLOCK), Observation::Start);
    let late = pts(BLOCK, 0.0, 0.001_9);
    assert_eq!(e.observe(late, BLOCK), Observation::Tracked);
    assert!((e.epoch_seconds() - 0.02).abs() < 1e-12);
}

#[test]
fn a_step_before_the_reference_is_told_and_left_out_of_it() {
    // A drift-free board with half a millisecond of jitter loses 60 ms at
    // 0.4 s or 40 ms at 0.9 s: a step told a few blocks on, as after the
    // reference, and the reference taken at a second is the level before
    // it, so the timing error shows the step from then on.
    for (at, lost) in [(40, 6), (90, 4)] {
        let mut e = estimator();
        let mut noise = jitter(0.000_5);
        let (mut steps, mut frame) = (Vec::new(), 0);
        for k in 0..12_000 {
            if k == at {
                frame += lost * BLOCK;
            }
            if let Observation::Step(by) = e.observe(pts(frame, 0.0, noise()), BLOCK) {
                steps.push((k, by));
            }
            frame += BLOCK;
            if e.has_reference() {
                let (_, by) = steps[0];
                assert!((e.error() - by).abs() < 0.000_2, "{at}: block {k}: {by}");
            }
        }
        let [(k, by)] = steps[..] else {
            panic!("{at}: {steps:?}");
        };
        assert_eq!(k, at + CONFIRM_BLOCKS - 1);
        assert!((by + 0.01 * lost as f64).abs() < 0.001_5, "{at}: {by}");
        assert!(e.drift_ppm().abs() < 0.5, "{at}: {}", e.drift_ppm());
        assert_eq!(e.resets(), 0);
    }
    // In the first tenth of a second there is no scatter to tell a step
    // by yet: the block enters the fit.
    let mut e = estimator();
    for k in 0..5 {
        e.observe(pts(k * BLOCK, 0.0, 0.0), BLOCK);
    }
    let seen = e.observe(pts(9 * BLOCK, 0.0, 0.0), BLOCK);
    assert_eq!((seen, e.steps()), (Observation::Tracked, 0));
}
