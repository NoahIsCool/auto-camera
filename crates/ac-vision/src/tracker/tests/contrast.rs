//! Tests of the contrast check on synthetic stage frames.
//!
//! NCC cannot tell a faint shadow from a contrasty performer of the same
//! shape, nor from a gentle relief on the wall, so these scenes start the
//! tracker on a faint silhouette and offer it such a match. The check must
//! hold the box where it was, without punishing a real performer whose
//! contrast changes because a light cue hit them: those matches stay strong
//! and must still be followed. See the `Trust` section of the module docs.

use super::stage::{self, Figure, SH, SW};
use super::*;

/// A soft person-shaped shadow cast on the wall, about 134 x 384 px.
fn faint_shadow() -> Figure {
    Figure {
        cx: 720.0,
        top: 100.0,
        height: 370.0,
        soft: 14.0,
    }
}

/// Offset of `bbox`'s centre from `from`'s, in widths of `from`.
fn offset_in_widths(bbox: Rect, from: Rect) -> f64 {
    let (a, b) = (bbox.center(), from.center());
    let dx = (a.x - b.x) * SW as f64;
    let dy = (a.y - b.y) * SH as f64;
    dx.hypot(dy) / (from.w * SW as f64)
}

/// Starts subject 8 on `start` in `frame(0)` and returns its observations on
/// frames `1..frames`.
fn track(
    start: Rect,
    frames: u64,
    frame: impl Fn(u64) -> VideoFrame,
) -> Vec<(u64, TrackObservation)> {
    let mut tracker = TemplateTracker::default();
    tracker.start(&frame(0), SubjectId(8), start).unwrap();
    (1..frames)
        .map(|seq| (seq, tracker.update(&frame(seq))[0]))
        .collect()
}

/// Asserts that no confident observation lies more than a quarter of the
/// start box's width from it and that the box never grows by more than 5 %.
fn assert_held(path: &[(u64, TrackObservation)], start: Rect) {
    for (seq, o) in path {
        let off = offset_in_widths(o.bbox, start);
        assert!(
            o.confidence < 0.5 || off <= 0.25,
            "frame {seq}: confidence {} at {off:.2} box widths from the start",
            o.confidence
        );
        assert!(
            o.bbox.w <= 1.05 * start.w && o.bbox.h <= 1.05 * start.h,
            "frame {seq}: the box grew to {:?} from {start:?}",
            o.bbox
        );
    }
}

#[test]
fn a_faint_silhouette_does_not_jump_onto_a_high_contrast_lookalike() {
    // A shadow 12 % darker than a wall at 0.7, whose box varies by about 7
    // grey levels (standard deviation), with nobody near at the start. A guitarist of the same shape, 70 px clear of
    // its box, sways 140 px towards it within 15 frames: into its search
    // window, then halfway over its box. Without the contrast check the box
    // jumps onto him at 0.71 to 0.74 (gain 5.4 to 6) and grows to his size.
    let shadow = faint_shadow();
    let guitarist = |seq: u64| {
        Figure {
            cx: 520.0,
            top: 215.0,
            height: 475.0,
            soft: 1.5,
        }
        .shifted(stage::sway(seq, 140.0, 60.0))
    };
    let frame = |seq: u64| {
        let g = guitarist(seq);
        stage::frame(seq, 0.04, move |x, y| {
            stage::shadow_scene((x, y), 0.7, (shadow, 0.12), Some(g))
        })
    };
    let start = shadow.rect();
    assert_eq!(start.iou(&guitarist(0).rect()), 0.0, "nobody near at first");
    let path = track(start, 20, frame);
    assert_held(&path, start);
    for (seq, o) in &path {
        let iou = o.bbox.iou(&guitarist(*seq).rect());
        assert!(iou <= 0.3, "frame {seq}: IoU {iou:.2} with the guitarist");
    }
}

#[test]
fn a_faint_template_does_not_jump_onto_flat_background() {
    // The same shadow on a wall with a gentle relief (3 % either way over
    // about 260 px). At frame 5 the light casting it goes out, and the best
    // remaining match is a patch of relief 145 px away: 0.66 at a gain of
    // about 0.2 without the contrast check.
    let shadow = faint_shadow();
    let frame = |seq: u64| {
        let dark = if seq < 5 { 0.12 } else { 0.0 };
        stage::frame(seq, 0.04, move |x, y| {
            let wall = 0.7 + 0.06 * (value_noise(x / 260.0, y / 260.0, 3) - 0.5);
            stage::shadow_scene((x, y), wall, (shadow, dark), None)
        })
    };
    let start = shadow.rect();
    let path = track(start, 15, frame);
    assert_held(&path, start);
    assert!(
        path.iter().all(|(seq, o)| *seq < 5 || o.confidence <= 0.3),
        "the vanished shadow reports at most the ambiguous confidence: {path:?}"
    );
}

