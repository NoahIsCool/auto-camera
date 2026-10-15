//! Association tests: matching, crossing, aliasing, admission and capacity.

use std::collections::HashSet;

use ac_core::{Rect, SubjectId, TrackState};

use crate::testkit::*;
use crate::{Detection, RetireReason, StoreConfig, StoreEvent, SubjectStore};

/// A store with room for `max` subjects.
fn capped(max: usize) -> SubjectStore {
    store_with(StoreConfig {
        max_subjects: max,
        ..StoreConfig::default()
    })
}

/// A detection with an explicit score.
fn scored(query: &str, bbox: Rect, score: f32) -> Detection {
    Detection {
        score,
        ..det(query, bbox)
    }
}

#[test]
fn repeated_detections_continue_the_same_subjects() {
    let mut s = store();
    let ev = s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.2, 0.5)),
            det("person", person(0.6, 0.5)),
        ],
    );
    assert_eq!(created(&ev), vec![SubjectId(1), SubjectId(2)]);
    assert!(ev.contains(&StoreEvent::NeedsTrackerStart {
        id: SubjectId(1),
        bbox: person(0.2, 0.5),
        pts: t(0.0)
    }));
    for i in 1..=10 {
        let dx = 0.005 * i as f64;
        let ev = s.ingest_detections(
            t(0.5 * i as f64),
            // Reverse order: association must not depend on it.
            &[
                det("person", person(0.6 - dx, 0.5)),
                det("person", person(0.2 + dx, 0.5)),
            ],
        );
        assert!(created(&ev).is_empty(), "step {i}: {ev:?}");
        s.tick(t(0.5 * i as f64));
    }
    assert_eq!(ids(&s), vec![SubjectId(1), SubjectId(2)]);
    assert!((cx(&s, SubjectId(1)) - 0.25).abs() < 1e-9);
    assert!((cx(&s, SubjectId(2)) - 0.55).abs() < 1e-9);
}

#[test]
fn invalid_detections_are_ignored_and_edge_boxes_clipped() {
    let mut s = store();
    let bad = [
        scored("person", person(0.5, 0.5), 0.1),
        scored("person", person(0.5, 0.5), f32::NAN),
        det("person", Rect::new(f64::NAN, 0.2, 0.1, 0.1)),
        det("person", Rect::new(0.2, 0.2, -0.1, 0.1)),
        det("person", Rect::new(1.2, 0.2, 0.1, 0.1)),
    ];
    assert!(s.ingest_detections(t(0.0), &bad).is_empty());
    assert!(s.ingest_detections(t(0.0), &[]).is_empty());
    let ev = s.ingest_detections(t(0.1), &[det("person", Rect::new(-0.05, 0.2, 0.1, 0.3))]);
    let bbox = s.get(created(&ev)[0]).unwrap().bbox;
    assert!(bbox.x == 0.0 && (bbox.w - 0.05).abs() < 1e-12);
}

/// Two people walk past each other at 0.1 frame widths per second and meet
/// at x = 0.5 at t = 2 s, observed only by a 5 Hz detector.
#[test]
fn crossing_subjects_keep_their_ids() {
    let mut s = store();
    let path = |start: f64, speed: f64, time: f64| person(start + speed * time, 0.5);
    for step in 0..=20 {
        let time = step as f64 * 0.2;
        let mut dets = vec![
            det("person", path(0.3, 0.1, time)),
            det("person", path(0.7, -0.1, time)),
        ];
        if step % 2 == 1 {
            dets.reverse();
        }
        let ev = s.ingest_detections(t(time), &dets);
        assert!(step == 0 || created(&ev).is_empty(), "step {step}");
        s.tick(t(time));
    }
    assert_eq!(s.len(), 2);
    assert!(
        (cx(&s, SubjectId(1)) - 0.7).abs() < 1e-9,
        "left walker ends right"
    );
    assert!(
        (cx(&s, SubjectId(2)) - 0.3).abs() < 1e-9,
        "right walker ends left"
    );
}

