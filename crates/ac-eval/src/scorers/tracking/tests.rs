//! Tracking on hand-computed keyframes: roles, zones, CLEAR MOT matching
//! and switches, a two-track IDF1, waits, and the scored metrics.

use std::collections::BTreeMap;

use ac_core::{Point, Rect, SubjectId};

use super::*;
use crate::labels::{ExclusionZone, RoleInterval};
use crate::scorers::testkit::*;

/// Person A's box.
const A: Rect = Rect::new(0.1, 0.2, 0.2, 0.6);
/// Person B's box, apart from A's.
const B: Rect = Rect::new(0.6, 0.2, 0.2, 0.6);

/// A target `track` at `bbox`.
fn t(track: u32, bbox: Rect) -> (TrackId, Rect) {
    (TrackId(track), bbox)
}

/// A subject `id` at `bbox`.
fn s(id: u32, bbox: Rect) -> (SubjectId, Rect) {
    (SubjectId(id), bbox)
}

#[test]
fn roles_come_from_tags_and_intervals() {
    let mut labels = tagged(labels(2, Vec::new()), 1, "pastor");
    labels.roles.push(RoleInterval {
        track: TrackId(2),
        role: RoleTag("speaker".into()),
        start_frame: 10,
        end_frame: 20,
    });
    let pastor = RoleTag("pastor".into());
    let speaker = RoleTag("speaker".into());
    assert!(holds_role(&labels, TrackId(1), &pastor, 0));
    assert!(!holds_role(&labels, TrackId(2), &pastor, 15));
    // The interval is half open: frames 10 to 19.
    assert!(!holds_role(&labels, TrackId(2), &speaker, 9));
    assert!(holds_role(&labels, TrackId(2), &speaker, 10));
    assert!(holds_role(&labels, TrackId(2), &speaker, 19));
    assert!(!holds_role(&labels, TrackId(2), &speaker, 20));
}

#[test]
fn points_in_polygons() {
    let triangle = [
        Point { x: 0.0, y: 0.0 },
        Point { x: 1.0, y: 0.0 },
        Point { x: 0.0, y: 1.0 },
    ];
    assert!(inside_polygon(&triangle, Point { x: 0.2, y: 0.2 }));
    assert!(!inside_polygon(&triangle, Point { x: 0.6, y: 0.6 }));
    assert!(!inside_polygon(&triangle, Point { x: -0.1, y: 0.5 }));
    assert!(!inside_polygon(&[], Point { x: 0.0, y: 0.0 }));
}

#[test]
fn matching_finds_the_optimum_and_keeps_last_pairs() {
    // Boxes 0.6 high at y = 0.2, from x = 0 to 0.2 (person 1), 0.45
    // (person 2), 0.25 (subject 1) and 0.15 (subject 2). IoU: 1-1 0.8,
    // 2-1 0.25 / 0.45 = 0.556, 1-2 0.75, 2-2 0.15 / 0.45 = 0.333. Greedy
    // pairs the best overlap, person 1 with subject 1, and leaves person 2
    // with no subject above 0.5; the optimum matches both people.
    let wide = |w: f64| Rect::new(0.0, 0.2, w, 0.6);
    let targets = [t(1, wide(0.2)), t(2, wide(0.45))];
    let found = [s(1, wide(0.25)), s(2, wide(0.15))];
    let pairs = match_frame(&targets, &found, &BTreeMap::new(), 0.5);
    assert_eq!(
        pairs,
        [(TrackId(1), SubjectId(2)), (TrackId(2), SubjectId(1))]
    );
    let between = Rect::new(0.35, 0.2, 0.2, 0.6);
    // B last matched subject 1, which still overlaps it at IoU 0.6: kept,
    // although subject 2 overlaps it exactly.
    let near = Rect::new(0.65, 0.2, 0.2, 0.6);
    assert!((B.iou(&near) - 0.6).abs() < 1e-9);
    let last = BTreeMap::from([(TrackId(2), SubjectId(1))]);
    assert_eq!(
        match_frame(&[t(2, B)], &[s(1, near), s(2, B)], &last, 0.5),
        [(TrackId(2), SubjectId(1))]
    );
    // Below the threshold the last pair goes and the best one comes.
    assert_eq!(
        match_frame(&[t(2, B)], &[s(1, between), s(2, B)], &last, 0.5),
        [(TrackId(2), SubjectId(2))]
    );
    assert!(match_frame(&[], &[s(1, A)], &last, 0.5).is_empty());
}

