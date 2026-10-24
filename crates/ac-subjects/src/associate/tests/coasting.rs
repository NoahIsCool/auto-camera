//! Coasting trackers: a detection applied to a subject whose tracker coasted
//! on every observation it reported since the subject's previous applied
//! detection restarts the tracker, as a re-seed does, even when it agrees
//! with the box. No observation in that window, or a confident one, leaves
//! an agreeing detection a re-anchor, as before the rule.

use ac_core::{Point, Rect, SubjectId};

use crate::record::TrackerReport;
use crate::testkit::*;
use crate::{StoreEvent, SubjectStore, TrackObservation};

/// A track observation below the default `track_min_confidence` (0.5), as a
/// tracker reports while it coasts.
fn coasting(id: SubjectId, bbox: Rect) -> TrackObservation {
    TrackObservation {
        confidence: 0.3,
        ..track(id, bbox)
    }
}

/// The restart a re-seed asks for.
fn restart(id: SubjectId, bbox: Rect, pts: f64) -> StoreEvent {
    StoreEvent::NeedsTrackerStart {
        id,
        bbox,
        pts: t(pts),
    }
}

/// A store holding one subject (id 1) admitted at `person(0.3, 0.5)` at
/// t = 0.
fn one() -> (SubjectStore, SubjectId) {
    let mut s = store();
    s.ingest_detections(t(0.0), &[det("person", person(0.3, 0.5))]);
    (s, SubjectId(1))
}

/// Feeds `id`'s tracker coasting at `person(0.3, 0.5)` at each time in
/// `times`, checking that the store leaves the box alone.
fn coast(s: &mut SubjectStore, id: SubjectId, times: &[f64]) {
    for &time in times {
        let ev = s.ingest_tracks(t(time), &[coasting(id, person(0.3, 0.5))]);
        assert!(ev.is_empty(), "t = {time}: {ev:?}");
    }
}

