//! Coasting trackers: a detection applied to a subject whose tracker reported
//! held-back observations and no confident one since the subject's previous
//! applied detection restarts the tracker, as a re-seed does, even when it
//! agrees with the box, unless the tracker's report on the frame the detection
//! was computed on had the match held back overlapping the detection, but no
//! more than the subject's box (a lookalike occluder over part of the
//! subject); one held back clear of the detection restarts it. No observation
//! in that window, only low ones the tracker did not hold back (an occluder
//! over the subject), or a confident one, leaves an agreeing detection a
//! re-anchor, as before the rule.

use ac_core::{Point, Rect, SubjectId};

use crate::record::TrackerReport;
use crate::testkit::*;
use crate::{StoreEvent, SubjectStore, TrackObservation};

/// A track observation below the default `track_min_confidence` (0.5) that
/// the tracker held back, as it reports while its contrast check refuses
/// every match.
fn coasting(id: SubjectId, bbox: Rect) -> TrackObservation {
    TrackObservation {
        confidence: 0.3,
        held: true,
        ..track(id, bbox)
    }
}

/// [`coasting`], with the tracker saying that the match it held back lay at
/// `at`.
fn held_at(id: SubjectId, bbox: Rect, at: Rect) -> TrackObservation {
    TrackObservation {
        held_at: Some(at),
        ..coasting(id, bbox)
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
fn a_tracker_that_coasts_for_want_of_score_is_not_restarted() {
    // Something covers the subject: the match sinks with the visible part,
    // below `accept_score`, and the tracker reports that score without
    // holding the box back for any other reason. A restart would take the
    // template from a frame with the occluder in the box, so the agreeing
    // detection only re-anchors.
    let (mut s, id) = one();
    for time in [0.1, 0.2, 0.3, 0.4] {
        let occluded = TrackObservation {
            confidence: 0.44,
            ..track(id, person(0.3, 0.5))
        };
        assert!(s.ingest_tracks(t(time), &[occluded]).is_empty());
    }
    assert_eq!(s.record(id).unwrap().tracker_report, TrackerReport::Silent);
    let ev = s.ingest_detections(t(0.5), &[det("person", person(0.31, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert_eq!(s.record(id).unwrap().motion.anchor, person(0.31, 0.5));
    // One held observation among them is evidence, and the unheld ones
    // around it neither add to it nor take it away.
    let occluded = TrackObservation {
        confidence: 0.44,
        ..track(id, person(0.31, 0.5))
    };
    s.ingest_tracks(t(0.6), &[occluded]);
    coast(&mut s, id, &[0.7]);
    s.ingest_tracks(t(0.8), &[occluded]);
    assert_eq!(
        s.record(id).unwrap().tracker_report,
        TrackerReport::Coasting
    );
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
fn the_held_match_on_the_detection_s_frame_decides_the_restart() {
    let here = person(0.3, 0.5);
    // Under a spot, the performer sidestepped to where the match the
    // tracker holds back lies; their detection lands there and restarts it.
    let (mut s, id) = one();
    let sidestep = person(0.33, 0.5);
    for time in [0.1, 0.2, 0.3, 0.4, 0.5] {
        assert!(
            s.ingest_tracks(t(time), &[held_at(id, here, sidestep)])
                .is_empty()
        );
    }
    assert_eq!(
        s.record(id).unwrap().tracker_report,
        TrackerReport::Coasting
    );
    let ev = s.ingest_detections(t(0.5), &[det("person", sidestep)]);
    assert_eq!(
        ev,
        vec![StoreEvent::Updated { id }, restart(id, sidestep, 0.5)]
    );
    // A brighter occluder over the subject's left half that looks partly
    // like it drew the match onto itself on the detection's frame, and the
    // subject stayed: its detection only re-anchors, and the window ends
    // all the same. A restart would take the template from the occluded
    // frame.
    let (mut s, id) = one();
    let lookalike = person(0.26, 0.5);
    coast(&mut s, id, &[0.1]);
    for time in [0.2, 0.3, 0.4, 0.5] {
        s.ingest_tracks(t(time), &[held_at(id, here, lookalike)]);
    }
    let ev = s.ingest_detections(t(0.5), &[det("person", person(0.301, 0.5))]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    let rec = s.record(id).unwrap();
    assert_eq!(rec.motion.anchor, person(0.301, 0.5));
    assert_eq!(rec.tracker_report, TrackerReport::Silent);
    // A position on an observation that is not held back is no evidence;
    // an invalid one is no position, which leaves the held window to
    // restart the tracker.
    let unheld = TrackObservation {
        held: false,
        ..held_at(id, here, sidestep)
    };
    s.ingest_tracks(t(0.6), &[unheld]);
    assert_eq!(s.record(id).unwrap().tracker_report, TrackerReport::Silent);
    let nowhere = Rect::new(f64::NAN, 0.0, 0.1, 0.1);
    s.ingest_tracks(t(0.7), &[held_at(id, here, nowhere)]);
    assert_eq!(
        s.record(id).unwrap().tracker_report,
        TrackerReport::Coasting
    );
    let ev = s.ingest_detections(t(1.0), &[det("person", here)]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }, restart(id, here, 1.0)]);
}

#[test]
fn a_match_held_back_clear_of_the_detection_restarts_a_coasting_tracker() {
    // A narrow performer walking under a spot: the match the tracker holds
    // back lies off to the side, clear of their box, and their detection
    // lands where the box was. Nothing that drew the match is in the box the
    // new template would be taken from, so the tracker restarts there.
    let here = person(0.3, 0.5);
    let clear = person(0.45, 0.5);
    assert_eq!(clear.iou(&here), 0.0);
    let (mut s, id) = one();
    for time in [0.1, 0.2, 0.3, 0.4, 0.5] {
        let ob = held_at(id, here, clear);
        assert!(s.ingest_tracks(t(time), &[ob]).is_empty());
    }
    let ev = s.ingest_detections(t(0.5), &[det("person", here)]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }, restart(id, here, 0.5)]);
}

#[test]
fn a_late_pass_from_a_frame_the_tracker_trusted_restarts_a_coasting_tracker() {
    // Passes every half second, applied half a second late. The tracker
    // trusts its match up to t = 1.0, the performer sidesteps at 1.05
    // under the spot, and from then on the tracker holds back the match on
    // them. The pass from 0.5, applied after the frame at 1.0, closes a
    // window of trusted frames and only moves the anchor.
    let here = person(0.3, 0.5);
    let sidestep = person(0.33, 0.5);
    let (mut s, id) = one();
    for i in 1..=10 {
        s.ingest_tracks(t(f64::from(i) / 10.0), &[track(id, here)]);
    }
    let ev = s.ingest_detections(t(0.5), &[det("person", here)]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    for i in 11..=15 {
        let ob = held_at(id, here, sidestep);
        assert!(s.ingest_tracks(t(f64::from(i) / 10.0), &[ob]).is_empty());
    }
    // The pass from 1.0, applied after the frame at 1.5, closes a window of
    // held frames only. It boxes the performer where they stood on its
    // frame, which the tracker trusted, so it restarts the tracker there,
    // although every match held back since lay on the new spot.
    let ev = s.ingest_detections(t(1.0), &[det("person", here)]);
    assert_eq!(ev, vec![StoreEvent::Updated { id }, restart(id, here, 1.0)]);
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
    // tracker is held back for a second at a time.
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
                        held: confidence < 0.5,
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
