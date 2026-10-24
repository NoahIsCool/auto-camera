//! Shot-label tests: an alias continues its subject from the first sighting,
//! but frames shots only once it has been seen in enough observing passes,
//! and stops after enough observing passes without it.

use std::collections::{BTreeMap, HashSet};

use ac_core::{Point, QueryId, Rect, SubjectId, TrackState};

use crate::testkit::*;
use crate::{Detection, LabelEvidence, StoreEvent, SubjectStore};

/// The queries subject `id` frames shots for, by name.
fn shots(s: &SubjectStore, id: SubjectId) -> Vec<&str> {
    let rec = s.record(id).expect("live subject");
    rec.shot_queries().map(|q| q.0.as_str()).collect()
}

/// Subject `id`'s evidence for `query`.
fn evidence(s: &SubjectStore, id: SubjectId, query: &str) -> Option<LabelEvidence> {
    s.record(id)
        .expect("live subject")
        .labels
        .get(&q(query))
        .copied()
}

/// One pass at `time`, then a tick at the same time.
fn pass(s: &mut SubjectStore, time: f64, dets: &[Detection]) -> Vec<StoreEvent> {
    let mut ev = s.ingest_detections(t(time), dets);
    ev.extend(s.tick(t(time)));
    ev
}

/// A store whose subject 1 answers "speaker" (primary) and "guitarist" (an
/// alias that frames shots), at `bbox`, after passes at 0, 0.5 and 1.0 s.
fn confirmed(bbox: Rect) -> SubjectStore {
    let mut s = store();
    pass(&mut s, 0.0, &[det("speaker", bbox)]);
    for time in [0.5, 1.0] {
        pass(
            &mut s,
            time,
            &[det("speaker", bbox), det("guitarist", bbox)],
        );
    }
    assert_eq!(shots(&s, SubjectId(1)), ["speaker", "guitarist"]);
    s
}

/// A guitarist boxed by both prompts and an audience member boxed by the
/// speaker prompt only; one pass hands the guitarist box to the audience
/// member (people-first matching picked the wrong holder). The audience
/// member answers "guitarist" from then on, so a later guitarist box there
/// stays theirs, but never frames a guitarist shot.
#[test]
fn one_stray_pass_frames_no_shot() {
    let mut s = store();
    let (g, a) = (person(0.3, 0.5), person(0.7, 0.5));
    let normal = [det("guitarist", g), det("speaker", g), det("speaker", a)];
    let mut time = 0.0;
    while time < 10.0 {
        pass(&mut s, time, &normal);
        time += 0.5;
    }
    // Admission is fair across prompts: "guitarist" sorts first and takes
    // the guitarist, whose speaker box aliases on; the audience member is
    // the speaker prompt's.
    let (guitarist, audience) = (SubjectId(1), SubjectId(2));
    assert_eq!(shots(&s, guitarist), ["guitarist", "speaker"]);
    assert_eq!(shots(&s, audience), ["speaker"]);
    let stray = [det("speaker", g), det("speaker", a), det("guitarist", a)];
    let ev = pass(&mut s, time, &stray);
    assert!(ev.contains(&StoreEvent::QueriesChanged { id: audience }));
    assert!(s.record(audience).unwrap().answers(&q("guitarist")));
    assert_eq!(shots(&s, audience), ["speaker"]);
    while time < 70.0 {
        time += 0.5;
        pass(&mut s, time, &normal);
        assert_eq!(shots(&s, audience), ["speaker"], "at {time} s");
        assert_eq!(shots(&s, guitarist), ["guitarist", "speaker"]);
    }
    assert_eq!(s.len(), 2);
    assert!(s.record(audience).unwrap().answers(&q("guitarist")));
}

