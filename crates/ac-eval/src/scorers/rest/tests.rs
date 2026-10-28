//! Shots of people at rest: rest spans from the labels, hand-computed
//! settle times and jitter, and the scored metrics.

use ac_core::{Point, Rect};

use super::*;
use crate::report::Verdict;
use crate::scorers::testkit::*;

/// A crop 0.4 wide and `h` high centred on the middle of the picture.
fn crop(h: f64) -> Rect {
    Rect::from_center(Point { x: 0.5, y: 0.5 }, 0.4, h)
}

/// `heights` as crops 0.1 s apart from t = 0.
fn samples(heights: &[f64]) -> Vec<(f64, Rect)> {
    heights
        .iter()
        .enumerate()
        .map(|(i, &h)| (i as f64 * 0.1, crop(h)))
        .collect()
}

/// Person 1's box, standing.
const STANDING: Rect = Rect::new(0.4, 0.2, 0.2, 0.7);

#[test]
fn rest_spans_are_still_runs_long_enough() {
    let moved = Rect::new(0.5, 0.2, 0.2, 0.7);
    let labels = labels(
        3,
        vec![
            // Person 1 still at 0, 1 and 2 s, then 0.1 source widths away
            // a second later: 0.1 / s, far above 0.005 / s.
            keyframe(
                0,
                vec![
                    person(1, STANDING),
                    person(2, STANDING),
                    person(3, STANDING),
                ],
            ),
            keyframe(5, vec![person(2, STANDING), person(3, moved)]),
            keyframe(10, vec![person(1, STANDING), person(3, STANDING)]),
            keyframe(20, vec![person(1, STANDING)]),
            keyframe(30, vec![person(1, moved)]),
        ],
    );
    let spans = rest_spans(&labels, 0.005, 1.0);
    // Person 1's run from frame 10 (missing at 5, so the run from 0 broke)
    // to 20 is 1 s, just long enough; person 2's 0.5 s is not, and person
    // 3 moves between every pair.
    assert_eq!(
        spans,
        [RestSpan {
            track: TrackId(1),
            first_frame: 10,
            last_frame: 20,
            start_ns: 1_000_000_000,
            end_ns: 2_000_000_000,
        }]
    );
    assert!(rest_spans(&labels, 0.005, 1.5).is_empty());
}

#[test]
fn a_growing_box_is_not_at_rest() {
    // Height 0.7 then 0.707 a second later: ln(1.01) = 0.00995 / s, above
    // 0.005 / s; at 0.7035 it is ln(1.005) = 0.00499 / s, below.
    let grown = Rect::new(0.4, 0.2, 0.2, 0.707);
    let barely = Rect::new(0.4, 0.2, 0.2, 0.7035);
    let spans = |b: Rect| {
        rest_spans(
            &labels(
                1,
                vec![
                    keyframe(0, vec![person(1, STANDING)]),
                    keyframe(10, vec![person(1, b)]),
                ],
            ),
            0.005,
            1.0,
        )
        .len()
    };
    assert_eq!(spans(grown), 0);
    assert_eq!(spans(barely), 1);
}

#[test]
fn settle_time_runs_to_the_first_sample_that_stays_in_the_band() {
    // Final height 0.32; the 1% band is 0.0032. 0.315 is 0.005 away, so
    // the crop settles at the next sample, t = 0.3. The midpoint of the
    // 0.5 s span is 0.25, so jitter is measured from t = 0.3 too: 0.
    let s = settling(
        &samples(&[0.30, 0.31, 0.315, 0.32, 0.32, 0.32]),
        0.0,
        0.5,
        0.01,
    )
    .unwrap();
    assert!((s.settle_s - 0.3).abs() < 1e-12);
    assert_eq!(s.jitter, 0.0);
    // Already settled: no time at all.
    let s = settling(&samples(&[0.32, 0.32]), 0.0, 0.1, 0.01).unwrap();
    assert_eq!(s.settle_s, 0.0);
}

#[test]
fn jitter_is_the_height_variation_after_settling() {
    // 0.300 and 0.301 in turn, final 0.301: every sample is within 1% of
    // it, so the crop settled at once, and its height varies by 0.001 /
    // 0.301 = 0.00332, more than the 0.0019 of one 1080p output pixel.
    let s = settling(
        &samples(&[0.300, 0.301, 0.300, 0.301, 0.300, 0.301]),
        0.0,
        0.5,
        0.01,
    )
    .unwrap();
    assert_eq!(s.settle_s, 0.0);
    assert!((s.jitter - 0.001 / 0.301).abs() < 1e-12);
    // Swinging 0.30 to 0.33 it never settles before its last sample (0.5
    // s), and is scored from the midpoint: 0.33, 0.30, 0.33, a variation
    // of 0.03 / 0.33.
    let s = settling(
        &samples(&[0.30, 0.33, 0.30, 0.33, 0.30, 0.33]),
        0.0,
        0.5,
        0.01,
    )
    .unwrap();
    assert!((s.settle_s - 0.5).abs() < 1e-12);
    assert!((s.jitter - 0.03 / 0.33).abs() < 1e-12);
    // One sample settles nothing.
    assert_eq!(settling(&samples(&[0.3]), 0.0, 0.0, 0.01), None);
    assert_eq!(settling(&[], 0.0, 0.0, 0.01), None);
}

#[test]
fn the_scored_metrics_find_jitter_at_rest() {
    // Person 1 still for 2 s (keyframes at 0, 1 and 2 s); subject 1's
    // close-up alternates between 0.300 and 0.301 high every tick, ending
    // at 0.300 on frame 20: a variation of 0.001 / 0.3.
    let labels = labels(
        1,
        (0..3)
            .map(|i| keyframe(i * 10, vec![person(1, STANDING)]))
            .collect(),
    );
    let trace: Vec<_> = (0..=20)
        .map(|f| {
            let h = if f % 2 == 0 { 0.300 } else { 0.301 };
            record(
                f,
                vec![subject(1, STANDING)],
                vec![shot("cam/1/close", Some(1), crop(h))],
                None,
            )
        })
        .collect();
    let r = report(&labels, &trace);
    let jitter = r.metric("motion.jitter_at_rest", None).unwrap();
    assert!((jitter.value().unwrap() - 0.001 / 0.3).abs() < 1e-12);
    assert_eq!(jitter.verdict, Verdict::Fail);
    assert_eq!(jitter.details["rest_spans"], 1.0);
    assert_eq!(value(&r, "motion.settle_time"), Some(0.0));
    // A shot whose subject matches the person on only part of the span is
    // not scored: from frame 10 subject 1's box is elsewhere.
    let mut moved = trace.clone();
    for r in &mut moved[10..] {
        r.sources[0].subjects[0].bbox = Rect::new(0.0, 0.0, 0.1, 0.1);
    }
    assert_eq!(
        value(&report(&labels, &moved), "motion.jitter_at_rest"),
        None
    );
}
