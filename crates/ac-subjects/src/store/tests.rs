//! Lifecycle tests: tracks, time-based transitions and operator commands.

use std::collections::HashSet;

use ac_core::{Rect, SourceId, SubjectId, TrackState};

use crate::testkit::*;
use crate::{RetireReason, StoreConfig, StoreError, StoreEvent, SubjectStore, TrackObservation};

/// A store holding one tracking subject (id 1) at `person(0.3, 0.5)`, created
/// at t = 0.
fn one() -> (SubjectStore, SubjectId) {
    let mut s = store();
    let ev = s.ingest_detections(t(0.0), &[det("person", person(0.3, 0.5))]);
    (s, created(&ev)[0])
}

#[test]
fn invalid_config_is_refused() {
    let config = StoreConfig {
        max_subjects: 0,
        ..StoreConfig::default()
    };
    let err = SubjectStore::new(SourceId("cam".into()), config).unwrap_err();
    assert!(matches!(err, StoreError::InvalidConfig(_)));
}

#[test]
fn new_store_is_empty_and_ids_start_at_one() {
    let s = store();
    assert!(s.is_empty());
    assert_eq!(s.source(), &SourceId("cam".into()));
    let (s, id) = one();
    assert_eq!(id, SubjectId(1));
    assert_eq!(s.len(), 1);
    let rec = s.record(id).unwrap();
    assert!(rec.tracker_running && rec.is_tracking());
    assert_eq!(rec.subject.source, SourceId("cam".into()));
}

#[test]
fn tracks_move_the_box_between_detections() {
    let (mut s, id) = one();
    let ev = s.ingest_tracks(t(0.1), &[track(id, person(0.32, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert!((cx(&s, id) - 0.32).abs() < 1e-9);
    assert_eq!(s.record(id).unwrap().last_seen, t(0.1));
}

#[test]
fn unusable_tracks_are_ignored() {
    let (mut s, id) = one();
    s.ingest_tracks(t(0.2), &[track(id, person(0.31, 0.5))]);
    let ignored = [
        // Low and NaN confidence.
        TrackObservation {
            confidence: 0.1,
            ..track(id, person(0.5, 0.5))
        },
        TrackObservation {
            confidence: f32::NAN,
            ..track(id, person(0.5, 0.5))
        },
        // Unknown subject and degenerate box.
        track(SubjectId(99), person(0.5, 0.5)),
        track(id, Rect::new(f64::NAN, 0.0, 0.1, 0.1)),
    ];
    assert!(s.ingest_tracks(t(0.3), &ignored).is_empty());
    // Older than the current box.
    assert!(
        s.ingest_tracks(t(0.1), &[track(id, person(0.5, 0.5))])
            .is_empty()
    );
    assert!((cx(&s, id) - 0.31).abs() < 1e-9);
}

#[test]
fn one_update_event_per_subject_per_batch() {
    let (mut s, id) = one();
    let obs = [track(id, person(0.31, 0.5)), track(id, person(0.32, 0.5))];
    assert_eq!(s.ingest_tracks(t(0.1), &obs).len(), 1);
    assert!((cx(&s, id) - 0.32).abs() < 1e-9);
}

#[test]
fn subject_is_lost_after_a_second_without_confident_observations() {
    let (mut s, id) = one();
    s.ingest_tracks(t(0.5), &[track(id, person(0.3, 0.5))]);
    assert!(s.tick(t(1.4)).is_empty());
    let ev = s.tick(t(1.6));
    assert_eq!(
        ev,
        vec![StoreEvent::Lost { id }, StoreEvent::NeedsTrackerStop { id }]
    );
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.state, TrackState::Lost);
    assert_eq!(rec.lost_since, Some(t(1.5)));
    assert!(!rec.tracker_running);
    // Tracks cannot revive it.
    assert!(
        s.ingest_tracks(t(1.7), &[track(id, person(0.3, 0.5))])
            .is_empty()
    );
    assert!(s.tick(t(1.8)).is_empty());
}

#[test]
fn lost_subject_retires_after_thirty_seconds() {
    let (mut s, id) = one();
    s.tick(t(1.0));
    assert!(s.tick(t(30.9)).is_empty());
    let ev = s.tick(t(31.0));
    assert_eq!(
        ev,
        vec![StoreEvent::Retired {
            id,
            reason: RetireReason::Expired
        }]
    );
    assert!(s.is_empty() && s.get(id).is_none());
}

#[test]
fn a_late_tick_reports_lost_and_retired_together() {
    let (mut s, id) = one();
    let ev = s.tick(t(100.0));
    assert_eq!(ev.len(), 3);
    assert_eq!(ev[0], StoreEvent::Lost { id });
    assert_eq!(retired(&ev), vec![id]);
}

#[test]
fn protected_pinned_and_hidden_subjects_are_never_expired() {
    for flag in 0..3 {
        let (mut s, id) = one();
        match flag {
            0 => s.set_protected(&HashSet::from([id])),
            1 => s.set_pinned(id, true).unwrap(),
            _ => s.set_hidden(id, true).unwrap(),
        }
        let ev = s.tick(t(1000.0));
        assert_eq!(ev[0], StoreEvent::Lost { id });
        assert!(retired(&ev).is_empty(), "flag {flag}");
        assert!(s.record(id).unwrap().is_retained());
    }
}

#[test]
fn releasing_protection_lets_an_old_lost_subject_retire() {
    let (mut s, id) = one();
    s.set_protected(&HashSet::from([id]));
    s.tick(t(100.0));
    s.set_protected(&HashSet::new());
    assert!(!s.record(id).unwrap().protected);
    assert_eq!(retired(&s.tick(t(100.1))), vec![id]);
}

#[test]
fn operator_flags_on_unknown_subjects_are_errors() {
    let mut s = store();
    let ghost = SubjectId(5);
    assert_eq!(
        s.set_pinned(ghost, true),
        Err(StoreError::UnknownSubject(ghost))
    );
    assert_eq!(
        s.set_hidden(ghost, true),
        Err(StoreError::UnknownSubject(ghost))
    );
    // Unknown ids in the protected set are simply ignored.
    s.set_protected(&HashSet::from([ghost]));
    assert!(s.is_empty());
}

#[test]
fn hidden_subjects_leave_the_visible_list_only() {
    let mut s = store();
    s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.2, 0.5)),
            det("person", person(0.6, 0.5)),
        ],
    );
    s.set_hidden(SubjectId(1), true).unwrap();
    let visible: Vec<SubjectId> = s.visible().map(|r| r.id()).collect();
    assert_eq!(visible, vec![SubjectId(2)]);
    assert_eq!(ids(&s), vec![SubjectId(1), SubjectId(2)]);
    assert_eq!(s.records().count(), 2);
}

