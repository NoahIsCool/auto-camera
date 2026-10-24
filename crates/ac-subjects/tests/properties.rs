//! Property tests of the store's identity invariants under random input.
//!
//! Random sequences of detections (with latency), track observations, ticks
//! and operator commands are applied to a store while a model replays the
//! returned events. After every step the model checks that:
//!
//! * every created id is new and larger than all earlier ones;
//! * no protected subject is ever retired, and pinned or hidden subjects are
//!   never expired or evicted;
//! * the store never exceeds its capacity;
//! * tracker start/stop events follow the protocol and agree with the
//!   records (a tracker runs exactly while its subject is tracking);
//! * the live set and display order match the events;
//!
//! and finally that replaying the same calls yields the same events.
//!
//! A second property pins down same-pass aliasing: when several prompts box
//! one entity identically in one batch (as people-first matching does), the
//! batch reaches exactly one subject and that subject answers every one of
//! them, whatever the tracker's box (moved and rescaled to IoU 0.3 to 1.0
//! with the detection), however late the batch, and in any query set and
//! order. When one of the prompts is already the subject's, nothing is
//! created at all.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use ac_core::{Point, QueryId, Rect, SourceId, SubjectId};
use ac_subjects::{
    Detection, RetireReason, StoreConfig, StoreEvent, SubjectStore, TrackObservation,
};
use proptest::prelude::*;

/// Queries the generator draws from.
const QUERIES: [&str; 3] = ["person", "guitar", "speaker"];

/// One call on the store.
#[derive(Debug, Clone)]
enum Op {
    /// A detector batch computed `lag_ms` before the current time.
    Detect {
        /// Clock advance before the call.
        advance_ms: u64,
        /// Detector latency: the batch's PTS is this far in the past.
        lag_ms: u64,
        /// (query index, grid cell, jitter, score) per detection.
        dets: Vec<(usize, u8, u8, f32)>,
    },
    /// A tracker batch at the current time.
    Track {
        /// Clock advance before the call.
        advance_ms: u64,
        /// (subject id, grid cell, jitter, confidence) per observation.
        obs: Vec<(u32, u8, u8, f32)>,
    },
    /// A frame tick.
    Tick {
        /// Clock advance before the call.
        advance_ms: u64,
    },
    /// The switcher's program and preview subjects.
    Protect(Vec<u32>),
    /// Pin or unpin.
    Pin(u32, bool),
    /// Hide or show.
    Hide(u32, bool),
    /// Operator correction.
    Reacquire(u32, u8, u8),
    /// Remove a query.
    RemoveQuery(usize),
}

/// A box on a coarse grid so that random boxes often overlap.
fn cell(cell: u8, jitter: u8) -> Rect {
    let x = 0.05 + 0.13 * f64::from(cell % 6) + 0.01 * f64::from(jitter % 5);
    let y = 0.1 + 0.4 * f64::from((cell / 6) % 2);
    Rect::new(x, y, 0.12, 0.3)
}

/// Strategy for one operation.
fn op() -> impl Strategy<Value = Op> {
    let det = (0..QUERIES.len(), 0u8..12, 0u8..5, 0.2f32..1.0);
    let ob = (1u32..12, 0u8..12, 0u8..5, 0.2f32..1.0);
    prop_oneof![
        4 => (0u64..800, 0u64..400, prop::collection::vec(det, 0..6))
            .prop_map(|(advance_ms, lag_ms, dets)| Op::Detect { advance_ms, lag_ms, dets }),
        3 => (0u64..200, prop::collection::vec(ob, 0..6))
            .prop_map(|(advance_ms, obs)| Op::Track { advance_ms, obs }),
        3 => (0u64..20_000).prop_map(|advance_ms| Op::Tick { advance_ms }),
        1 => prop::collection::vec(1u32..12, 0..3).prop_map(Op::Protect),
        1 => (1u32..12, any::<bool>()).prop_map(|(id, on)| Op::Pin(id, on)),
        1 => (1u32..12, any::<bool>()).prop_map(|(id, on)| Op::Hide(id, on)),
        1 => (1u32..12, 0u8..12, 0u8..5).prop_map(|(id, c, j)| Op::Reacquire(id, c, j)),
        1 => (0..QUERIES.len()).prop_map(Op::RemoveQuery),
    ]
}