#[test]
fn idf1_matches_a_hand_computed_two_track_example() {
    // Five keyframes, people A and B on each. Subject 1 is on A for the
    // first three and on B for the last two; subject 2 the other way.
    // Frames in common: A-1 3, A-2 2, B-1 2, B-2 3. The best pairing is
    // A-1 and B-2, 3 + 3 = 6 (the other, A-2 and B-1, gives 4), so
    // IDF1 = 2 * 6 / (10 targets + 10 subjects) = 0.6.
    let frames: Vec<Frame> = (0..5)
        .map(|i| {
            let (on_a, on_b) = if i < 3 { (1, 2) } else { (2, 1) };
            Frame {
                targets: vec![t(1, A), t(2, B)],
                found: vec![s(on_a, A), s(on_b, B)],
            }
        })
        .collect();
    let counts = idf1(&frames, 0.5);
    assert_eq!(
        counts,
        Idf1 {
            idtp: 6,
            targets: 10,
            found: 10
        }
    );
    assert_eq!(counts.value(), Some(0.6));
    // One false subject more on the first keyframe: 12 / 21.
    let mut extra = frames.clone();
    extra[0].found.push(s(3, Rect::new(0.4, 0.0, 0.1, 0.1)));
    assert_eq!(idf1(&extra, 0.5).value(), Some(12.0 / 21.0));
    assert_eq!(idf1(&[], 0.5).value(), None);
}

#[test]
fn waits_run_from_first_sight_or_loss_to_the_next_match() {
    const S: u64 = 1_000_000_000;
    // Unmatched at 0 and 1 s, matched at 2 s: acquired after 2 s. Lost at
    // 3 s, found at 5 s: 2 s. Lost again at 6 s to the end, 7 s: 1 s,
    // unresolved.
    let seen = [
        (0, false),
        (S, false),
        (2 * S, true),
        (3 * S, false),
        (4 * S, false),
        (5 * S, true),
        (6 * S, false),
        (7 * S, false),
    ];
    let w = waits(&seen);
    assert_eq!(
        w.acquire,
        Some(Wait {
            seconds: 2.0,
            resolved: true
        })
    );
    assert_eq!(
        w.reacquire,
        [
            Wait {
                seconds: 2.0,
                resolved: true
            },
            Wait {
                seconds: 1.0,
                resolved: false
            }
        ]
    );
    // Matched at once: no wait; never matched: a wait to the end.
    assert_eq!(
        waits(&[(0, true), (S, true)]).acquire,
        Some(Wait {
            seconds: 0.0,
            resolved: true
        })
    );
    assert_eq!(
        waits(&[(0, false), (3 * S, false)]).acquire,
        Some(Wait {
            seconds: 3.0,
            resolved: false
        })
    );
    assert_eq!(waits(&[]), Waits::default());
}

#[test]
fn the_scored_metrics_count_per_query() {
    // Six keyframes, 1 s apart, people A and B on each, an audience zone
    // at the bottom. Subject 1 tracks A throughout; subject 2 finds B only
    // from the third keyframe (2 s); subject 3 sits in the zone matching
    // nobody (ignored); subject 4, lost, holds A's box (not counted).
    let mut labels = labels(
        2,
        (0..6)
            .map(|i| keyframe(i * 10, vec![person(1, A), person(2, B)]))
            .collect(),
    );
    labels.exclusions.push(ExclusionZone {
        id: "audience".into(),
        polygon: vec![
            Point { x: 0.0, y: 0.9 },
            Point { x: 1.0, y: 0.9 },
            Point { x: 1.0, y: 1.0 },
            Point { x: 0.0, y: 1.0 },
        ],
    });
    let trace: Vec<_> = (0..6)
        .map(|i| {
            let mut subjects = vec![
                subject(1, A),
                subject(3, Rect::new(0.4, 0.92, 0.05, 0.05)),
                lost(subject(4, A)),
            ];
            if i >= 2 {
                subjects.push(subject(2, B));
            }
            record(i * 10, subjects, vec![wide()], Some("cam/wide"))
        })
        .collect();
    let r = report(&labels, &trace);
    // 10 of 12 targets matched; no false subject; no switch.
    assert!((value(&r, "tracking.recall").unwrap() - 10.0 / 12.0).abs() < 1e-12);
    assert_eq!(value(&r, "tracking.false_subjects_per_frame"), Some(0.0));
    assert_eq!(value(&r, "tracking.id_switches"), Some(0.0));
    // IDF1: A-1 on 6 keyframes, B-2 on 4: 2 * 10 / (12 + 10) = 20 / 22.
    assert!((value(&r, "tracking.idf1").unwrap() - 20.0 / 22.0).abs() < 1e-12);
    // B waited 2 s to be found, more than nothing and within the 2 s
    // limit; nobody was lost again.
    let acquire = r
        .metric("tracking.time_to_acquire", Some("person"))
        .unwrap();
    assert_eq!((acquire.value(), acquire.samples), (Some(2.0), 2));
    assert_eq!(acquire.verdict, crate::report::Verdict::Pass);
    assert_eq!(value(&r, "tracking.time_to_reacquire"), Some(0.0));
    // Out of the zone, subject 3 is false on every keyframe.
    labels.exclusions.clear();
    let r = report(&labels, &trace);
    assert_eq!(value(&r, "tracking.false_subjects_per_frame"), Some(1.0));
}
