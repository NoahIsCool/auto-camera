//! Confirmed step levels remain measurable while their new fit settles;
//! another loss, a reconnection, and subsequent clock drift keep working.

use super::*;

/// Another real loss is made up before, on either side of, and after the
/// settling boundary; a later reconnection forgets that guard.
#[test]
fn steps_remain_detectable_around_the_settling_boundary() {
    for second in [20u64, 52, 53, 54, 100] {
        let mut e = estimator();
        let first = feed(&mut e, 0, 3, 0.0, 0.0);
        let mut steps = Vec::new();
        for k in 0..second + 200 {
            let gap = if k < second { 0.02 } else { 0.05 };
            let at = pts(first + k * BLOCK, 0.0, gap);
            if let Observation::Step(by) = e.observe(at, BLOCK) {
                steps.push(by);
            }
        }
        assert_eq!(steps.len(), 2, "second {second}: {steps:?}");
        assert!((steps[0] + 0.02).abs() < 1e-8, "{steps:?}");
        assert!((steps[1] + 0.03).abs() < 1e-8, "{steps:?}");
        assert!(e.drift_ppm().abs() < 0.01);

        let frame = first + (second + 200) * BLOCK;
        assert_eq!(
            e.observe(pts(frame, 0.0, 1.0), BLOCK),
            Observation::Discontinuity
        );
        assert_eq!(e.epoch.unwrap().settling, 0.0);
        assert!(!e.has_reference());
    }
}

/// A long block before confirmation does not consume a guard that only
/// starts at confirmation; subsequent variable blocks consume their time.
#[test]
fn the_settling_window_counts_only_time_after_confirmation() {
    let mut e = estimator();
    let mut frame = feed(&mut e, 0, 3, 0.0, 0.0);
    for n in [BLOCK, BLOCK, 48_000, BLOCK] {
        e.observe(pts(frame, 0.0, 0.02), n);
        frame += n;
    }
    assert_eq!(e.steps(), 1);
    assert_eq!(e.epoch.unwrap().settling, STEP_SETTLE.as_secs_f64());
    e.observe(pts(frame, 0.0, 0.02), 48_000);
    assert!((e.epoch.unwrap().settling - 0.49).abs() < 1e-12);
    frame += 48_000;
    e.observe(pts(frame, 0.0, 0.02), BLOCK);
    assert_eq!(e.epoch.unwrap().settling, 0.0);
}

/// A long block crossing an early guard's expiry cannot extrapolate the
/// unreliable pre-reference slope across the whole interval.
#[test]
fn a_long_block_after_an_early_step_does_not_invent_an_inverse_step() {
    for loss in [0.0, 0.03] {
        let mut e = estimator();
        let mut frame = 0;
        for jitter in [-0.00003, 0.0, 0.00003] {
            e.observe(pts(frame, 0.0, 1.0 + jitter), BLOCK);
            frame += BLOCK;
        }
        let mut steps = Vec::new();
        for k in 0..200 {
            let n = if k == 4 { 48_000 } else { BLOCK };
            let off = 1.02 + if k >= 5 { loss } else { 0.0 };
            if let Observation::Step(by) = e.observe(pts(frame, 0.0, off), n) {
                steps.push(by);
            }
            frame += n;
        }
        let expected = 1 + u64::from(loss > 0.0);
        assert_eq!((e.steps(), e.resets()), (expected, 0), "{e:?}");
        assert!((steps.iter().sum::<f64>() + 0.02 + loss).abs() < 0.0001);
        assert!(e.drift_ppm().abs() < 1.0, "{e:?}");
    }
}

/// Freezing the post-step prediction delays learning a changed crystal
/// rate only until the guard ends; positive and negative drift still fit.
#[test]
fn a_changed_sample_clock_is_learned_after_the_step_settles() {
    for ppm in [-1_000.0, -100.0, 100.0, 1_000.0] {
        let mut e = estimator();
        let first = feed(&mut e, 0, 3, 0.0, 0.0);
        let start = pts(first, 0.0, 0.02);
        for k in 0..30_000 {
            let at = start + pts(k * BLOCK, ppm, 0.0);
            e.observe(at, BLOCK);
        }
        assert_eq!((e.steps(), e.resets()), (1, 0), "{ppm}");
        assert!((e.drift_ppm() - ppm).abs() < 0.1, "{ppm}: {e:?}");
    }
}