/// An alias attached in one pass frames shots after a second observing
/// pass sees it, without a second `QueriesChanged`.
#[test]
fn a_label_seen_in_two_passes_frames_shots() {
    let mut s = store();
    let a = person(0.4, 0.5);
    let id = SubjectId(1);
    pass(&mut s, 0.0, &[det("speaker", a)]);
    let both = [det("speaker", a), det("guitarist", a)];
    let ev = pass(&mut s, 0.5, &both);
    assert!(ev.contains(&StoreEvent::QueriesChanged { id }));
    assert_eq!(shots(&s, id), ["speaker"]);
    let ev = pass(&mut s, 1.0, &both);
    assert!(!ev.contains(&StoreEvent::QueriesChanged { id }));
    assert_eq!(shots(&s, id), ["speaker", "guitarist"]);
    let e = evidence(&s, id, "guitarist").unwrap();
    assert_eq!((e.sightings, e.missed, e.frames), (2, 0, true));
    assert_eq!(s.len(), 1);
}

/// A confirmed alias frames through `stale - 1` missed passes, stops at the
/// `stale`-th, keeps continuing the subject, and frames again after two new
/// sightings.
#[test]
fn a_label_missed_for_stale_observing_passes_stops_framing_and_needs_two_sightings_to_return() {
    let a = person(0.4, 0.5);
    let mut s = confirmed(a);
    let id = SubjectId(1);
    let stale = s.config().alias_stale_passes;
    let mut time = 1.0;
    for missed in 1..stale {
        time += 0.5;
        pass(&mut s, time, &[det("speaker", a)]);
        assert_eq!(shots(&s, id), ["speaker", "guitarist"], "{missed} missed");
        assert_eq!(evidence(&s, id, "guitarist").unwrap().missed, missed);
    }
    time += 0.5;
    pass(&mut s, time, &[det("speaker", a)]);
    assert_eq!(shots(&s, id), ["speaker"]);
    let e = evidence(&s, id, "guitarist").unwrap();
    assert_eq!((e.sightings, e.missed, e.frames), (0, stale, false));
    // Identity is untouched: the alias still continues the subject.
    assert!(s.record(id).unwrap().answers(&q("guitarist")));
    let both = [det("speaker", a), det("guitarist", a)];
    time += 0.5;
    pass(&mut s, time, &both);
    assert_eq!(shots(&s, id), ["speaker"], "one sighting is not enough");
    time += 0.5;
    pass(&mut s, time, &both);
    assert_eq!(shots(&s, id), ["speaker", "guitarist"]);
}

/// A guitarist walks off for 15 s (lost, not retired) while the detector
/// keeps passing over someone else, and comes back in a pass that sees only
/// the speaker box (the guitar is still past the edge of the picture). The
/// gap observed nothing of them, so it costs nothing, and the recovery pass
/// costs the guitarist label one miss.
#[test]
fn recovery_after_a_loss_keeps_shot_labels() {
    let a = person(0.3, 0.5);
    let other = person(0.8, 0.5);
    let mut s = confirmed(a);
    let id = SubjectId(1);
    let before = s.record(id).unwrap().labels.clone();
    let mut time = 1.0;
    for _ in 0..15 {
        for _ in 0..30 {
            time += 1.0 / 30.0;
            s.tick(t(time));
        }
        s.ingest_detections(t(time), &[det("speaker", other)]);
    }
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.state, TrackState::Lost);
    assert_eq!(
        rec.labels, before,
        "passes that did not see it count nothing"
    );
    let ev = s.ingest_detections(t(time + 0.1), &[det("speaker", a)]);
    assert!(ev.contains(&StoreEvent::Recovered { id }));
    assert_eq!(shots(&s, id), ["speaker", "guitarist"]);
    assert_eq!(evidence(&s, id, "guitarist").unwrap().missed, 1);
    assert_eq!(s.record(id).unwrap().subject.query, q("speaker"));
}

