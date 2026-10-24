//! Helpers shared by the store's unit tests.
//!
//! Scenarios are written in seconds and in box centres, which reads closer to
//! the footage they model than raw `Duration`s and corners do.

use std::time::Duration;

use ac_core::{Extent, Point, QueryId, Rect, SourceId, SubjectId};

use crate::{Detection, StoreConfig, StoreEvent, SubjectStore, TrackObservation};

/// Media time `s` seconds into the stream.
pub(crate) fn t(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

/// A query id.
pub(crate) fn q(name: &str) -> QueryId {
    QueryId(name.into())
}

/// A person-sized box (0.1 × 0.3) centred at `(x, y)`.
pub(crate) fn person(x: f64, y: f64) -> Rect {
    Rect::from_center(Point { x, y }, 0.1, 0.3)
}

/// A detection of `query` at `bbox` with score 0.9, boxing an object.
pub(crate) fn det(query: &str, bbox: Rect) -> Detection {
    Detection {
        query: q(query),
        bbox,
        score: 0.9,
        extent: Extent::Object,
    }
}

/// A detection of `query` at `bbox` with score 0.9, boxing a whole person.
pub(crate) fn person_det(query: &str, bbox: Rect) -> Detection {
    Detection {
        extent: Extent::Person,
        ..det(query, bbox)
    }
}

/// A confident track observation.
pub(crate) fn track(id: SubjectId, bbox: Rect) -> TrackObservation {
    TrackObservation {
        id,
        bbox,
        confidence: 0.9,
    }
}

/// A store on source "cam" with the default config.
pub(crate) fn store() -> SubjectStore {
    store_with(StoreConfig::default())
}

/// A store on source "cam" with `config`.
pub(crate) fn store_with(config: StoreConfig) -> SubjectStore {
    SubjectStore::new(SourceId("cam".into()), config).expect("valid test config")
}

/// Ids of the `Created` events in `events`.
pub(crate) fn created(events: &[StoreEvent]) -> Vec<SubjectId> {
    events
        .iter()
        .filter_map(|e| match e {
            StoreEvent::Created { id, .. } => Some(*id),
            _ => None,
        })
        .collect()
}

/// Ids of the `Retired` events in `events`.
pub(crate) fn retired(events: &[StoreEvent]) -> Vec<SubjectId> {
    events
        .iter()
        .filter_map(|e| match e {
            StoreEvent::Retired { id, .. } => Some(*id),
            _ => None,
        })
        .collect()
}

/// Ids of live subjects in display order.
pub(crate) fn ids(store: &SubjectStore) -> Vec<SubjectId> {
    store.subjects().map(|s| s.id).collect()
}

/// Centre x of subject `id`'s box.
pub(crate) fn cx(store: &SubjectStore, id: SubjectId) -> f64 {
    store.get(id).expect("live subject").bbox.center().x
}
