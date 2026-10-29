//! What the engine core keeps of each source: its description, picture
//! and health ([`SourceSlot`]) and its discovery schedule
//! ([`DiscoveryState`]).
//!
//! # Theory
//!
//! The core sees a source only through the [`SourceFeed`](super::SourceFeed)
//! the runtime hands it every tick. A slot is what remains of those feeds:
//! the newest frame shown, when it arrived on the master clock, the health
//! and its reason, the counters the snapshot reports, and the source's
//! description, which a source missing at start gets from its first frame
//! and that frame's connection ([`SourceSlot::describe`]; see the module
//! docs of [`super`]). Discovery is scheduled per source: a request is due
//! at `next_due`, at most one is in flight, and a source whose detector is
//! still loading is *preparing* and gets none ([`DiscoveryState`]).
//!
//! # Invariants
//!
//! * The core holds one slot per configured source, in config order, and
//!   nothing but the core changes one.
//! * A source without a detector never prepares and never becomes due.
//! * A source is available only while it reports itself delivering and its
//!   newest new frame is at most [`SOURCE_STALE_AFTER`] old.
//! * Time is master-clock time handed in by the caller; nothing here reads
//!   a clock.

use std::collections::HashMap;
use std::time::Duration;

use ac_core::{QueryId, SourceId};
use ac_media::{SourceInfo, SourceState, VideoFrame};

use super::SOURCE_STALE_AFTER;
use crate::presentation::PresentationStats;
use crate::schedule::RateMeter;
use crate::snapshot::{DiscoveryStats, MatchView};
use crate::vision::SourceVision;

/// Discovery scheduling of one source.
#[derive(Debug, Clone, Default)]
pub(super) struct DiscoveryState {
    /// The source has a detector worker.
    pub(super) enabled: bool,
    /// Earliest time of the next request.
    pub(super) next_due: Duration,
    /// When the request in flight was sent.
    pub(super) in_flight_since: Option<Duration>,
    /// When the newest request was sent, for taking turns on a shared
    /// detector.
    pub(super) last_sent: Option<Duration>,
    /// Sequence number of the newest frame requested.
    pub(super) last_requested: Option<u64>,
    /// Sequence number of the newest frame whose results were applied.
    pub(super) last_applied: Option<u64>,
    /// Prompt feedback: per live query, what the newest applied result
    /// that looked for it found.
    pub(super) matches: HashMap<QueryId, MatchView>,
    /// Counters, and whether the detector is still preparing.
    pub(super) stats: DiscoveryStats,
}

impl DiscoveryState {
    /// The state of a source that runs discovery when `enabled`: it
    /// prepares until a tick says its detector has loaded.
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            stats: DiscoveryStats {
                preparing: enabled,
                ..DiscoveryStats::default()
            },
            ..Self::default()
        }
    }

    /// Takes in whether the detector is still `loading` at `now`; when it
    /// stops, the source is ready and due at once (see the module docs).
    pub(super) fn note_loading(&mut self, loading: bool, now: Duration) {
        if !self.enabled {
            return;
        }
        if loading {
            self.stats.preparing = true;
        } else if self.stats.preparing {
            self.stats.preparing = false;
            self.stats.ready_after = Some(now);
            self.next_due = now;
        }
    }
}

/// One source as the core sees it.
#[derive(Debug)]
pub(super) struct SourceSlot {
    /// Stable id.
    pub(super) id: SourceId,
    /// Operator-facing name.
    pub(super) name: String,
    /// The source's description: as given to
    /// [`EngineCore::new`](super::EngineCore::new), with the
    /// size and rate of its first frame's connection once one arrived (see
    /// [`SourceSlot::describe`]).
    pub(super) info: SourceInfo,
    /// Picture aspect (width / height).
    pub(super) aspect: f64,
    /// Identity and tracking.
    pub(super) vision: SourceVision,
    /// Newest frame seen.
    pub(super) frame: Option<VideoFrame>,
    /// Health reported by the source.
    pub(super) state: SourceState,
    /// Why the source is not delivering, as reported with `state`.
    pub(super) error: Option<String>,
    /// Master-clock time of the newest new frame.
    pub(super) last_frame_at: Option<Duration>,
    /// New frames seen.
    pub(super) frames: u64,
    /// Frames the source produced that the engine never showed (sequence
    /// gaps in the frames fed: the mailbox kept only the newest, or the
    /// source's presentation buffer passed over them).
    pub(super) missed: u64,
    /// The runtime's report on the source's presentation buffer
    /// ([`EngineCore::note_presentation`](super::EngineCore::note_presentation));
    /// `None` for a source without one.
    pub(super) presentation: Option<PresentationStats>,
    /// New-frame rate.
    pub(super) rate: RateMeter,
    /// Discovery scheduling.
    pub(super) discovery: DiscoveryState,
}

impl SourceSlot {
    /// Takes the description of a source's first frame, `frame`, and of the
    /// connection that delivered it, `connection` (see the module docs): a
    /// source missing at start was given a placeholder, and nothing has been
    /// tracked or framed on its picture yet. The size comes from the frame,
    /// the rate from the connection. An empty size, a description of another
    /// source and a rate that is not positive and finite are ignored.
    pub(super) fn describe(&mut self, frame: &VideoFrame, connection: Option<&SourceInfo>) {
        let size = (frame.width, frame.height);
        if size != (self.info.width, self.info.height) && size.0 > 0 && size.1 > 0 {
            self.info.width = frame.width;
            self.info.height = frame.height;
            self.aspect = frame.aspect();
        }
        let usable = |c: &&SourceInfo| c.id == self.id && c.fps.is_finite() && c.fps > 0.0;
        if let Some(c) = connection.filter(usable) {
            self.info.fps = c.fps;
        }
    }

    /// True when the source is delivering and has a fresh frame at `now`.
    pub(super) fn available(&self, now: Duration) -> bool {
        matches!(self.state, SourceState::Starting | SourceState::Running)
            && self
                .last_frame_at
                .is_some_and(|t| now.saturating_sub(t) <= SOURCE_STALE_AFTER)
    }
}