#[test]
fn reacquire_keeps_the_id_and_restarts_the_tracker() {
    let (mut s, id) = one();
    s.tick(t(2.0));
    let fix = person(0.7, 0.5);
    let ev = s.reacquire(id, fix, t(2.5)).unwrap();
    assert_eq!(
        ev,
        vec![
            StoreEvent::Recovered { id },
            StoreEvent::NeedsTrackerStart {
                id,
                bbox: fix,
                pts: t(2.5)
            },
        ]
    );
    let rec = s.record(id).unwrap();
    assert!(rec.is_tracking() && rec.tracker_running && rec.lost_since.is_none());
    assert_eq!(rec.subject.bbox, fix);
    assert_eq!(s.len(), 1);
    // A tracking subject can be corrected too.
    let ev = s.reacquire(id, person(0.6, 0.5), t(2.6)).unwrap();
    assert_eq!(ev[0], StoreEvent::Updated { id });
}

#[test]
fn reacquire_validates_its_input() {
    let (mut s, id) = one();
    let bad = Rect::new(0.2, 0.2, 0.0, 0.1);
    assert_eq!(
        s.reacquire(id, bad, t(1.0)),
        Err(StoreError::InvalidBox(bad))
    );
    assert_eq!(
        s.reacquire(SubjectId(9), person(0.5, 0.5), t(1.0)),
        Err(StoreError::UnknownSubject(SubjectId(9)))
    );
}

#[test]
fn reacquire_demotes_an_unprotected_duplicate() {
    let mut s = store();
    s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.2, 0.5)),
            det("person", person(0.6, 0.5)),
        ],
    );
    let (a, b) = (SubjectId(1), SubjectId(2));
    // The operator says the person tracked as `b` is really `a`.
    let ev = s.reacquire(a, person(0.6, 0.5), t(0.5)).unwrap();
    assert!(ev.contains(&StoreEvent::Lost { id: b }));
    assert!(ev.contains(&StoreEvent::NeedsTrackerStop { id: b }));
    assert!(s.record(a).unwrap().is_tracking());
    // A protected duplicate is left alone.
    s.reacquire(b, person(0.6, 0.5), t(0.6)).unwrap();
    s.set_protected(&HashSet::from([b]));
    let ev = s.reacquire(a, person(0.6, 0.5), t(0.7)).unwrap();
    assert!(!ev.contains(&StoreEvent::Lost { id: b }));
}

#[test]
fn remove_query_retires_its_subjects_and_keeps_others() {
    let mut s = store();
    s.ingest_detections(
        t(0.0),
        &[
            det("person", person(0.2, 0.5)),
            det("guitar", person(0.7, 0.5)),
        ],
    );
    // Equal counts admit in query order: "guitar" (1) before "person" (2).
    let (guitar, person_id) = (SubjectId(1), SubjectId(2));
    assert_eq!(s.get(person_id).unwrap().query, q("person"));
    s.set_pinned(person_id, true).unwrap();
    let ev = s.remove_query(&q("person"));
    assert_eq!(
        ev,
        vec![
            StoreEvent::NeedsTrackerStop { id: person_id },
            StoreEvent::Retired {
                id: person_id,
                reason: RetireReason::QueryRemoved
            },
        ]
    );
    assert_eq!(ids(&s), vec![guitar]);
    assert!(s.remove_query(&q("unknown")).is_empty());
}