/// A shared sidecar reaches this camera every 4 s. The guitarist label is
/// missed in three passes in a row, 16 s between two sightings (more than a
/// 10 s label lifetime would allow), and keeps framing: misses are counted
/// in passes.
#[test]
fn slow_cadence_misses_are_counted_in_passes() {
    let mut s = store();
    let a = person(0.4, 0.5);
    let id = SubjectId(1);
    s.ingest_detections(t(0.0), &[det("speaker", a)]);
    let hits = [true, true, true, false, false, false, true, true];
    let mut time = 0.0;
    for (k, &hit) in hits.iter().enumerate() {
        // Ticks in between: the subject is lost before each pass recovers it.
        for _ in 0..4 {
            time += 1.0;
            s.tick(t(time));
        }
        let mut dets = vec![det("speaker", a)];
        if hit {
            dets.push(det("guitarist", a));
        }
        s.ingest_detections(t(time), &dets);
        let want: &[&str] = if k == 0 {
            &["speaker"]
        } else {
            &["speaker", "guitarist"]
        };
        assert_eq!(shots(&s, id), want, "pass {k} at {time} s");
        if k == 5 {
            assert_eq!(evidence(&s, id, "guitarist").unwrap().missed, 3);
        }
    }
    assert_eq!(s.len(), 1);
}

/// The guitarist label goes stale while the speaker prompt keeps finding
/// the subject; then a pass in which only the guitarist prompt fires, on a
/// box that takes the guitar in (IoU about 0.565: a match, not an alias).
/// The subject still answers "guitarist", so phase 3 matches it: one
/// subject, as before. A rule that dropped the alias would admit a twin.
#[test]
fn an_expiring_shot_label_never_creates_a_twin() {
    let a = Rect::from_center(Point { x: 0.5, y: 0.5 }, 0.1, 0.4);
    let wide = Rect::from_center(Point { x: 0.53, y: 0.52 }, 0.15, 0.4);
    let mut s = confirmed(a);
    let id = SubjectId(1);
    let iou = wide.iou(&a);
    let c = *s.config();
    assert!(iou > c.match_iou && iou <= c.alias_iou && (iou - 0.565).abs() < 0.01);
    let mut time = 1.0;
    for _ in 0..c.alias_stale_passes {
        time += 0.5;
        pass(&mut s, time, &[det("speaker", a)]);
    }
    assert_eq!(shots(&s, id), ["speaker"]);
    let ev = pass(&mut s, time + 0.5, &[det("guitarist", wide)]);
    assert!(created(&ev).is_empty(), "{ev:?}");
    assert!(ev.contains(&StoreEvent::Updated { id }));
    assert_eq!(s.len(), 1);
    assert_eq!(evidence(&s, id, "guitarist").unwrap().sightings, 1);
    assert_eq!(shots(&s, id), ["speaker"]);
}

/// Two guitarist boxes of one pass on one entity are one sighting.
#[test]
fn two_boxes_of_one_pass_count_once() {
    let mut s = store();
    let a = person(0.4, 0.5);
    let id = SubjectId(1);
    pass(&mut s, 0.0, &[det("speaker", a)]);
    // A second guitarist box on the same entity (IoU above alias_iou).
    let twice = [
        det("speaker", a),
        det("guitarist", a),
        det(
            "guitarist",
            Rect {
                x: a.x + 0.005,
                ..a
            },
        ),
    ];
    assert!(twice[2].bbox.iou(&a) > s.config().alias_iou);
    pass(&mut s, 0.5, &twice);
    assert_eq!(evidence(&s, id, "guitarist").unwrap().sightings, 1);
    assert_eq!(shots(&s, id), ["speaker"]);
    pass(&mut s, 1.0, &twice);
    assert_eq!(shots(&s, id), ["speaker", "guitarist"]);
    assert_eq!(s.len(), 1);
}

