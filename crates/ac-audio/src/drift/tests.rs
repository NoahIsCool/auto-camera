//! Tests of the drift estimator: an input on the master clock, fast and
//! slow clocks, jittery PTS and irregular blocks, steps in the PTS taken
//! for breaks, and settings it refuses.

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
        // The timing error is what the drift built up: ppm × elapsed.
        let built = ppm * 1e-6 * 300.0;
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
    assert_eq!(e.resets(), 0);
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
    // 40 ms of audio lost from a drift-free input: not a break (the pacer
    // does not fill it either), but timing error for the corrector to take
    // up, while the drift barely moves.
    let mut e = estimator();
    let frame = feed(&mut e, 0, 60, 0.0, 0.0);
    let lost = frame + 4 * 480;
    for k in 0..2_000 {
        let f = lost + k * BLOCK;
        let seen = e.observe(pts(f, 0.0, 0.0), BLOCK);
        assert_eq!(seen, Observation::Tracked);
    }
    assert!((e.error() + 0.04).abs() < 0.005, "{}", e.error());
    assert_eq!(e.resets(), 0);
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