#[test]
fn remove_query_promotes_an_alias() {
    let mut s = store();
    s.ingest_detections(t(0.0), &[det("person", person(0.4, 0.5))]);
    s.ingest_detections(t(0.5), &[det("speaker", person(0.4, 0.5))]);
    let id = SubjectId(1);
    assert!(s.record(id).unwrap().answers(&q("speaker")));
    let ev = s.remove_query(&q("person"));
    assert_eq!(ev, vec![StoreEvent::QueriesChanged { id }]);
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.query, q("speaker"));
    assert!(rec.aliases.is_empty());
    // Removing an alias only drops it.
    s.ingest_detections(t(0.6), &[det("pastor", person(0.4, 0.5))]);
    assert_eq!(
        s.remove_query(&q("pastor")),
        vec![StoreEvent::QueriesChanged { id }]
    );
    assert_eq!(s.len(), 1);
}

#[test]
fn removing_the_query_of_an_on_air_subject_defers_its_retirement() {
    let (mut s, id) = one();
    s.set_protected(&HashSet::from([id]));
    assert!(s.remove_query(&q("person")).is_empty());
    let rec = s.record(id).unwrap();
    assert!(rec.orphaned && rec.tracker_running);
    // It keeps following its tracker for the on-air shot ...
    assert_eq!(
        s.ingest_tracks(t(0.1), &[track(id, person(0.31, 0.5))])
            .len(),
        1
    );
    // ... but no longer matches detections: a new subject is admitted.
    let ev = s.ingest_detections(t(0.2), &[det("person", person(0.6, 0.5))]);
    assert_eq!(created(&ev), vec![SubjectId(2)]);
    assert!(retired(&s.tick(t(0.3))).is_empty());
    s.set_protected(&HashSet::new());
    let ev = s.tick(t(0.4));
    assert_eq!(
        ev,
        vec![
            StoreEvent::NeedsTrackerStop { id },
            StoreEvent::Retired {
                id,
                reason: RetireReason::QueryRemoved
            },
        ]
    );
}

#[test]
fn ids_are_never_reused_after_retirement() {
    let (mut s, id) = one();
    s.tick(t(100.0));
    assert!(s.is_empty());
    let ev = s.ingest_detections(t(101.0), &[det("person", person(0.3, 0.5))]);
    assert_eq!(created(&ev), vec![SubjectId(id.0 + 1)]);
}

#[test]
fn admission_stops_when_ids_are_exhausted() {
    let mut s = store_with(StoreConfig {
        max_subjects: 1,
        ..StoreConfig::default()
    });
    s.next_id = u64::from(u32::MAX);
    let ev = s.ingest_detections(t(0.0), &[det("person", person(0.3, 0.5))]);
    assert_eq!(created(&ev), vec![SubjectId(u32::MAX)]);
    s.tick(t(2.0));
    // At capacity with an evictable subject, but no id to give the newcomer:
    // nothing is evicted for nothing.
    let ev = s.ingest_detections(t(2.1), &[det("person", person(0.8, 0.5))]);
    assert!(ev.is_empty(), "{ev:?}");
    assert_eq!(s.len(), 1);
}

#[test]
fn a_failed_tracker_start_is_retried_by_the_next_matching_detection() {
    let (mut s, id) = one();
    s.tracker_failed(id);
    let rec = s.record(id).unwrap();
    assert!(rec.is_tracking() && !rec.tracker_running);
    // Without a running tracker, track observations are ignored ...
    assert!(
        s.ingest_tracks(t(0.1), &[track(id, person(0.31, 0.5))])
            .is_empty()
    );
    // ... and a detection that agrees with the prediction restarts it
    // instead of only re-anchoring the box.
    let bbox = person(0.3, 0.5);
    let ev = s.ingest_detections(t(0.5), &[det("person", bbox)]);
    assert!(
        ev.contains(&StoreEvent::NeedsTrackerStart {
            id,
            bbox,
            pts: t(0.5)
        }),
        "{ev:?}"
    );
    assert!(s.record(id).unwrap().tracker_running);
    // A running tracker is not restarted by the next agreeing detection.
    let ev = s.ingest_detections(t(1.0), &[det("person", bbox)]);
    assert!(
        !ev.iter()
            .any(|e| matches!(e, StoreEvent::NeedsTrackerStart { .. })),
        "{ev:?}"
    );
}

#[test]
fn a_lost_subject_whose_tracker_failed_asks_for_no_stop() {
    let (mut s, id) = one();
    s.tracker_failed(id);
    // Unknown ids are ignored.
    s.tracker_failed(SubjectId(42));
    let ev = s.tick(t(5.0));
    assert!(ev.contains(&StoreEvent::Lost { id }), "{ev:?}");
    assert!(
        !ev.contains(&StoreEvent::NeedsTrackerStop { id }),
        "nothing runs, so nothing is stopped: {ev:?}"
    );
}