/// The model's view of the store, rebuilt from events.
#[derive(Debug, Default)]
struct Model {
    /// Clock in milliseconds.
    now: u64,
    /// Every id ever created.
    created: BTreeSet<SubjectId>,
    /// Ids created and not retired.
    live: BTreeSet<SubjectId>,
    /// Ids whose tracker runs.
    running: BTreeSet<SubjectId>,
    /// Current protected set.
    protected: HashSet<SubjectId>,
}

/// Applies `op` to `store`, returning its events.
fn apply(store: &mut SubjectStore, model: &mut Model, op: &Op) -> Vec<StoreEvent> {
    let ms = Duration::from_millis;
    match op {
        Op::Detect {
            advance_ms,
            lag_ms,
            dets,
        } => {
            model.now += advance_ms;
            let dets: Vec<Detection> = dets
                .iter()
                .map(|&(q, c, j, score)| Detection {
                    query: QueryId(QUERIES[q].into()),
                    bbox: cell(c, j),
                    score,
                })
                .collect();
            store.ingest_detections(ms(model.now.saturating_sub(*lag_ms)), &dets)
        }
        Op::Track { advance_ms, obs } => {
            model.now += advance_ms;
            let obs: Vec<TrackObservation> = obs
                .iter()
                .map(|&(id, c, j, confidence)| TrackObservation {
                    id: SubjectId(id),
                    bbox: cell(c, j),
                    confidence,
                })
                .collect();
            store.ingest_tracks(ms(model.now), &obs)
        }
        Op::Tick { advance_ms } => {
            model.now += advance_ms;
            store.tick(ms(model.now))
        }
        Op::Protect(ids) => {
            let ids: HashSet<SubjectId> = ids.iter().map(|&i| SubjectId(i)).collect();
            store.set_protected(&ids);
            // Ids that are not live are ignored by the store.
            model.protected = ids
                .into_iter()
                .filter(|id| model.live.contains(id))
                .collect();
            Vec::new()
        }
        Op::Pin(id, on) => {
            let _ = store.set_pinned(SubjectId(*id), *on);
            Vec::new()
        }
        Op::Hide(id, on) => {
            let _ = store.set_hidden(SubjectId(*id), *on);
            Vec::new()
        }
        Op::Reacquire(id, c, j) => store
            .reacquire(SubjectId(*id), cell(*c, *j), ms(model.now))
            .unwrap_or_default(),
        Op::RemoveQuery(q) => store.remove_query(&QueryId(QUERIES[*q].into())),
    }
}

/// Checks `events` against the model and the store, updating the model.
fn check(
    store: &SubjectStore,
    model: &mut Model,
    before: &SubjectStore,
    events: &[StoreEvent],
) -> Result<(), TestCaseError> {
    for event in events {
        let id = event.subject();
        match *event {
            StoreEvent::Created { .. } => {
                let newest = model.created.last().copied();
                prop_assert!(newest.is_none_or(|n| id > n), "id {id:?} not increasing");
                prop_assert!(model.created.insert(id), "id {id:?} reused");
                model.live.insert(id);
            }
            StoreEvent::Retired { reason, .. } => {
                prop_assert!(!model.protected.contains(&id), "protected {id:?} retired");
                if matches!(reason, RetireReason::Expired | RetireReason::Evicted) {
                    let rec = before.record(id).expect("retired subject was live");
                    prop_assert!(
                        !rec.subject.pinned && !rec.hidden,
                        "retained {id:?} retired"
                    );
                }
                prop_assert!(!model.running.contains(&id), "tracker left running");
                prop_assert!(model.live.remove(&id));
            }
            StoreEvent::NeedsTrackerStart { .. } => {
                model.running.insert(id);
            }
            StoreEvent::NeedsTrackerStop { .. } => {
                prop_assert!(model.running.remove(&id), "stop without start");
            }
            _ => prop_assert!(model.live.contains(&id), "event for dead {id:?}"),
        }
    }
    prop_assert!(store.len() <= store.config().max_subjects);
    let ids: Vec<SubjectId> = store.subjects().map(|s| s.id).collect();
    prop_assert!(ids.windows(2).all(|w| w[0] < w[1]), "display order");
    prop_assert_eq!(
        ids.iter().copied().collect::<BTreeSet<_>>(),
        model.live.clone()
    );
    for rec in store.records() {
        prop_assert_eq!(rec.tracker_running, model.running.contains(&rec.id()));
        prop_assert_eq!(rec.tracker_running, rec.is_tracking());
    }
    Ok(())
}

