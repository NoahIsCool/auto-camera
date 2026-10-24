//! The per-source subject store: one owner for every identity on a source.
//!
//! # Lifecycle
//!
//! ```text
//!            detection (unmatched, capacity)          confident observation
//!   (none) ──────────────────────────────► Tracking ◄──────────────────────┐
//!                                            │  no confident observation    │
//!                                            │  for `lost_after`            │ matching detection
//!                                            ▼                              │ or reacquire
//!                                           Lost ───────────────────────────┘
//!                                            │  lost for `retire_after`, or evicted
//!                                            ▼  at capacity (only if not retained)
//!                                         Retired (id never reused)
//! ```
//!
//! Time is the source's media clock: every call takes the PTS of the frame
//! its inputs were computed on, and [`SubjectStore::tick`] applies the
//! time-based transitions. Nothing reads a wall clock, so a recorded stream of
//! calls replays to the same events.
//!
//! While a subject is tracking, a matched detection may also restart its
//! tracker (a [`StoreEvent::NeedsTrackerStart`] while it runs). That happens
//! when the detection disagrees with the box (the tracker drifted), when no
//! tracker runs (a failed start), and when the tracker coasted on every
//! observation it reported since the subject's previous applied detection
//! ([`crate::record`]). A coasting tracker reports no confident observation,
//! so only detections keep its subject alive. When they are applied within
//! `lost_after` of the previous one's PTS (pass interval plus latency), the
//! first one after a whole window of coasting restarts the tracker; when
//! they are further apart, the subject is lost first and the detection that
//! recovers it restarts the tracker.
//!
//! # Invariants
//!
//! * Ids come from a counter that only moves forward: they are unique for the
//!   life of the store and never reused, and display order (ascending id) is
//!   admission order, so a subject's number never changes.
//! * At most [`StoreConfig::max_subjects`] subjects are live.
//! * Expiry and eviction never touch a *retained* subject: protected (on
//!   program or preview), pinned or hidden. Protected subjects are never
//!   retired at all while protected, and never demoted as duplicates.
//! * The fast tracker of a subject runs only while the subject is tracking,
//!   and the start/stop events say so. It runs whenever the subject is
//!   tracking, except after a start the engine reported as failed
//!   ([`SubjectStore::tracker_failed`]): the subject then follows its
//!   detections alone until the next matching detection retries the start.
//! * A tracker that coasted since the previous applied detection is
//!   restarted by the next applied one. A subject that got no track
//!   observation in that window, or a confident one, is not: without
//!   observations a detection that agrees with the box only re-anchors it,
//!   as it always has.
//! * Fast-track observations never create or revive a subject; only
//!   detections and operator reacquires do.
//! * Shot labels ([`crate::labels`]) change only in detection passes that
//!   observe the subject and in [`SubjectStore::remove_query`]. Ticks,
//!   losses, retention flags and reacquires leave them alone, so a lost
//!   subject keeps every label through the gap. Protected and pinned
//!   subjects get no exemption: their labels follow the evidence like any
//!   other's (the engine keeps the shots an on-air subject already holds).
//!
//! Association of detections lives in [`crate::associate`].

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use ac_core::{QueryId, Rect, SourceId, Subject, SubjectId, TrackState};

use crate::config::StoreConfig;
use crate::event::{RetireReason, StoreError, StoreEvent};
use crate::observation::{TrackObservation, sanitize};
use crate::record::{SubjectRecord, TrackerReport};

/// Stable identities for the subjects of one source.
#[derive(Debug, Clone)]
pub struct SubjectStore {
    /// Source every subject belongs to.
    pub(crate) source: SourceId,
    /// Validated tuning.
    pub(crate) config: StoreConfig,
    /// Live subjects in display (id) order.
    pub(crate) subjects: BTreeMap<SubjectId, SubjectRecord>,
    /// Next id to hand out; ids above `u32::MAX` are exhausted.
    pub(crate) next_id: u64,
}

impl SubjectStore {
    /// An empty store for `source`, after validating `config`.
    pub fn new(source: SourceId, config: StoreConfig) -> Result<Self, StoreError> {
        config.validate()?;
        Ok(Self {
            source,
            config,
            subjects: BTreeMap::new(),
            next_id: 1,
        })
    }

    /// The source this store tracks.
    pub fn source(&self) -> &SourceId {
        &self.source
    }

    /// The store's tuning.
    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    /// Number of live subjects.
    pub fn len(&self) -> usize {
        self.subjects.len()
    }