/// Removing an alias drops its evidence, so a re-added prompt starts over;
/// removing the primary keeps the promoted alias's evidence.
#[test]
fn remove_query_clears_label_evidence() {
    let a = person(0.4, 0.5);
    let mut s = confirmed(a);
    let id = SubjectId(1);
    let ev = s.remove_query(&q("guitarist"));
    assert_eq!(ev, vec![StoreEvent::QueriesChanged { id }]);
    assert_eq!(evidence(&s, id, "guitarist"), None);
    assert_eq!(shots(&s, id), ["speaker"]);
    // A guitarist prompt added again starts from nothing.
    pass(&mut s, 1.5, &[det("speaker", a), det("guitarist", a)]);
    assert_eq!(evidence(&s, id, "guitarist").unwrap().sightings, 1);
    assert_eq!(shots(&s, id), ["speaker"]);
    pass(&mut s, 2.0, &[det("speaker", a), det("guitarist", a)]);
    // Removing the primary promotes the alias, which keeps its evidence.
    let kept = evidence(&s, id, "guitarist");
    s.remove_query(&q("speaker"));
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.query, q("guitarist"));
    assert_eq!(rec.labels.keys().collect::<Vec<_>>(), [&q("guitarist")]);
    assert_eq!(evidence(&s, id, "guitarist"), kept);
    assert_eq!(shots(&s, id), ["guitarist"]);
}

/// On program, preview or pinned, a subject's labels still follow the
/// evidence; the engine keeps the shots an on-air subject already holds.
#[test]
fn on_air_and_pinned_subjects_follow_the_evidence_too() {
    let a = person(0.4, 0.5);
    let mut s = confirmed(a);
    let id = SubjectId(1);
    s.set_protected(&HashSet::from([id]));
    s.set_pinned(id, true).unwrap();
    let mut time = 1.0;
    for _ in 0..s.config().alias_stale_passes {
        time += 0.5;
        pass(&mut s, time, &[det("speaker", a)]);
    }
    assert_eq!(shots(&s, id), ["speaker"]);
    assert!(s.record(id).unwrap().answers(&q("guitarist")));
}

/// Per call: the events, then each subject's queries and shot queries.
type Trace = Vec<(
    Vec<StoreEvent>,
    Vec<(SubjectId, Vec<QueryId>, Vec<QueryId>)>,
)>;

/// Each subject's label evidence at the end of a run.
type Labels = BTreeMap<SubjectId, BTreeMap<QueryId, LabelEvidence>>;

/// A varied scripted run: two people, a guitarist prompt that comes and
/// goes on one of them and strays onto the other now and then, and a loss
/// and recovery in the middle.
fn scripted() -> (Trace, Labels) {
    let mut s = store();
    let (a, b) = (person(0.3, 0.5), person(0.7, 0.5));
    let mut trace = Trace::new();
    // A small linear congruential generator: a fixed, varied pattern.
    let mut seed: u32 = 7;
    let mut roll = move || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (seed >> 16) % 8
    };
    for k in 0..120u32 {
        let time = 0.5 * f64::from(k);
        let r = roll();
        let mut dets = Vec::new();
        if !(40..60).contains(&k) {
            dets.push(det("speaker", a));
            if r < 5 {
                dets.push(det("guitarist", a));
            }
        }
        dets.push(det("speaker", b));
        if r == 7 {
            dets.push(det("guitarist", b));
        }
        let mut ev = s.ingest_detections(t(time), &dets);
        ev.extend(s.tick(t(time)));
        let subjects = s
            .records()
            .map(|r| {
                let all = r.queries().cloned().collect();
                (r.id(), all, r.shot_queries().cloned().collect())
            })
            .collect();
        trace.push((ev, subjects));
    }
    let labels = s.records().map(|r| (r.id(), r.labels.clone())).collect();
    (trace, labels)
}

/// The same calls give the same events, shot queries and evidence.
#[test]
fn replay_gives_identical_shot_queries() {
    let (first, labels) = scripted();
    assert_eq!(scripted(), (first.clone(), labels));
    // The run takes both paths: a subject framing guitarist shots, and a
    // subject answering "guitarist" without framing them.
    let g = q("guitarist");
    let states = first.iter().flat_map(|(_, subjects)| subjects);
    let framing = states.clone().any(|(_, _, shot)| shot.contains(&g));
    let identity_only = states
        .clone()
        .any(|(_, all, shot)| all.contains(&g) && !shot.contains(&g));
    assert!(framing && identity_only);
}