#[test]
fn strong_matches_survive_instant_light_cues() {
    // A textured guitarist walks 4 px per frame while the light on him and
    // the backdrop steps at frame 20: x3.3 and x5 up (a wash snapping to a
    // spot) and x0.3 down. Luma is light times albedo and stays below 1, so
    // every grey level scales by the ratio and the window's contrast gain
    // jumps by the ratio, beyond `max_contrast_gain` either way. The matches
    // stay strong (about 0.9), so the box follows and the template relearns
    // the new contrast within a few frames.
    let walker = |seq: u64| Figure {
        cx: 400.0 + 4.0 * seq as f64,
        top: 180.0,
        height: 480.0,
        soft: 1.5,
    };
    for (before, after) in [(0.3, 0.99), (0.2, 1.0), (1.0, 0.3)] {
        let frame = |seq: u64| {
            let light = if seq < 20 { before } else { after };
            let figure = walker(seq);
            stage::frame(seq, 0.01, move |x, y| {
                stage::lit_performer((x, y), figure, light)
            })
        };
        for (seq, o) in track(walker(0).rect(), 45, frame) {
            let iou = o.bbox.iou(&walker(seq).rect());
            assert!(
                o.confidence > 0.8 && iou > 0.8,
                "light {before} to {after}, frame {seq}: confidence {}, IoU {iou:.2}",
                o.confidence
            );
        }
    }
}

#[test]
fn weak_contrast_mismatched_match_coasts() {
    // A textured square moves 30 px in one frame while half its texture
    // changes, so it matches at about 0.65. With its contrast unchanged the
    // box follows it; four times more contrasty (a gain of about 4.7, above
    // `max_contrast_gain`) the same match holds the box where it was.
    let side = 128.0;
    let texture = move |(x, y): (f64, f64), cx: f64, seed: u64| {
        let (u, v) = ((x - cx) / side + 0.5, (y - 360.0) / side + 0.5);
        ((0.0..1.0).contains(&u) && (0.0..1.0).contains(&v))
            .then(|| value_noise(u * 6.0, v * 6.0, seed) - 0.5)
    };
    let first = stage::frame(0, 0.0, move |x, y| {
        texture((x, y), 500.0, 7).map_or(0.45, |a| 0.45 + 0.12 * a)
    });
    let start = Rect::new(
        (500.0 - side / 2.0) / SW as f64,
        (360.0 - side / 2.0) / SH as f64,
        side / SW as f64,
        side / SH as f64,
    );
    let moved = start.center().x + 30.0 / SW as f64;
    for contrast in [1.0, 4.0] {
        let next = stage::frame(1, 0.0, move |x, y| {
            match (texture((x, y), 530.0, 7), texture((x, y), 530.0, 8)) {
                (Some(a), Some(b)) => 0.45 + contrast * 0.12 * (a + b) / 2f64.sqrt(),
                _ => 0.45,
            }
        });
        let mut tracker = TemplateTracker::default();
        tracker.start(&first, SubjectId(1), start).unwrap();
        let o = tracker.update(&next)[0];
        let x = o.bbox.center().x;
        if contrast == 1.0 {
            assert!(
                (0.5..0.8).contains(&o.confidence),
                "a weak match: {}",
                o.confidence
            );
            assert!((x - moved).abs() * (SW as f64) < 3.0, "followed: {o:?}");
        } else {
            assert!(o.confidence <= 0.3, "coasts: {}", o.confidence);
            assert!(
                (x - start.center().x).abs() * (SW as f64) < 1.0,
                "held: {o:?}"
            );
            assert!((o.bbox.w - start.w).abs() < 1e-9, "size held: {o:?}");
        }
    }
}

#[test]
fn config_validation_rejects_bad_contrast_gains() {
    for g in [0.99, 0.0, -2.0, f64::NAN, f64::INFINITY] {
        let config = TemplateTrackerConfig {
            max_contrast_gain: g,
            ..Default::default()
        };
        assert!(
            matches!(
                TemplateTracker::new(config),
                Err(VisionError::InvalidConfig(_))
            ),
            "max_contrast_gain {g} accepted"
        );
    }
    for g in [1.0, 2.5, 1e6] {
        let config = TemplateTrackerConfig {
            max_contrast_gain: g,
            ..Default::default()
        };
        assert!(config.validate().is_ok(), "max_contrast_gain {g} refused");
    }
}
