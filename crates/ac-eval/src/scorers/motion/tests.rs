//! Crop motion on hand-computed paths: difference quotients, a step with
//! known acceleration and jerk, holds and turnarounds, path splitting and
//! the scored metrics.

use std::f64::consts::TAU;
use std::time::Duration;

use ac_core::{Rect, SourceId};

use super::*;
use crate::scorers::testkit::*;
use crate::trace::ShotTrace;

/// A 0.2-wide crop whose centre is at `cx` (its left edge 0.1 to the left).
fn at(cx: f64) -> Rect {
    Rect::new(cx - 0.1, 0.4, 0.2, 0.2)
}

/// `centres` 0.1 s apart from t = 0.
fn path(centres: &[f64]) -> Vec<(f64, Rect)> {
    centres
        .iter()
        .enumerate()
        .map(|(i, &c)| (i as f64 * 0.1, at(c)))
        .collect()
}

#[test]
fn differences_are_taken_at_midpoints() {
    // t^2 at t = 0, 1, 2, 3: velocities 1, 3, 5 at 0.5, 1.5, 2.5, then
    // accelerations 2, 2 at 1 and 2, then jerk 0 at 1.5.
    let squares: Vec<(f64, [f64; 1])> = (0..4).map(|t| (t as f64, [(t * t) as f64])).collect();
    let v = differentiate(&squares);
    assert_eq!(v, [(0.5, [1.0]), (1.5, [3.0]), (2.5, [5.0])]);
    assert_eq!(differentiate(&v), [(1.0, [2.0]), (2.0, [2.0])]);
    assert_eq!(differentiate(&differentiate(&v)), [(1.5, [0.0])]);
    // A skipped tick doubles the gap and the quotient stays right.
    let gap = [(0.0, [0.0]), (2.0, [4.0])];
    assert_eq!(differentiate(&gap), [(1.0, [2.0])]);
    // A time that does not advance gives no quotient.
    assert!(differentiate(&[(1.0, [0.0]), (1.0, [5.0])]).is_empty());
}

#[test]
fn a_step_crop_path_has_the_hand_computed_acceleration_and_jerk() {
    // Centre x 0.2, 0.2, 0.2, 0.3, 0.3, 0.3 (x 200 to 300 px of a 1000 px
    // source), 0.1 s apart:
    //   velocity     0, 0, 1000, 0, 0 px/s        (100 px / 0.1 s)
    //   acceleration 0, 10000, -10000, 0 px/s^2   (1000 / 0.1)
    //   jerk         100000, -200000, 100000 px/s^3
    // Peak acceleration 10000, RMS sqrt((1e8 + 1e8) / 4) = sqrt(5e7);
    // peak jerk 200000, RMS sqrt((1e10 + 4e10 + 1e10) / 3) = sqrt(2e10).
    let m = path_motion(
        &path(&[0.2, 0.2, 0.2, 0.3, 0.3, 0.3]),
        1000.0,
        1000.0,
        0.005,
    );
    let (peak, rms) = peak_and_rms(&m.acceleration).unwrap();
    assert!((peak - 10_000.0).abs() < 1e-6, "{peak}");
    assert!((rms - 5e7_f64.sqrt()).abs() < 1e-6, "{rms}");
    let (peak, rms) = peak_and_rms(&m.jerk).unwrap();
    assert!((peak - 200_000.0).abs() < 1e-4, "{peak}");
    assert!((rms - 2e10_f64.sqrt()).abs() < 1e-4, "{rms}");
    // The step moves 0.1 source widths in 0.1 s, five 0.2-wide crops per
    // second; the size never changes.
    let (pan, _) = peak_and_rms(&m.pan_speed).unwrap();
    assert!((pan - 5.0).abs() < 1e-9, "{pan}");
    assert_eq!(peak_and_rms(&m.zoom_rate), Some((0.0, 0.0)));
    // Still before and after the step, but those runs touch the path's
    // ends, so no hold is counted.
    assert!(m.holds.is_empty());
}