#[test]
fn an_agreeing_detection_restarts_a_tracker_that_coasted_since_the_last_one() {
    let (mut s, id) = one();
    // The performer sidesteps at t = 0.05 and the tracker coasts where they
    // were. The store ignores the box and the time of every observation.
    coast(&mut s, id, &[0.1, 0.2, 0.3, 0.4]);
    let rec = s.record(id).unwrap();
    assert_eq!(rec.last_seen, t(0.0));
    assert_eq!(rec.tracker_report, TrackerReport::Coasting);
    // The detection overlaps the box at IoU 0.54, above `reseed_iou`, so
    // without the rule it would only re-anchor the box.
    let sidestep = person(0.33, 0.5);
    assert!(sidestep.iou(&person(0.3, 0.5)) >= s.config().reseed_iou);
    let ev = s.ingest_detections(t(0.5), &[det("person", sidestep)]);
    assert_eq!(
        ev,
        vec![StoreEvent::Updated { id }, restart(id, sidestep, 0.5)]
    );
    // The box handling is a re-seed's: box and anchor at the detection, no
    // velocity from the jump, and a fresh window.
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.bbox, sidestep);
    assert_eq!(rec.motion.anchor, sidestep);
    assert_eq!(
        (rec.motion.box_pts, rec.motion.anchor_pts),
        (t(0.5), t(0.5))
    );
    assert_eq!(rec.velocity(), Point::default());
    assert!(rec.tracker_running && rec.is_tracking());
    assert_eq!(rec.tracker_report, TrackerReport::Silent);
    // The restarted tracker is followed at once.
    let ev = s.ingest_tracks(t(0.53), &[track(id, person(0.331, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
}

#[test]
fn a_confident_observation_since_the_last_detection_keeps_an_agreeing_one_a_reanchor() {
    let near = person(0.31, 0.5);
    // Confident first, then coasting; and coasting first, then confident.
    for confident_at in [0.1, 0.4] {
        let (mut s, id) = one();
        for time in [0.1, 0.2, 0.3, 0.4] {
            let ob = if time == confident_at {
                track(id, near)
            } else {
                coasting(id, near)
            };
            s.ingest_tracks(t(time), &[ob]);
        }
        let rec = s.record(id).unwrap();
        assert_eq!(rec.tracker_report, TrackerReport::Following);
        let ev = s.ingest_detections(t(0.5), &[det("person", person(0.32, 0.5))]);
        assert_eq!(ev, vec![StoreEvent::Updated { id }], "{confident_at}");
        let rec = s.record(id).unwrap();
        assert_eq!(rec.motion.anchor, person(0.32, 0.5));
        assert_eq!(rec.tracker_report, TrackerReport::Silent);
    }
}

#[test]
fn each_applied_detection_starts_a_new_window() {
    let (mut s, id) = one();
    s.ingest_tracks(t(0.1), &[track(id, person(0.3, 0.5))]);
    coast(&mut s, id, &[0.2, 0.3, 0.4]);
    // Not coasting since the admission: the tracker was confident at 0.1.
    let ev = s.ingest_detections(t(0.5), &[det("person", person(0.31, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    // Coasting on everything since that detection.
    coast(&mut s, id, &[0.6, 0.7, 0.8, 0.9]);
    let ev = s.ingest_detections(t(1.0), &[det("person", person(0.31, 0.5))]);
    assert_eq!(
        ev,
        vec![
            StoreEvent::Updated { id },
            restart(id, person(0.31, 0.5), 1.0)
        ]
    );
}

#[test]
fn without_observations_an_agreeing_detection_only_reanchors() {
    let (mut s, id) = one();
    for (time, x) in [(0.5, 0.31), (1.0, 0.32), (1.5, 0.33)] {
        let ev = s.ingest_detections(t(time), &[det("person", person(x, 0.5))]);
        assert_eq!(ev, vec![StoreEvent::Updated { id }], "t = {time}");
        assert_eq!(s.record(id).unwrap().tracker_report, TrackerReport::Silent);
    }
    // A failed start: the next detection retries it, as before, and the
    // observations of a tracker that does not run are no evidence.
    s.tracker_failed(id);
    coast(&mut s, id, &[1.6, 1.7]);
    assert_eq!(s.record(id).unwrap().tracker_report, TrackerReport::Silent);
    let ev = s.ingest_detections(t(2.0), &[det("person", person(0.33, 0.5))]);
    assert_eq!(
        ev,
        vec![
            StoreEvent::Updated { id },
            restart(id, person(0.33, 0.5), 2.0)
        ]
    );
    let ev = s.ingest_detections(t(2.5), &[det("person", person(0.33, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
}

#[test]
fn unusable_observations_are_no_coasting_evidence() {
    let (mut s, id) = one();
    s.ingest_detections(t(0.5), &[det("person", person(0.3, 0.5))]);
    let unusable = [
        // Older than the box, which the detection at 0.5 set.
        (0.4, coasting(id, person(0.3, 0.5))),
        // A NaN confidence is neither confident nor below the threshold.
        (
            0.6,
            TrackObservation {
                confidence: f32::NAN,
                ..track(id, person(0.3, 0.5))
            },
        ),
        // A degenerate box, and another subject's id.
        (0.7, coasting(id, Rect::new(f64::NAN, 0.0, 0.1, 0.1))),
        (0.8, coasting(SubjectId(9), person(0.3, 0.5))),
    ];
    for (time, ob) in unusable {
        assert!(s.ingest_tracks(t(time), &[ob]).is_empty());
    }
    assert_eq!(s.record(id).unwrap().tracker_report, TrackerReport::Silent);
    let ev = s.ingest_detections(t(1.0), &[det("person", person(0.31, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
}

#[test]
fn a_late_detection_restarts_a_coasting_tracker_at_its_own_pts() {
    let (mut s, id) = one();
    s.ingest_tracks(t(0.4), &[track(id, person(0.31, 0.5))]);
    // A late pass from t = 0.3 agrees with the newer tracked box, which it
    // keeps; it only moves the anchor.
    let ev = s.ingest_detections(t(0.3), &[det("person", person(0.3, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert_eq!(s.record(id).unwrap().motion.box_pts, t(0.4));
    // The tracker coasts from then on, and the next pass is late too: older
    // than the box, not older than the anchor. Like a late re-seed it
    // restarts the tracker on its own frame.
    coast(&mut s, id, &[0.5, 0.6, 0.7]);
    let ev = s.ingest_detections(t(0.35), &[det("person", person(0.32, 0.5))]);
    assert_eq!(
        ev,
        vec![
            StoreEvent::Updated { id },
            restart(id, person(0.32, 0.5), 0.35)
        ]
    );
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.bbox, person(0.32, 0.5));
    assert_eq!(rec.motion.box_pts, t(0.35));
    // The restarted tracker reports from the detection's frame onwards.
    let ev = s.ingest_tracks(t(0.4), &[track(id, person(0.32, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
}

#[test]
fn a_detection_older_than_the_anchor_leaves_the_evidence_to_the_next_one() {
    let (mut s, id) = one();
    s.ingest_detections(t(0.5), &[det("person", person(0.3, 0.5))]);
    coast(&mut s, id, &[0.6, 0.7]);
    // Out of order, from before the anchor: it only confirms the subject.
    let ev = s.ingest_detections(t(0.4), &[det("person", person(0.31, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.bbox, person(0.3, 0.5));
    assert_eq!(rec.tracker_report, TrackerReport::Coasting);
    coast(&mut s, id, &[0.8, 0.9]);
    let ev = s.ingest_detections(t(1.0), &[det("person", person(0.31, 0.5))]);
    assert_eq!(
        ev,
        vec![
            StoreEvent::Updated { id },
            restart(id, person(0.31, 0.5), 1.0)
        ]
    );
}

#[test]
fn a_tracker_restart_or_a_loss_ends_the_window() {
    // An operator reacquire restarts the tracker: the coasting before it
    // does not count against the new one.
    let (mut s, id) = one();
    coast(&mut s, id, &[0.1, 0.2]);
    s.reacquire(id, person(0.3, 0.5), t(0.3)).unwrap();
    assert_eq!(s.record(id).unwrap().tracker_report, TrackerReport::Silent);
    let ev = s.ingest_detections(t(0.5), &[det("person", person(0.31, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    // A coasting tracker keeps no subject alive: it is lost a second after
    // the last confident observation (here the detection), its tracker
    // stops, and the detection that recovers it restarts the tracker.
    coast(&mut s, id, &[0.6, 1.0, 1.4]);
    let ev = s.tick(t(1.5));
    assert_eq!(
        ev,
        vec![StoreEvent::Lost { id }, StoreEvent::NeedsTrackerStop { id }]
    );
    assert_eq!(s.record(id).unwrap().tracker_report, TrackerReport::Silent);
    let ev = s.ingest_detections(t(2.0), &[det("person", person(0.31, 0.5))]);
    assert_eq!(
        ev,
        vec![
            StoreEvent::Recovered { id },
            restart(id, person(0.31, 0.5), 2.0)
        ]
    );
    let ev = s.ingest_detections(t(2.5), &[det("person", person(0.31, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
}

#[test]
fn coasting_restarts_replay_identically() {
    // Two subjects at 2 Hz detections and 30 Hz tracks; the second one's
    // tracker coasts for a second at a time.
    let run = || {
        let mut s = store();
        let mut log = Vec::new();
        for frame in 0..180u32 {
            let time = f64::from(frame) / 30.0;
            let x = 0.6 + 0.05 * (time * 0.7).sin();
            if frame > 0 {
                let confidence = if (frame / 30) % 2 == 1 { 0.3 } else { 0.9 };
                let obs = [
                    track(SubjectId(1), person(0.3, 0.5)),
                    TrackObservation {
                        confidence,
                        ..track(SubjectId(2), person(x, 0.5))
                    },
                ];
                log.extend(s.ingest_tracks(t(time), &obs));
            }
            if frame % 15 == 0 {
                let dets = [
                    det("person", person(0.3, 0.5)),
                    det("person", person(x, 0.5)),
                ];
                log.extend(s.ingest_detections(t(time), &dets));
            }
            log.extend(s.tick(t(time)));
        }
        log
    };
    let log = run();
    assert_eq!(log, run());
    // Admission starts each tracker once; every later start is the rule's,
    // for the coasting subject only.
    let starts = |id: SubjectId| {
        log.iter()
            .filter(|e| matches!(e, StoreEvent::NeedsTrackerStart { id: i, .. } if *i == id))
            .count()
    };
    assert_eq!(starts(SubjectId(1)), 1);
    assert!(starts(SubjectId(2)) > 1, "{} starts", starts(SubjectId(2)));
}
