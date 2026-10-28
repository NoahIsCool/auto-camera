//! Crop motion on hand-computed paths: difference quotients, a step with
//! known acceleration and jerk, holds, path splitting and the scored
//! metrics.

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
    assert_eq!(holds(&[0.0, 1.0, 2.0], &[true, true]), Vec::<f64>::new());
    assert_eq!(holds(&[0.0, 1.0, 3.0, 4.0], &[false, true, false]), [2.0]);
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
