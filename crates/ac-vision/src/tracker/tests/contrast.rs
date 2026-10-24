//! Tests of the contrast check on synthetic stage frames.
//!
//! NCC cannot tell a faint shadow from a contrasty performer of the same
//! shape, nor from a gentle relief on the wall, so these scenes start the
//! tracker on a faint silhouette and offer it such a match a box width or
//! more away. The check must hold the box where it was, without punishing a
//! real performer whose contrast changes because a light cue hit them. A
//! wash keeps their match strong; a followspot on their figure alone weakens
//! it to about 0.7 at a gain of 5 to 6, but it lands where they were heading,
//! so the check leaves it alone and they must still be followed. See the
//! `search` module docs for the rules.

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
fn a_late_start_does_not_jump_onto_a_high_contrast_lookalike() {
    // The lookalike scene, with the shadow's tracker started on frame 0 and
    // first updated on frame 7, as when a detection is applied a few frames
    // late. The search window, and the distance beyond which a move is
    // surprising, are three times as wide on that update, but the jump onto
    // the guitarist is still well beyond it.
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
    let mut tracker = TemplateTracker::default();
    tracker.start(&frame(0), SubjectId(8), start).unwrap();
    let path: Vec<_> = (7..27)
        .map(|seq| (seq, tracker.update(&frame(seq))[0]))
        .collect();
    assert_held(&path, start);
    for (seq, o) in &path {
        let iou = o.bbox.iou(&guitarist(*seq).rect());
        assert!(iou <= 0.3, "frame {seq}: IoU {iou:.2} with the guitarist");
    }
}