#[test]
fn holds_are_still_runs_with_motion_on_both_sides() {
    // Intervals: still, moving, still, still, moving: the run from the
    // third sample (t = 0.2) to the fifth (t = 0.4) is a 0.2 s hold.
    let m = path_motion(
        &path(&[0.2, 0.2, 0.3, 0.3, 0.3, 0.4]),
        1000.0,
        1000.0,
        0.005,
    );
    assert_eq!(m.holds.len(), 1);
    assert!((m.holds[0] - 0.2).abs() < 1e-9);
    // Rates across, down, log width and log height, at a rest speed of 1.
    let (still, right, down) = ([0.0; 4], [2.0, 0.0, 0.0, 0.0], [0.0, 2.0, 0.0, 0.0]);
    assert_eq!(
        holds(&[0.0, 1.0, 2.0], &[still, still], 1.0),
        Vec::<f64>::new()
    );
    assert_eq!(
        holds(&[0.0, 1.0, 3.0, 4.0], &[right, still, right], 1.0),
        [2.0]
    );
    // A pan, a stop, then a tilt: different axes, so a hold.
    assert_eq!(
        holds(&[0.0, 1.0, 3.0, 4.0], &[right, still, down], 1.0),
        [2.0]
    );
    // A rate at the rest speed itself is still.
    assert!(is_still(&[1.0, -1.0, 1.0, -1.0], 1.0));
    assert!(!is_still(&[1.0 + 1e-9, 0.0, 0.0, 0.0], 1.0));
}

#[test]
fn a_crop_that_goes_back_the_way_it_came_turns_round() {
    let (still, right, left) = ([0.0; 4], [2.0, 0.0, 0.0, 0.0], [-2.0, 0.0, 0.0, 0.0]);
    assert!(turns_round(&right, &left, 1.0));
    assert!(!turns_round(&right, &right, 1.0));
    // A rate at the rest speed on either side moves nothing back.
    assert!(!turns_round(&right, &[-1.0, 0.0, 0.0, 0.0], 1.0));
    assert!(!turns_round(&still, &left, 1.0));
    // One axis going back is enough, whatever the others do.
    assert!(turns_round(
        &[2.0, 2.0, 0.0, 0.0],
        &[2.0, -2.0, 0.0, 0.0],
        1.0
    ));
    assert_eq!(
        holds(&[0.0, 1.0, 3.0, 4.0], &[right, still, left], 1.0),
        Vec::<f64>::new()
    );
    // Out to 0.3, still for 0.2 s, back to 0.2: a turnaround, not a hold.
    let m = path_motion(
        &path(&[0.2, 0.2, 0.3, 0.3, 0.3, 0.2]),
        1000.0,
        1000.0,
        0.005,
    );
    assert!(m.holds.is_empty());
}

/// A 0.4-wide crop at 30 fps for 8 s, its centre swinging across as
/// `0.5 + amp * sin(2 pi (t - phase) / period)`.
fn swing(amp: f64, period: f64, phase: f64) -> Vec<(f64, Rect)> {
    (0..240)
        .map(|i| {
            let t = f64::from(i) / 30.0;
            let cx = 0.5 + amp * (TAU * (t - phase) / period).sin();
            (t, Rect::new(cx - 0.2, 0.3, 0.4, 0.4))
        })
        .collect()
}

#[test]
fn a_smooth_pan_back_and_forth_has_no_holds() {
    // A crop following a pacing speaker slows through zero at every
    // reversal, still for a tick or a few, and never settles. Over ten
    // phases of three swings, some reversals are still, none is a hold.
    let mut still_ticks = 0;
    for (amp, period) in [(0.05, 4.0), (0.1, 8.0), (0.03, 6.0)] {
        for k in 0..10 {
            let phase = period * f64::from(k) / 10.0;
            let m = path_motion(&swing(amp, period, phase), 1000.0, 1000.0, 0.005);
            assert!(m.holds.is_empty(), "{amp} {period} {phase}: {:?}", m.holds);
            still_ticks += m.pan_speed.iter().filter(|&&v| v <= 0.005).count();
        }
    }
    assert!(still_ticks > 0);
}

#[test]
fn a_smooth_pan_back_and_forth_meets_the_motion_defaults() {
    // The swing of 0.05 every 4 s, as a medium shot's trace at 30 fps:
    // within every C.4 cap, and with no hold to judge.
    let trace: Vec<_> = swing(0.05, 4.0, 1.0 / 60.0)
        .into_iter()
        .enumerate()
        .map(|(f, (_, crop))| {
            let mut r = record(
                f as u64,
                Vec::new(),
                vec![shot("cam/1/medium", Some(1), crop)],
                None,
            );
            r.pts = Duration::from_nanos(f as u64 * 1_000_000_000 / 30);
            r
        })
        .collect();
    let r = report(&labels(0, Vec::new()), &trace);
    for name in ["motion.peak_acceleration", "motion.peak_pan_speed"] {
        assert_eq!(
            r.metric(name, None).unwrap().verdict,
            crate::report::Verdict::Pass,
            "{name}"
        );
    }
    let hold = r.metric("motion.min_static_hold", None).unwrap();
    assert_eq!(
        (hold.value(), hold.verdict),
        (None, crate::report::Verdict::NotMeasurable)
    );
}