/// The same crossing seen by 30 Hz trackers and a 2 Hz detector whose
/// results arrive 0.2 s late.
#[test]
fn crossing_with_tracks_and_late_detections_keeps_ids() {
    let mut s = store();
    let path = |start: f64, speed: f64, time: f64| person(start + speed * time, 0.5);
    s.ingest_detections(
        t(0.0),
        &[
            det("person", path(0.3, 0.1, 0.0)),
            det("person", path(0.7, -0.1, 0.0)),
        ],
    );
    let (a, b) = (SubjectId(1), SubjectId(2));
    for frame in 1..=120 {
        let time = frame as f64 / 30.0;
        let tracks = [
            track(a, path(0.3, 0.1, time)),
            track(b, path(0.7, -0.1, time)),
        ];
        s.ingest_tracks(t(time), &tracks);
        if frame % 15 == 6 {
            let shot = (time - 0.2).max(0.0);
            let ev = s.ingest_detections(
                t(shot),
                &[
                    det("person", path(0.7, -0.1, shot)),
                    det("person", path(0.3, 0.1, shot)),
                ],
            );
            assert!(created(&ev).is_empty(), "frame {frame}: {ev:?}");
            let restarted = ev
                .iter()
                .any(|e| matches!(e, StoreEvent::NeedsTrackerStart { .. }));
            assert!(!restarted, "agreeing detections must not restart trackers");
        }
        assert!(s.tick(t(time)).is_empty());
    }
    assert!((cx(&s, a) - 0.7).abs() < 1e-9);
    assert!((cx(&s, b) - 0.3).abs() < 1e-9);
}

#[test]
fn drifted_tracker_is_restarted_from_the_detection() {
    let mut s = store();
    s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.3, 0.5)),
            det("person", person(0.6, 0.5)),
        ],
    );
    let (a, b) = (SubjectId(1), SubjectId(2));
    // B's tracker slides onto A while B stands still.
    s.ingest_tracks(
        t(0.2),
        &[track(a, person(0.3, 0.5)), track(b, person(0.31, 0.5))],
    );
    let ev = s.ingest_detections(
        t(0.5),
        &[
            det("person", person(0.3, 0.5)),
            det("person", person(0.6, 0.5)),
        ],
    );
    assert!(created(&ev).is_empty());
    assert!(ev.contains(&StoreEvent::NeedsTrackerStart {
        id: b,
        bbox: person(0.6, 0.5),
        pts: t(0.5)
    }));
    assert!((cx(&s, b) - 0.6).abs() < 1e-9);
    assert!((cx(&s, a) - 0.3).abs() < 1e-9);
}

#[test]
fn a_twin_sitting_on_another_subject_is_lost_conservatively() {
    let mut s = store();
    s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.3, 0.5)),
            det("person", person(0.6, 0.5)),
        ],
    );
    let (a, b) = (SubjectId(1), SubjectId(2));
    // B walked out; its tracker latched onto A and stays confident.
    let mut lost_at = None;
    for frame in 1..=60 {
        let time = frame as f64 / 30.0;
        s.ingest_tracks(
            t(time),
            &[track(a, person(0.3, 0.5)), track(b, person(0.3, 0.5))],
        );
        if frame % 15 == 0 {
            let ev = s.ingest_detections(t(time), &[det("person", person(0.3, 0.5))]);
            assert!(!ev.contains(&StoreEvent::Lost { id: a }));
            if ev.contains(&StoreEvent::Lost { id: b }) {
                lost_at = Some(time);
            }
        }
    }
    assert_eq!(lost_at, Some(1.5), "lost after shadowing for lost_after");
    assert!(s.record(a).unwrap().is_tracking());
    assert!(!s.record(b).unwrap().tracker_running);
}

#[test]
fn protected_twins_are_not_demoted() {
    let mut s = store();
    s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.3, 0.5)),
            det("person", person(0.6, 0.5)),
        ],
    );
    let b = SubjectId(2);
    s.set_protected(&HashSet::from([b]));
    for frame in 1..=90 {
        let time = frame as f64 / 30.0;
        s.ingest_tracks(
            t(time),
            &[
                track(SubjectId(1), person(0.3, 0.5)),
                track(b, person(0.3, 0.5)),
            ],
        );
        if frame % 15 == 0 {
            s.ingest_detections(t(time), &[det("person", person(0.3, 0.5))]);
        }
    }
    assert!(s.record(b).unwrap().is_tracking());
}

#[test]
fn lost_subject_is_recovered_under_the_same_id() {
    let mut s = store();
    s.ingest_detections(t(0.0), &[det("person", person(0.4, 0.5))]);
    let id = SubjectId(1);
    s.tick(t(2.0));
    assert_eq!(s.get(id).unwrap().state, TrackState::Lost);
    let ev = s.ingest_detections(t(5.0), &[det("person", person(0.42, 0.5))]);
    assert_eq!(
        ev,
        vec![
            StoreEvent::Recovered { id },
            StoreEvent::NeedsTrackerStart {
                id,
                bbox: person(0.42, 0.5),
                pts: t(5.0)
            },
        ]
    );
    let rec = s.record(id).unwrap();
    assert!(rec.is_tracking() && rec.lost_since.is_none() && rec.tracker_running);
    // Somebody far away is somebody new.
    s.tick(t(7.0));
    let ev = s.ingest_detections(t(8.0), &[det("person", person(0.9, 0.5))]);
    assert_eq!(created(&ev), vec![SubjectId(2)]);
}