#[test]
fn config_validation_rejects_bad_surprise_fractions() {
    for f in [-0.1, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let config = TemplateTrackerConfig {
            surprise_fraction: f,
            ..Default::default()
        };
        assert!(
            matches!(
                TemplateTracker::new(config),
                Err(VisionError::InvalidConfig(_))
            ),
            "surprise_fraction {f} accepted"
        );
    }
    for f in [0.0, 0.25, 10.0] {
        let config = TemplateTrackerConfig {
            surprise_fraction: f,
            ..Default::default()
        };
        assert!(config.validate().is_ok(), "surprise_fraction {f} refused");
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
    // A textured guitarist walks 4 px per frame while a wash on him and the
    // backdrop steps at frame 20: x3.3 and x5 up and x0.3 down. Luma is
    // light times albedo and stays below 1, so every grey level scales by
    // the ratio and the window's contrast gain jumps by the ratio, beyond
    // `max_contrast_gain` either way. The matches stay strong (about 0.9),
    // so the box follows and the template relearns the new contrast within
    // a few frames.
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
                stage::lit_performer((x, y), figure, (light, light))
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
fn weak_contrast_mismatched_match_coasts_only_far_from_the_prediction() {
    // A textured square jumps 60 px in one frame, about half its side and
    // well beyond `surprise_fraction` of it, while half its texture changes,
    // so it matches at about 0.65. With its contrast unchanged the box
    // follows it; four times more contrasty (a gain of about 4.7, above
    // `max_contrast_gain`) the same match holds the box where it was. A
    // 15 px step, within `surprise_fraction` of the prediction, is followed
    // at that gain too.
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
    for (jump, contrast) in [(60.0, 1.0), (60.0, 4.0), (15.0, 4.0)] {
        let cx = 500.0 + jump;
        let next = stage::frame(1, 0.0, move |x, y| {
            match (texture((x, y), cx, 7), texture((x, y), cx, 8)) {
                (Some(a), Some(b)) => 0.45 + contrast * 0.12 * (a + b) / 2f64.sqrt(),
                _ => 0.45,
            }
        });
        let mut tracker = TemplateTracker::default();
        tracker.start(&first, SubjectId(1), start).unwrap();
        let o = tracker.update(&next)[0];
        let dx = (o.bbox.center().x - start.center().x) * SW as f64;
        if jump < 30.0 || contrast == 1.0 {
            assert!(
                (0.5..0.8).contains(&o.confidence),
                "jump {jump}, contrast {contrast}: a weak match, {}",
                o.confidence
            );
            assert!(
                (dx - jump).abs() < 3.0,
                "jump {jump}, contrast {contrast}: followed, {o:?}"
            );
        } else {
            assert!(o.confidence <= 0.3, "coasts: {}", o.confidence);
            assert!(dx.abs() < 1.0, "held: {o:?}");
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

#[test]
fn a_followspot_on_the_performer_alone_keeps_them_followed() {
    // At frame 20 a followspot snaps onto the guitarist, x2.5, x3 and x3.3
    // over the wash, and from 0.2 to 1.0 (x5), while the backdrop around him
    // stays at the wash's level, inside his box too. That changes the box's
    // structure as well as its contrast: his match sinks to 0.6 to 0.8 at a
    // gain of 5 to 6, beyond `max_contrast_gain`, and is never learned
    // because it stays below `update_score`. It lands where he was heading,
    // though, so the contrast check leaves it alone and the box keeps
    // following him, standing and walking 4 px per frame.
    for speed in [0.0, 4.0] {
        let walker = move |seq: u64| Figure {
            cx: 400.0 + speed * seq as f64,
            top: 180.0,
            height: 480.0,
            soft: 1.5,
        };
        for (before, after) in [(0.3, 0.75), (0.3, 0.9), (0.3, 0.99), (0.2, 1.0)] {
            let frame = |seq: u64| {
                let on_figure = if seq < 20 { before } else { after };
                let figure = walker(seq);
                stage::frame(seq, 0.01, move |x, y| {
                    stage::lit_performer((x, y), figure, (on_figure, before))
                })
            };
            for (seq, o) in track(walker(0).rect(), 60, frame) {
                let iou = o.bbox.iou(&walker(seq).rect());
                assert!(
                    o.confidence > 0.5 && iou > 0.75,
                    "{speed} px per frame, spot {before} to {after}, frame {seq}: \
                     confidence {}, IoU {iou:.2}",
                    o.confidence
                );
            }
        }
    }
}

#[test]
fn a_mismatched_match_below_accept_score_reports_its_score() {
    // The weak-match scene's 60 px jump at four times the contrast, with
    // most of the texture replaced (weights 0.3 and 0.95), so the best
    // match scores about 0.46, below `accept_score`. Its score alone holds
    // the box back, and it reports that score as any weak match does: the
    // `ambiguous_confidence` cap is only for matches the contrast check
    // held back.
    let side = 128.0;
    let texture = move |(x, y): (f64, f64), cx: f64, seed: u64| {
        let (u, v) = ((x - cx) / side + 0.5, (y - 360.0) / side + 0.5);
        ((0.0..1.0).contains(&u) && (0.0..1.0).contains(&v))
            .then(|| value_noise(u * 6.0, v * 6.0, seed) - 0.5)
    };
    let first = stage::frame(0, 0.0, move |x, y| {
        texture((x, y), 500.0, 7).map_or(0.45, |a| 0.45 + 0.12 * a)
    });
    let next = stage::frame(1, 0.0, move |x, y| {
        match (texture((x, y), 560.0, 7), texture((x, y), 560.0, 8)) {
            (Some(a), Some(b)) => 0.45 + 4.0 * 0.12 * (0.3 * a + 0.91f64.sqrt() * b),
            _ => 0.45,
        }
    });
    let start = Rect::new(
        (500.0 - side / 2.0) / SW as f64,
        (360.0 - side / 2.0) / SH as f64,
        side / SW as f64,
        side / SH as f64,
    );
    let mut tracker = TemplateTracker::default();
    tracker.start(&first, SubjectId(1), start).unwrap();
    let o = tracker.update(&next)[0];
    let config = tracker.config();
    assert!(
        o.confidence > config.ambiguous_confidence && o.confidence < config.accept_score,
        "reports its own score: {}",
        o.confidence
    );
    let dx = (o.bbox.center().x - start.center().x) * SW as f64;
    assert!(dx.abs() < 1.0, "held: {o:?}");
}
