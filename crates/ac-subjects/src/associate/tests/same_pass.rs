//! Same-pass aliasing: several prompts boxing one entity in one batch give
//! one subject, also when the result is late and the tracker holds a newer,
//! differently sized or placed box (the duplicate-guitarist bug).

use ac_core::{Rect, SubjectId};

use crate::testkit::*;
use crate::{Detection, StoreEvent, SubjectStore};

/// The guitarist and speaker prompts boxing one entity identically, as
/// people-first matching does.
fn both(bbox: Rect) -> [Detection; 2] {
    [det("guitarist", bbox), det("speaker", bbox)]
}

/// `bbox` made 25% wider to the right, as when a later pass takes the guitar
/// neck in: IoU 0.8 with `bbox`, which agrees with it (above `reseed_iou`)
/// but is not above `alias_iou`.
fn wider(bbox: Rect) -> Rect {
    Rect {
        w: bbox.w * 1.25,
        ..bbox
    }
}

/// Feeds subject `id`'s tracker the 30 Hz `frames`, the box at `at(time)`.
fn track_frames(
    s: &mut SubjectStore,
    id: SubjectId,
    frames: std::ops::RangeInclusive<u32>,
    at: impl Fn(f64) -> Rect,
) {
    for frame in frames {
        let time = f64::from(frame) / 30.0;
        s.ingest_tracks(t(time), &[track(id, at(time))]);
    }
}

/// Asserts that `iou` lies in the window where a late detection keeps the
/// tracker's box and the box alone would not alias.
fn assert_in_twin_window(s: &SubjectStore, iou: f64) {
    let c = s.config();
    assert!(
        iou >= c.reseed_iou && iou <= c.alias_iou,
        "IoU {iou} outside [{}, {}]",
        c.reseed_iou,
        c.alias_iou
    );
}

/// A static guitarist whose tracker holds the seed box, and a pass 200 ms
/// late that boxes them 25% wider for both prompts: the first prompt's box
/// keeps the tracker's newer box, and the second prompt's identical box must
/// still find the subject the first one just observed.
#[test]
fn a_late_pass_boxing_a_static_subject_wider_stays_one_subject() {
    let mut s = store();
    let seed = person(0.4, 0.5);
    let ev = s.ingest_detections(t(0.0), &both(seed));
    assert_eq!(created(&ev), vec![SubjectId(1)]);
    let id = SubjectId(1);
    track_frames(&mut s, id, 1..=36, |_| seed);
    let seen = wider(seed);
    assert_in_twin_window(&s, seen.iou(&seed));
    let ev = s.ingest_detections(t(1.0), &both(seen));
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert_eq!(s.len(), 1);
    let rec = s.record(id).unwrap();
    assert!(rec.answers(&q("guitarist")) && rec.answers(&q("speaker")));
    assert_eq!(rec.subject.bbox, seed, "the tracker's newer box is kept");
}

/// A walking subject whose tracker runs 200 ms ahead of the pass and whose
/// detector box sits 0.02 to the right of the tracker's (IoU about 0.67 at
/// the pass's PTS).
#[test]
fn a_late_pass_behind_a_moving_tracker_stays_one_subject() {
    let mut s = store();
    let walk = |time: f64| person(0.3 + 0.1 * time, 0.5);
    s.ingest_detections(t(0.0), &both(walk(0.0)));
    let id = SubjectId(1);
    track_frames(&mut s, id, 1..=36, walk);
    let seen = person(0.42, 0.5);
    let iou = s.record(id).unwrap().predicted(t(1.0)).iou(&seen);
    assert!((iou - 0.67).abs() < 0.01, "IoU {iou}");
    let ev = s.ingest_detections(t(1.0), &both(seen));
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert_eq!(s.len(), 1);
    let rec = s.record(id).unwrap();
    assert!(rec.answers(&q("guitarist")) && rec.answers(&q("speaker")));
}

/// A subject whose primary prompt sorts after the other one (speaker, with
/// guitarist as an alias): the late pass matches it in the speaker group,
/// and the guitarist box reaches it through the same-pass box.
#[test]
fn a_late_pass_reaches_a_subject_whose_primary_prompt_sorts_last() {
    let mut s = store();
    let seed = person(0.4, 0.5);
    s.ingest_detections(t(0.0), &[det("speaker", seed)]);
    let id = SubjectId(1);
    s.ingest_detections(t(0.5), &[det("guitarist", seed)]);
    let rec = s.record(id).unwrap();
    assert!(rec.subject.query == q("speaker") && rec.aliases.contains(&q("guitarist")));
    track_frames(&mut s, id, 16..=36, |_| seed);
    let ev = s.ingest_detections(t(1.0), &both(wider(seed)));
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert_eq!(s.len(), 1);
}

/// A subject that answers only the speaker prompt, and a late pass in which
/// the guitarist prompt boxes it too: the guitarist prompt becomes an alias
/// (one `QueriesChanged`), nothing is created and the subject is observed
/// once.
#[test]
fn a_late_pass_adds_the_other_prompt_as_an_alias() {
    let mut s = store();
    let seed = person(0.4, 0.5);
    s.ingest_detections(t(0.0), &[det("speaker", seed)]);
    let id = SubjectId(1);
    track_frames(&mut s, id, 1..=36, |_| seed);
    let ev = s.ingest_detections(t(1.0), &both(wider(seed)));
    assert_eq!(
        ev,
        vec![
            StoreEvent::Updated { id },
            StoreEvent::QueriesChanged { id }
        ]
    );
    assert_eq!(s.len(), 1);
    let rec = s.record(id).unwrap();
    assert_eq!(rec.subject.query, q("speaker"));
    assert!(rec.aliases.contains(&q("guitarist")));
}

/// An operator fix at t = 2.0, then a pass computed at t = 1.9: the pass is
/// older than the anchor, so it only confirms the subject, and the second
/// prompt's identical box must not become a twin.
#[test]
fn a_pass_older_than_an_operator_fix_does_not_create_a_twin() {
    let mut s = store();
    let seed = person(0.4, 0.5);
    s.ingest_detections(t(0.0), &both(seed));
    let id = SubjectId(1);
    track_frames(&mut s, id, 1..=60, |_| seed);
    s.reacquire(id, seed, t(2.0)).unwrap();
    let ev = s.ingest_detections(t(1.9), &both(wider(seed)));
    assert_eq!(ev, vec![StoreEvent::Updated { id }]);
    assert_eq!(s.len(), 1);
    assert_eq!(s.get(id).unwrap().bbox, seed, "the operator's box stands");
}

/// Only a box from the same pass counts besides the prediction: a subject
/// that walked away keeps its old anchor, and a newcomer of another prompt
/// standing there is a new subject.
#[test]
fn a_newcomer_at_a_walked_away_subjects_old_anchor_is_admitted() {
    let mut s = store();
    s.ingest_detections(t(0.0), &[det("guitarist", person(0.2, 0.5))]);
    let id = SubjectId(1);
    track_frames(&mut s, id, 1..=30, |time| person(0.2 + 0.3 * time, 0.5));
    let ev = s.ingest_detections(t(1.0), &[det("speaker", person(0.2, 0.5))]);
    let newcomer = SubjectId(2);
    assert_eq!(created(&ev), vec![newcomer]);
    assert!(!s.record(id).unwrap().answers(&q("speaker")));
    assert_eq!(s.get(newcomer).unwrap().query, q("speaker"));
}
