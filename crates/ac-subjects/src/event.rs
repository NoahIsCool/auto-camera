//! What the store reports and why it refuses commands.
//!
//! The store never calls out; every state change it makes is returned as a
//! [`StoreEvent`] from the call that caused it, in the order it happened. The
//! engine turns the tracker events into commands for the fast tracker and the
//! rest into shot-catalog and UI updates. Replaying the same inputs yields the
//! same events, which is what makes the store testable offline.
//!
//! Tracker events follow a strict protocol per subject: a
//! [`NeedsTrackerStart`](StoreEvent::NeedsTrackerStart) (re)starts the
//! subject's tracker at a box, a [`NeedsTrackerStop`](StoreEvent::NeedsTrackerStop)
//! ends it, and the store never asks to stop a tracker that is not running.
//! A start while a tracker runs means "restart from this box".

use std::time::Duration;

use ac_core::{Rect, SubjectId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Why a subject left the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetireReason {
    /// Lost for longer than the configured retirement delay.
    Expired,
    /// Lost and evicted to admit a new subject at capacity.
    Evicted,
    /// The operator removed the subject's query.
    QueryRemoved,
}

/// A state change reported by the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreEvent {
    /// A new subject was admitted.
    Created {
        /// The new subject.
        id: SubjectId,
        /// Its first box.
        bbox: Rect,
    },
    /// A tracking subject's box or confidence changed.
    Updated {
        /// The subject.
        id: SubjectId,
    },
    /// The set of queries a subject answers changed (alias added or removed,
    /// or an alias promoted to its primary query). Re-read its record.
    QueriesChanged {
        /// The subject.
        id: SubjectId,
    },
    /// A subject stopped being observed; framing should hold its last box.
    Lost {
        /// The subject.
        id: SubjectId,
    },
    /// A lost subject was observed again under the same id.
    Recovered {
        /// The subject.
        id: SubjectId,
    },
    /// A subject left the store. Its id is never reused.
    Retired {
        /// The subject.
        id: SubjectId,
        /// Why it left.
        reason: RetireReason,
    },
    /// Start, or restart, the subject's fast tracker at `bbox`.
    NeedsTrackerStart {
        /// The subject.
        id: SubjectId,
        /// Box to seed the tracker with.
        bbox: Rect,
        /// Media time of the frame `bbox` was observed on; seed the tracker on
        /// that frame when it is still available.
        pts: Duration,
    },
    /// Stop the subject's fast tracker and drop its pending observations.
    NeedsTrackerStop {
        /// The subject.
        id: SubjectId,
    },
}

impl StoreEvent {
    /// The subject the event is about.
    pub fn subject(&self) -> SubjectId {
        match self {
            StoreEvent::Created { id, .. }
            | StoreEvent::Updated { id }
            | StoreEvent::QueriesChanged { id }
            | StoreEvent::Lost { id }
            | StoreEvent::Recovered { id }
            | StoreEvent::Retired { id, .. }
            | StoreEvent::NeedsTrackerStart { id, .. }
            | StoreEvent::NeedsTrackerStop { id } => *id,
        }
    }
}

/// Why the store refused a command or a config.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum StoreError {
    /// A [`StoreConfig`](crate::StoreConfig) field is out of range.
    #[error("invalid store config: {0}")]
    InvalidConfig(&'static str),
    /// No live subject has this id.
    #[error("unknown subject {0:?}")]
    UnknownSubject(SubjectId),
    /// The box is not finite, has no area, or lies outside the frame.
    #[error("invalid box {0:?}")]
    InvalidBox(Rect),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_names_its_subject() {
        let id = SubjectId(7);
        let events = [
            StoreEvent::Created {
                id,
                bbox: Rect::FULL,
            },
            StoreEvent::Updated { id },
            StoreEvent::QueriesChanged { id },
            StoreEvent::Lost { id },
            StoreEvent::Recovered { id },
            StoreEvent::Retired {
                id,
                reason: RetireReason::Expired,
            },
            StoreEvent::NeedsTrackerStart {
                id,
                bbox: Rect::FULL,
                pts: Duration::ZERO,
            },
            StoreEvent::NeedsTrackerStop { id },
        ];
        assert!(events.iter().all(|e| e.subject() == id));
    }

    #[test]
    fn errors_render_readably() {
        let e = StoreError::UnknownSubject(SubjectId(3));
        assert_eq!(e.to_string(), "unknown subject SubjectId(3)");
    }
}