#[test]
fn a_tracking_subject_wins_over_a_lost_one_in_the_same_place() {
    let mut s = store();
    s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.53, 0.5)),
            det("person", person(0.9, 0.5)),
        ],
    );
    let (lost, live) = (SubjectId(1), SubjectId(2));
    s.ingest_tracks(t(1.0), &[track(live, person(0.9, 0.5))]);
    s.tick(t(1.5));
    assert_eq!(s.get(lost).unwrap().state, TrackState::Lost);
    s.reacquire(live, person(0.535, 0.5), t(1.5)).unwrap();
    // The detection overlaps the lost subject a little more (IoU 0.82 vs
    // 0.74), but the tracking subject keeps it.
    let ev = s.ingest_detections(t(1.6), &[det("person", person(0.52, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id: live }]);
    assert_eq!(s.get(lost).unwrap().state, TrackState::Lost);
}

#[test]
fn late_detections_confirm_newer_tracks_and_stale_ones_only_confirm() {
    let mut s = store();
    s.ingest_detections(t(0.0), &[det("person", person(0.3, 0.5))]);
    let id = SubjectId(1);
    s.ingest_tracks(t(1.0), &[track(id, person(0.32, 0.5))]);
    // Detection from t = 0.9 agrees: the newer tracked box is kept.
    let ev = s.ingest_detections(t(0.9), &[det("person", person(0.319, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert!((cx(&s, id) - 0.32).abs() < 1e-9);
    // An operator fix at t = 2 outranks a detection computed at t = 1.5.
    s.reacquire(id, person(0.5, 0.5), t(2.0)).unwrap();
    let ev = s.ingest_detections(t(1.5), &[det("person", person(0.47, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert!((cx(&s, id) - 0.5).abs() < 1e-9);
}

#[test]
fn a_late_detection_that_disagrees_restarts_the_tracker_at_its_own_pts() {
    let mut s = store();
    s.ingest_detections(t(0.0), &[det("person", person(0.3, 0.5))]);
    let id = SubjectId(1);
    // The tracker drifts while the person stands still.
    s.ingest_tracks(t(1.0), &[track(id, person(0.37, 0.5))]);
    let ev = s.ingest_detections(t(0.9), &[det("person", person(0.3, 0.5))]);
    assert_eq!(
        ev,
        vec![
            StoreEvent::Updated { id },
            StoreEvent::NeedsTrackerStart {
                id,
                bbox: person(0.3, 0.5),
                pts: t(0.9)
            },
        ]
    );
    assert!((cx(&s, id) - 0.3).abs() < 1e-9);
    // The restarted tracker reports from the detection's frame onwards.
    assert_eq!(
        s.ingest_tracks(t(0.95), &[track(id, person(0.3, 0.5))])
            .len(),
        1
    );
}

#[test]
fn another_query_on_the_same_box_becomes_an_alias() {
    let mut s = store();
    s.ingest_detections(t(0.0), &[det("person", person(0.4, 0.5))]);
    let id = SubjectId(1);
    let ev = s.ingest_detections(t(0.5), &[det("speaker", person(0.41, 0.5))]);
    assert_eq!(
        ev,
        vec![
            StoreEvent::QueriesChanged { id },
            StoreEvent::Updated { id }
        ]
    );
    assert_eq!(s.len(), 1);
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.query, q("person"));
    assert!(rec.aliases.contains(&q("speaker")));
    // From now on the alias query alone keeps the subject alive.
    for i in 2..=8 {
        let time = 0.5 * i as f64;
        let ev = s.ingest_detections(t(time), &[det("speaker", person(0.41, 0.5))]);
        assert_eq!(ev, vec![StoreEvent::Updated { id }]);
        assert!(s.tick(t(time)).is_empty());
    }
}

#[test]
fn prompts_finding_a_new_entity_together_admit_one_subject() {
    let mut s = store();
    let ev = s.ingest_detections(
        t(0.0),
        &[
            det("speaker", person(0.4, 0.5)),
            det("person", person(0.4, 0.5)),
        ],
    );
    assert_eq!(created(&ev), vec![SubjectId(1)]);
    let rec = s.record(SubjectId(1)).unwrap();
    assert_eq!(rec.subject.query, q("person"));
    assert!(rec.answers(&q("speaker")));
}

#[test]
fn nested_objects_stay_separate_and_duplicate_boxes_collapse() {
    let mut s = store();
    let player = Rect::new(0.3, 0.2, 0.2, 0.6);
    let guitar = Rect::new(0.35, 0.5, 0.1, 0.1);
    let twin = Rect::new(0.301, 0.2, 0.2, 0.6);
    let ev = s.ingest_detections(
        t(0.0),
        &[
            det("person", player),
            det("guitar", guitar),
            det("person", twin),
        ],
    );
    assert_eq!(created(&ev).len(), 2);
    assert!(s.records().all(|r| r.aliases.is_empty()));
}

#[test]
fn capacity_admits_the_best_scores_and_drops_the_rest() {
    let mut s = capped(2);
    let ev = s.ingest_detections(
        t(0.0),
        &[
            scored("person", person(0.1, 0.5), 0.5),
            scored("person", person(0.4, 0.5), 0.95),
            scored("person", person(0.7, 0.5), 0.7),
        ],
    );
    assert_eq!(created(&ev).len(), 2);
    let xs: Vec<f64> = s.subjects().map(|s| s.bbox.center().x).collect();
    assert_eq!(xs.len(), 2);
    assert!((xs[0] - 0.4).abs() < 1e-9 && (xs[1] - 0.7).abs() < 1e-9);
}

#[test]
fn admission_is_round_robin_across_queries() {
    let mut s = capped(3);
    let ev = s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.1, 0.5)),
            det("person", person(0.3, 0.5)),
            det("person", person(0.5, 0.5)),
            det("guitar", person(0.7, 0.2)),
            det("guitar", person(0.9, 0.2)),
        ],
    );
    let queries: Vec<String> = created(&ev)
        .into_iter()
        .map(|id| s.get(id).unwrap().query.0.clone())
        .collect();
    assert_eq!(queries, ["guitar", "person", "guitar"]);
}

#[test]
fn admission_favours_queries_with_fewer_subjects() {
    let mut s = capped(4);
    s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.1, 0.5)),
            det("person", person(0.3, 0.5)),
        ],
    );
    let ev = s.ingest_detections(
        t(0.5),
        &[
            det("person", person(0.1, 0.5)),
            det("person", person(0.3, 0.5)),
            det("person", person(0.5, 0.5)),
            det("drum", person(0.7, 0.2)),
            det("drum", person(0.9, 0.2)),
        ],
    );
    assert_eq!(created(&ev).len(), 2);
    assert!(
        created(&ev)
            .iter()
            .all(|&id| s.get(id).unwrap().query == q("drum"))
    );
}