    /// True when no subject is live.
    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty()
    }

    /// Every live subject in display order (ascending id), hidden ones
    /// included.
    pub fn subjects(&self) -> impl Iterator<Item = &Subject> {
        self.subjects.values().map(|r| &r.subject)
    }

    /// Every live subject's full record in display order.
    pub fn records(&self) -> impl Iterator<Item = &SubjectRecord> {
        self.subjects.values()
    }

    /// The records the operator has not hidden, in display order.
    pub fn visible(&self) -> impl Iterator<Item = &SubjectRecord> {
        self.subjects.values().filter(|r| !r.hidden)
    }

    /// The subject with `id`, if live.
    pub fn get(&self, id: SubjectId) -> Option<&Subject> {
        self.subjects.get(&id).map(|r| &r.subject)
    }

    /// The full record of the subject with `id`, if live.
    pub fn record(&self, id: SubjectId) -> Option<&SubjectRecord> {
        self.subjects.get(&id)
    }

    /// Applies fast-tracker observations made on the frame at `pts`.
    ///
    /// An observation is *usable* when the subject is tracking, its tracker
    /// is running, the box is valid, the confidence is a number and `pts` is
    /// not older than the subject's current box. A usable observation whose
    /// confidence reaches `track_min_confidence` moves the box; one below it
    /// leaves the box alone and only counts as coasting evidence
    /// ([`crate::record`]), so that the next detection restarts a tracker
    /// that coasted all along. Anything else is ignored: tracks never create
    /// or revive a subject. Emits one [`StoreEvent::Updated`] per moved
    /// subject.
    pub fn ingest_tracks(&mut self, pts: Duration, obs: &[TrackObservation]) -> Vec<StoreEvent> {
        let mut events = Vec::new();
        let min = self.config.track_min_confidence;
        for ob in obs {
            // Both comparisons are false for NaN, which is neither confident
            // nor coasting evidence.
            let confident = ob.confidence >= min;
            let coasting = ob.confidence < min;
            let (Some(bbox), true) = (sanitize(&ob.bbox), confident || coasting) else {
                continue;
            };
            let Some(rec) = self.subjects.get_mut(&ob.id) else {
                continue;
            };
            if !rec.is_tracking() || !rec.tracker_running || pts < rec.motion.box_pts {
                continue;
            }
            rec.note_track(confident);
            if !confident {
                continue;
            }
            rec.move_to(bbox, pts);
            rec.last_seen = rec.last_seen.max(pts);
            let updated = StoreEvent::Updated { id: ob.id };
            if !events.contains(&updated) {
                events.push(updated);
            }
        }
        events
    }

    /// Applies the time-based transitions due at `pts`; call once per frame.
    ///
    /// * A tracking subject with no confident observation for `lost_after`
    ///   becomes lost (as of `last_seen + lost_after`) and its tracker stops.
    /// * A lost subject that is not retained is retired once it has been lost
    ///   for `retire_after`.
    /// * An orphan (its query was removed while it was on air) is retired as
    ///   soon as it is no longer protected.
    pub fn tick(&mut self, pts: Duration) -> Vec<StoreEvent> {
        let mut events = Vec::new();
        let ids: Vec<SubjectId> = self.subjects.keys().copied().collect();
        for id in ids {
            let rec = &self.subjects[&id];
            if rec.orphaned && !rec.protected {
                self.retire(id, RetireReason::QueryRemoved, &mut events);
                continue;
            }
            let lost_at = rec.last_seen.saturating_add(self.config.lost_after);
            if rec.is_tracking() && pts >= lost_at {
                self.lose(id, lost_at, &mut events);
            }
            let rec = &self.subjects[&id];
            if let Some(since) = rec.lost_since
                && !rec.is_retained()
                && pts >= since.saturating_add(self.config.retire_after)
            {
                self.retire(id, RetireReason::Expired, &mut events);
            }
        }
        events
    }

    /// Pins or unpins a subject. A pinned subject is never retired
    /// automatically.
    pub fn set_pinned(&mut self, id: SubjectId, pinned: bool) -> Result<(), StoreError> {
        self.record_mut(id)?.subject.pinned = pinned;
        Ok(())
    }

    /// Hides or shows a subject. A hidden subject stays tracked (so the same
    /// entity is not re-admitted as a new subject) and is never retired
    /// automatically.
    pub fn set_hidden(&mut self, id: SubjectId, hidden: bool) -> Result<(), StoreError> {
        self.record_mut(id)?.hidden = hidden;
        Ok(())
    }

    /// Replaces the protected set with the subjects the switcher has on
    /// program or preview. Ids that are not live are ignored.
    pub fn set_protected(&mut self, ids: &HashSet<SubjectId>) {
        for (id, rec) in &mut self.subjects {
            rec.protected = ids.contains(id);
        }
    }

    /// Operator correction: the subject `id` is at `bbox` on the frame at
    /// `pts`.
    ///
    /// The subject keeps its id and becomes tracking at `bbox` (a fresh anchor
    /// with no velocity), and its tracker is restarted there. Any other
    /// unprotected tracking subject sitting on `bbox` above `alias_iou` was a
    /// duplicate of this entity and becomes lost.
    pub fn reacquire(
        &mut self,
        id: SubjectId,
        bbox: Rect,
        pts: Duration,
    ) -> Result<Vec<StoreEvent>, StoreError> {
        let clean = sanitize(&bbox).ok_or(StoreError::InvalidBox(bbox))?;
        let rec = self.record_mut(id)?;
        let mut events = Vec::new();
        let was_lost = !rec.is_tracking();
        rec.reseed(clean, pts);
        rec.subject.state = TrackState::Tracking;
        rec.lost_since = None;
        rec.shadowed_since = None;
        rec.last_seen = rec.last_seen.max(pts);
        rec.tracker_running = true;
        events.push(if was_lost {
            StoreEvent::Recovered { id }
        } else {
            StoreEvent::Updated { id }
        });
        events.push(StoreEvent::NeedsTrackerStart {
            id,
            bbox: clean,
            pts,
        });
        let alias_iou = self.config.alias_iou;
        let duplicates: Vec<SubjectId> = self
            .subjects
            .values()
            .filter(|r| r.id() != id && r.is_tracking() && !r.protected)
            .filter(|r| r.predicted(pts).iou(&clean) > alias_iou)
            .map(SubjectRecord::id)
            .collect();
        for dup in duplicates {
            self.lose(dup, pts, &mut events);
        }
        Ok(events)
    }

    /// The operator removed `query`.
    ///
    /// The query is dropped from every alias list, with its label evidence. A
    /// subject whose primary query it was is promoted to an alias when it has
    /// one, kept as an orphan while it is protected, and retired otherwise
    /// (pinned and hidden subjects included: this is an operator action, not
    /// automatic). The alias promoted is the first whose label frames shots
    /// ([`crate::labels`]), or the first alias when none does: the primary
    /// frames shots whatever its evidence, so promoting an alias from one
    /// stray box while a confirmed one waits would frame the stray at once.
    pub fn remove_query(&mut self, query: &QueryId) -> Vec<StoreEvent> {
        let mut events = Vec::new();
        let ids: Vec<SubjectId> = self.subjects.keys().copied().collect();
        for id in ids {
            let Some(rec) = self.subjects.get_mut(&id) else {
                continue;
            };
            let mut changed = rec.aliases.remove(query);
            rec.labels.remove(query);
            if rec.subject.query == *query && !rec.orphaned {
                let framing = rec
                    .aliases
                    .iter()
                    .find(|a| rec.labels.get(*a).is_some_and(|e| e.frames))
                    .cloned();
                let next = match framing {
                    Some(a) => rec.aliases.take(&a),
                    None => rec.aliases.pop_first(),
                };
                if let Some(next) = next {
                    rec.subject.query = next;
                    changed = true;
                } else if rec.protected {
                    rec.orphaned = true;
                } else {
                    self.retire(id, RetireReason::QueryRemoved, &mut events);
                    continue;
                }
            }
            if changed {
                events.push(StoreEvent::QueriesChanged { id });
            }
        }
        events
    }

    /// The engine could not start the tracker that a
    /// [`StoreEvent::NeedsTrackerStart`] for `id` asked for: the box was too
    /// small or had no texture, or no frame was at hand.
    ///
    /// The subject stays as it is (tracking, on its detections), but its
    /// tracker counts as not running, so its track observations are ignored
    /// and the next detection that matches it asks for a start again
    /// ([`crate::associate`] restarts a subject whose tracker is not
    /// running). Without this, an agreeing detection would only re-anchor
    /// the box and the tracker would never be retried. Unknown ids are
    /// ignored; no event is emitted, since nothing runs that could be
    /// stopped.
    pub fn tracker_failed(&mut self, id: SubjectId) {
        if let Some(rec) = self.subjects.get_mut(&id) {
            rec.tracker_running = false;
            rec.tracker_report = TrackerReport::Silent;
        }
    }

    /// The live record `id`, mutably, or [`StoreError::UnknownSubject`].
    fn record_mut(&mut self, id: SubjectId) -> Result<&mut SubjectRecord, StoreError> {
        self.subjects
            .get_mut(&id)
            .ok_or(StoreError::UnknownSubject(id))
    }

    /// Marks a tracking subject lost as of `at` and stops its tracker.
    pub(crate) fn lose(&mut self, id: SubjectId, at: Duration, events: &mut Vec<StoreEvent>) {
        let Some(rec) = self.subjects.get_mut(&id) else {
            return;
        };
        if !rec.is_tracking() {
            return;
        }
        rec.subject.state = TrackState::Lost;
        rec.lost_since = Some(at);
        rec.shadowed_since = None;
        rec.motion.velocity = Default::default();
        rec.tracker_report = TrackerReport::Silent;
        events.push(StoreEvent::Lost { id });
        if rec.tracker_running {
            rec.tracker_running = false;
            events.push(StoreEvent::NeedsTrackerStop { id });
        }
    }

    /// Removes a subject, stopping its tracker first if it runs.
    pub(crate) fn retire(
        &mut self,
        id: SubjectId,
        reason: RetireReason,
        events: &mut Vec<StoreEvent>,
    ) {
        let Some(rec) = self.subjects.remove(&id) else {
            return;
        };
        if rec.tracker_running {
            events.push(StoreEvent::NeedsTrackerStop { id });
        }
        events.push(StoreEvent::Retired { id, reason });
    }
}

#[cfg(test)]
mod tests;