#[test]
fn a_zoom_is_measured_in_log_width() {
    // Width 0.2 then 0.2e^0.03 in 0.1 s: a zoom rate of 0.3 log widths/s.
    let zoom = [
        (0.0, Rect::new(0.4, 0.4, 0.2, 0.2)),
        (0.1, Rect::new(0.4, 0.4, 0.2 * 0.03_f64.exp(), 0.2)),
    ];
    let m = path_motion(&zoom, 1000.0, 1000.0, 0.005);
    assert!((m.zoom_rate[0] - 0.3).abs() < 1e-9);
    assert!(m.acceleration.is_empty() && m.jerk.is_empty());
}

#[test]
fn paths_split_where_a_shot_is_missing_and_skip_wides_and_other_sources() {
    let mut side = shot("side/1/close", Some(1), at(0.5));
    side.source = SourceId("side".into());
    let crops = |f: u64| -> Vec<ShotTrace> {
        let mut shots = vec![wide(), side.clone()];
        if f != 2 {
            shots.push(shot("cam/1/close", Some(1), at(0.5)));
        }
        if f == 3 {
            // No size: left out.
            shots.push(shot("cam/2/close", Some(2), Rect::new(0.1, 0.1, 0.0, 0.2)));
        }
        shots
    };
    let trace: Vec<_> = (0..5)
        .map(|f| record(f, Vec::new(), crops(f), None))
        .collect();
    let paths = shot_paths(&trace, &SourceId(CAM.into()));
    let lengths: Vec<(String, usize)> = paths
        .iter()
        .map(|p| (p.shot.0.clone(), p.samples.len()))
        .collect();
    assert_eq!(
        lengths,
        [("cam/1/close".into(), 2), ("cam/1/close".into(), 2)]
    );
    assert_eq!(paths[1].samples[0].0, 0.3);
    assert!(shot_paths(&[], &SourceId(CAM.into())).is_empty());
}

#[test]
fn a_path_splits_where_time_fails_to_advance() {
    // Ticks at 0, 0.1 and 0.2 s with the crop centred on 0.5, then the
    // clock steps back to 0.1 s and runs on to 0.2 s with it on 0.7. The
    // step starts a second path, so the jump to 0.7 is no motion: two
    // still paths, of three samples and of two.
    let trace: Vec<_> = [(0, 0.5), (1, 0.5), (2, 0.5), (1, 0.7), (2, 0.7)]
        .iter()
        .map(|&(f, c)| {
            record(
                f,
                Vec::new(),
                vec![shot("cam/1/close", Some(1), at(c))],
                None,
            )
        })
        .collect();
    let paths = shot_paths(&trace, &SourceId(CAM.into()));
    let starts: Vec<(f64, usize)> = paths
        .iter()
        .map(|p| (p.samples[0].0, p.samples.len()))
        .collect();
    assert_eq!(starts, [(0.0, 3), (0.1, 2)]);
    for p in &paths {
        let m = path_motion(&p.samples, 1000.0, 1000.0, 0.005);
        assert_eq!(peak_and_rms(&m.pan_speed), Some((0.0, 0.0)));
    }
}

#[test]
fn the_scored_metrics_judge_the_step_against_the_defaults() {
    let labels = labels(0, Vec::new());
    let trace: Vec<_> = [0.2, 0.2, 0.2, 0.3, 0.3, 0.3]
        .iter()
        .enumerate()
        .map(|(f, &c)| {
            record(
                f as u64,
                Vec::new(),
                vec![shot("cam/1/close", Some(1), at(c))],
                None,
            )
        })
        .collect();
    let r = report(&labels, &trace);
    // 10000 px/s^2 is past the 1200 px/s^2 PoC baseline; 5 crop widths/s
    // past the 0.5 cap.
    let accel = r.metric("motion.peak_acceleration", None).unwrap();
    assert_eq!(accel.verdict, crate::report::Verdict::Fail);
    assert!((accel.value().unwrap() - 10_000.0).abs() < 1e-6);
    assert_eq!(
        r.metric("motion.peak_pan_speed", None).unwrap().verdict,
        crate::report::Verdict::Fail
    );
    assert_eq!(
        r.metric("motion.rms_jerk", None).unwrap().verdict,
        crate::report::Verdict::Unchecked
    );
    assert_eq!(value(&r, "motion.min_static_hold"), None);
}