#[test]
fn at_capacity_the_longest_lost_unretained_subject_is_evicted() {
    let mut s = capped(3);
    s.ingest_detections(t(0.0), &[det("person", person(0.1, 0.5))]);
    s.ingest_detections(t(1.0), &[det("person", person(0.4, 0.5))]);
    s.ingest_detections(t(2.0), &[det("person", person(0.7, 0.5))]);
    let (a, b, c) = (SubjectId(1), SubjectId(2), SubjectId(3));
    // a was lost at 1 s and b at 2 s; c is still tracking.
    s.tick(t(2.9));
    assert_eq!(s.records().filter(|r| !r.is_tracking()).count(), 2);
    // The longest-lost subject is protected, so the next one goes.
    s.set_protected(&HashSet::from([a]));
    let ev = s.ingest_detections(t(2.95), &[det("person", person(0.9, 0.2))]);
    assert_eq!(
        ev[0],
        StoreEvent::Retired {
            id: b,
            reason: RetireReason::Evicted
        }
    );
    let d = SubjectId(4);
    assert_eq!(created(&ev), vec![d]);
    assert_eq!(ids(&s), vec![a, c, d]);
    // Later c (pinned) and d (not retained) are lost too: d is evicted.
    s.set_pinned(c, true).unwrap();
    s.tick(t(10.0));
    let ev = s.ingest_detections(t(10.0), &[det("person", person(0.5, 0.9))]);
    assert_eq!(retired(&ev), vec![d]);
    let e = SubjectId(5);
    assert_eq!(created(&ev), vec![e]);
    // Now only protected, pinned and tracking subjects remain: the
    // newcomer is dropped and nothing is retired.
    let ev = s.ingest_detections(t(10.1), &[det("person", person(0.3, 0.15))]);
    assert!(ev.is_empty(), "{ev:?}");
    assert_eq!(ids(&s), vec![a, c, e]);
}

#[test]
fn identical_inputs_replay_identically() {
    let run = || {
        let mut s = capped(3);
        let mut log = Vec::new();
        for i in 0..40 {
            let time = i as f64 * 0.25;
            let x = (i as f64 * 0.37).sin() * 0.3 + 0.5;
            log.extend(s.ingest_detections(
                t(time),
                &[
                    det("person", person(x, 0.5)),
                    det("guitar", person(1.0 - x, 0.4)),
                ],
            ));
            log.extend(s.tick(t(time)));
        }
        log
    };
    assert_eq!(run(), run());
}