/// A small store, so capacity and eviction are exercised constantly.
fn fresh() -> SubjectStore {
    let config = StoreConfig {
        max_subjects: 4,
        ..StoreConfig::default()
    };
    SubjectStore::new(SourceId("cam".into()), config).expect("valid config")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn identity_invariants_hold(ops in prop::collection::vec(op(), 1..80)) {
        let mut store = fresh();
        let mut model = Model::default();
        let mut log = Vec::new();
        for op in &ops {
            let before = store.clone();
            let events = apply(&mut store, &mut model, op);
            check(&store, &mut model, &before, &events)?;
            log.push(events);
        }
        let mut replay = fresh();
        let mut replay_model = Model::default();
        for (op, events) in ops.iter().zip(&log) {
            prop_assert_eq!(&apply(&mut replay, &mut replay_model, op), events);
        }
    }
}

/// The entity every prompt boxes in the same-pass property.
fn entity() -> Rect {
    Rect::new(0.4, 0.3, 0.1, 0.3)
}

/// `bbox` moved by `(dx, dy)` and scaled by `(sx, sy)` about its centre.
fn warp(bbox: Rect, (dx, dy): (f64, f64), (sx, sy): (f64, f64)) -> Rect {
    let c = bbox.center();
    let centre = Point {
        x: c.x + dx,
        y: c.y + dy,
    };
    Rect::from_center(centre, bbox.w * sx, bbox.h * sy)
}

/// One detection of the entity per query in `queries`, in that order.
fn boxes_of_the_entity(queries: &[&str]) -> Vec<Detection> {
    queries
        .iter()
        .map(|&q| Detection {
            query: QueryId(q.into()),
            bbox: entity(),
            score: 0.9,
        })
        .collect()
}

/// A non-empty set of distinct queries in random order.
fn query_set() -> impl Strategy<Value = Vec<&'static str>> {
    prop::sample::subsequence(QUERIES.to_vec(), 1..=QUERIES.len()).prop_shuffle()
}

/// Number of `Created` events in `events`.
fn created_count(events: &[StoreEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, StoreEvent::Created { .. }))
        .count()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// A pass in which the `pass` prompts box one entity identically reaches
    /// one subject that answers them all; see the module docs.
    #[test]
    fn identical_boxes_of_one_pass_are_one_subject(
        seeded in query_set(),
        pass in query_set(),
        offset in (-0.04f64..0.04, -0.1f64..0.1),
        scale in (0.75f64..1.33, 0.75f64..1.33),
        lag_frames in 0u32..12,
    ) {
        let tracked = warp(entity(), offset, scale);
        prop_assume!((0.3..=1.0).contains(&tracked.iou(&entity())));
        let mut store = SubjectStore::new(SourceId("cam".into()), StoreConfig::default())
            .expect("valid config");
        let ev = store.ingest_detections(Duration::ZERO, &boxes_of_the_entity(&seeded));
        prop_assert_eq!(created_count(&ev), 1, "{:?}", ev);
        let id = SubjectId(1);
        // The tracker moves to `tracked` and holds it until `lag_frames`
        // after the PTS of the next pass, which sees the entity unchanged.
        for frame in 1..=30 + lag_frames {
            let pts = Duration::from_secs_f64(f64::from(frame) / 30.0);
            let ob = TrackObservation { id, bbox: tracked, confidence: 0.9 };
            store.ingest_tracks(pts, &[ob]);
        }
        let ev = store.ingest_detections(Duration::from_secs(1), &boxes_of_the_entity(&pass));
        prop_assert!(created_count(&ev) <= 1, "{:?}", ev);
        let touched: BTreeSet<SubjectId> = ev.iter().map(StoreEvent::subject).collect();
        prop_assert_eq!(touched.len(), 1, "{:?}", ev);
        let subject = *touched.first().expect("one subject");
        let rec = store.record(subject).expect("the touched subject is live");
        for q in &pass {
            prop_assert!(rec.answers(&QueryId((*q).into())), "{} unanswered: {:?}", q, ev);
        }
        if pass.iter().any(|q| seeded.contains(q)) {
            prop_assert_eq!(subject, id);
            prop_assert_eq!(created_count(&ev), 0, "{:?}", ev);
        }
    }
}
