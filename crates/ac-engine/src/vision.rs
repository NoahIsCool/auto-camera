//! Per-source vision state: the subject store and its fast tracker.
//!
//! [`SourceVision`] is the single owner of identity on one source. It glues
//! the two vision tiers to the [`SubjectStore`] following the store's engine
//! contract:
//!
//! * **Every new frame** advances the [`TemplateTracker`], feeds its
//!   observations to [`SubjectStore::ingest_tracks`] with the frame's PTS,
//!   and runs [`SubjectStore::tick`] at that PTS. Low-confidence
//!   observations are passed on too: they do not move a box, but they tell
//!   the store that a tracker coasts, so that its next detection restarts
//!   it even when it agrees with the box.
//! * **Every accepted detection batch** goes to
//!   [`SubjectStore::ingest_detections`] with the PTS of the frame it was
//!   computed on.
//! * **Tracker events** from the store are executed at once:
//!   `NeedsTrackerStart` seeds the tracker on the frame the box was observed
//!   on (the detection frame when it is still at hand, else the newest frame)
//!   and `NeedsTrackerStop` stops it. The tracker runs on this thread, in
//!   step with the store, so no observation from a replaced track can arrive
//!   late. A start that fails (a flat or tiny box, for example on a dark
//!   frame during a fade) is reported back with
//!   [`SubjectStore::tracker_failed`], so the store's view of which trackers
//!   run stays true and the subject's next matching detection retries the
//!   start instead of merely re-anchoring a box nothing tracks.
//!
//! * **Detections vouch for existence.** A tracker that drifted onto
//!   background or onto another subject can keep a high confidence
//!   indefinitely, and the store would keep such a "zombie" alive. So track
//!   observations of a subject are withheld once the newest detection batch
//!   is more than `max_undetected` newer than the subject's last detection;
//!   the store then loses it after `lost_after` as usual (and retires it
//!   later unless it is protected, pinned or hidden). Before the first batch
//!   every observation is passed on.
//!
//! The store and the tracker live in different crates with their own
//! observation types; the conversions happen here, and a detection's
//! extent (whole person or object) is passed on unchanged. Time is always the
//! source's media clock, never the engine's master clock.

use std::collections::HashMap;
use std::time::Duration;

use ac_core::{QueryId, Rect, SourceId, SubjectId};
use ac_media::VideoFrame;
use ac_subjects::{StoreConfig, StoreError, StoreEvent, SubjectStore};
use ac_vision::{TemplateTracker, TemplateTrackerConfig, Tracker};

/// Counters for one source's vision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisionStats {
    /// Frames the tracker advanced on.
    pub frames: u64,
    /// Detection batches applied.
    pub detection_batches: u64,
    /// Subjects admitted.
    pub created: u64,
    /// Subjects retired.
    pub retired: u64,
    /// Tracker starts that failed (box too small or without texture).
    pub tracker_start_failures: u64,
    /// Track observations withheld because the detector stopped seeing the
    /// subject.
    pub unvouched_observations: u64,
}

/// Identity and tracking state of one source; see the module docs.
#[derive(Debug)]
pub struct SourceVision {
    /// The identity owner.
    store: SubjectStore,
    /// The fast tier.
    tracker: TemplateTracker,
    /// Latest tracker confidence per subject.
    confidence: HashMap<SubjectId, f32>,
    /// Sequence number of the newest frame applied.
    last_seq: Option<u64>,
    /// PTS of the newest applied detection batch.
    last_batch: Option<Duration>,
    /// Longest a subject may go undetected (relative to the newest batch)
    /// while its tracker keeps it alive.
    max_undetected: Duration,
    /// Counters.
    stats: VisionStats,
}

impl SourceVision {
    /// Vision for `source` with the given tuning.
    pub fn new(
        source: SourceId,
        store: StoreConfig,
        tracker: TemplateTrackerConfig,
        max_undetected: Duration,
    ) -> Result<Self, String> {
        Ok(Self {
            store: SubjectStore::new(source, store).map_err(|e| e.to_string())?,
            tracker: TemplateTracker::new(tracker).map_err(|e| e.to_string())?,
            confidence: HashMap::new(),
            last_seq: None,
            last_batch: None,
            max_undetected,
            stats: VisionStats::default(),
        })
    }

    /// True when the detector still vouches for `id`: no batch was applied
    /// yet, or the subject was detected within `max_undetected` of the
    /// newest batch.
    fn vouched(&self, id: SubjectId) -> bool {
        match (self.last_batch, self.store.record(id)) {
            (Some(batch), Some(r)) => batch.saturating_sub(r.last_detected) <= self.max_undetected,
            _ => true,
        }
    }

    /// The subject store (read-only; changes go through this type).
    pub fn store(&self) -> &SubjectStore {
        &self.store
    }

    /// Counters.
    pub fn stats(&self) -> VisionStats {
        self.stats
    }

    /// Sequence number of the newest frame applied.
    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    /// Latest confidence of `id`: the tracker's, or the detection score
    /// before the tracker reported.
    pub fn confidence(&self, id: SubjectId) -> f32 {
        self.confidence.get(&id).copied().unwrap_or_else(|| {
            self.store
                .get(id)
                .map_or(0.0, |s| s.confidence.clamp(0.0, 1.0) as f32)
        })
    }

    /// Applies a new frame: tracker step, track ingestion and store tick.
    /// Frames not newer than the last one are ignored.
    pub fn on_frame(&mut self, frame: &VideoFrame) -> Vec<StoreEvent> {
        if self.last_seq.is_some_and(|s| frame.seq <= s) {
            return Vec::new();
        }
        self.last_seq = Some(frame.seq);
        self.stats.frames += 1;
        let mut observations: Vec<ac_subjects::TrackObservation> = Vec::new();
        for o in self.tracker.update(frame) {
            self.confidence.insert(o.id, o.confidence);
            if self.vouched(o.id) {
                observations.push((o.id, o.bbox, o.confidence).into());
            } else {
                self.stats.unvouched_observations += 1;
            }
        }
        let mut events = self.store.ingest_tracks(frame.pts, &observations);
        events.extend(self.store.tick(frame.pts));
        self.execute(&events, &[frame]);
        events
    }

    /// Applies detections computed on `seen` (the newest frame is `newest`),
    /// keeping only those of `live_queries`.
    pub fn on_detections(
        &mut self,
        seen: &VideoFrame,
        newest: Option<&VideoFrame>,
        detections: &[ac_vision::Detection],
        live_queries: &[QueryId],
    ) -> Vec<StoreEvent> {
        let dets: Vec<ac_subjects::Detection> = detections
            .iter()
            .filter(|d| live_queries.contains(&d.query))
            .map(|d| ac_subjects::Detection {
                query: d.query.clone(),
                bbox: d.bbox,
                score: d.score,
                extent: d.extent,
            })
            .collect();
        self.stats.detection_batches += 1;
        self.last_batch = Some(self.last_batch.map_or(seen.pts, |b| b.max(seen.pts)));
        let events = self.store.ingest_detections(seen.pts, &dets);
        let frames: Vec<&VideoFrame> = std::iter::once(seen).chain(newest).collect();
        self.execute(&events, &frames);
        events
    }

    /// Operator correction on the newest frame `frame`.
    pub fn reacquire(
        &mut self,
        id: SubjectId,
        bbox: Rect,
        frame: &VideoFrame,
    ) -> Result<Vec<StoreEvent>, StoreError> {
        let events = self.store.reacquire(id, bbox, frame.pts)?;
        self.execute(&events, &[frame]);
        Ok(events)
    }

    /// Removes `query` from every subject.
    pub fn remove_query(&mut self, query: &QueryId) -> Vec<StoreEvent> {
        let events = self.store.remove_query(query);
        self.execute(&events, &[]);
        events
    }

    /// Pins or unpins a subject.
    pub fn set_pinned(&mut self, id: SubjectId, pinned: bool) -> Result<(), StoreError> {
        self.store.set_pinned(id, pinned)
    }

    /// Hides or shows a subject.
    pub fn set_hidden(&mut self, id: SubjectId, hidden: bool) -> Result<(), StoreError> {
        self.store.set_hidden(id, hidden)
    }

    /// Replaces the set of on-air subjects.
    pub fn set_protected(&mut self, ids: &std::collections::HashSet<SubjectId>) {
        self.store.set_protected(ids);
    }

    /// Executes the tracker events among `events`. `frames` are the frames
    /// at hand to seed a tracker on, preferred in order of PTS match.
    fn execute(&mut self, events: &[StoreEvent], frames: &[&VideoFrame]) {
        for event in events {
            match event {
                StoreEvent::NeedsTrackerStart { id, bbox, pts } => {
                    let frame = frames
                        .iter()
                        .find(|f| f.pts == *pts)
                        .or_else(|| frames.last());
                    let started = frame.is_some_and(|f| self.tracker.start(f, *id, *bbox).is_ok());
                    if started {
                        self.confidence.insert(*id, 1.0);
                    } else {
                        self.tracker.stop(*id);
                        self.confidence.remove(id);
                        self.store.tracker_failed(*id);
                        self.stats.tracker_start_failures += 1;
                    }
                }
                StoreEvent::NeedsTrackerStop { id } => {
                    self.tracker.stop(*id);
                    self.confidence.remove(id);
                }
                StoreEvent::Created { .. } => self.stats.created += 1,
                StoreEvent::Retired { id, .. } => {
                    self.stats.retired += 1;
                    self.confidence.remove(id);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_sources::SyntheticPattern;

    mod light_cues;

    /// The test source id.
    fn source() -> SourceId {
        SourceId("cam".into())
    }

    /// Vision state with default tuning.
    fn vision() -> SourceVision {
        SourceVision::new(
            source(),
            StoreConfig::default(),
            TemplateTrackerConfig::default(),
            Duration::from_secs(3),
        )
        .unwrap()
    }

    /// Ground-truth detections for `frame`.
    fn truth(p: &SyntheticPattern, frame: &VideoFrame) -> Vec<ac_vision::Detection> {
        p.ground_truth(frame.pts)
            .into_iter()
            .map(|g| ac_vision::Detection {
                query: QueryId("person".into()),
                bbox: g.bbox,
                score: 0.9,
                extent: ac_core::Extent::Person,
            })
            .collect()
    }

    #[test]
    fn detections_create_subjects_and_start_trackers() {
        let p = SyntheticPattern::new(320, 180, 30.0, 2).unwrap();
        let mut v = vision();
        let f0 = p.frame(&source(), 0);
        v.on_frame(&f0);
        let events = v.on_detections(&f0, Some(&f0), &truth(&p, &f0), &[QueryId("person".into())]);
        let created = events
            .iter()
            .filter(|e| matches!(e, StoreEvent::Created { .. }))
            .count();
        assert_eq!(created, 2);
        assert_eq!(v.store().len(), 2);
        assert_eq!(
            v.tracker.tracked().len() as u64 + v.stats().tracker_start_failures,
            2
        );
        assert_eq!(v.confidence(SubjectId(1)), 1.0);
        assert_eq!(v.stats().created, 2);
        let extent = v.store().get(SubjectId(1)).unwrap().extent;
        assert_eq!(extent, ac_core::Extent::Person, "the detector's extent");
    }

    #[test]
    fn detections_of_removed_queries_are_ignored() {
        let p = SyntheticPattern::new(320, 180, 30.0, 2).unwrap();
        let mut v = vision();
        let f0 = p.frame(&source(), 0);
        let events = v.on_detections(&f0, None, &truth(&p, &f0), &[QueryId("other".into())]);
        assert!(events.is_empty());
        assert!(v.store().is_empty());
    }

    #[test]
    fn subjects_are_followed_between_detections() {
        let p = SyntheticPattern::new(320, 180, 30.0, 1).unwrap();
        let mut v = vision();
        let q = [QueryId("person".into())];
        for seq in 0..90 {
            let f = p.frame(&source(), seq);
            v.on_frame(&f);
            if seq % 30 == 0 {
                v.on_detections(&f, Some(&f), &truth(&p, &f), &q);
            }
        }
        let subject = v.store().get(SubjectId(1)).unwrap();
        assert_eq!(subject.state, ac_core::TrackState::Tracking);
        let gt = p.ground_truth(p.frame_rate().pts(89))[0].bbox;
        assert!(subject.bbox.iou(&gt) > 0.3, "{:?} vs {gt:?}", subject.bbox);
        assert_eq!(v.stats().frames, 90);
    }

    #[test]
    fn stale_and_repeated_frames_are_ignored() {
        let p = SyntheticPattern::new(64, 36, 30.0, 1).unwrap();
        let mut v = vision();
        v.on_frame(&p.frame(&source(), 5));
        assert!(v.on_frame(&p.frame(&source(), 5)).is_empty());
        assert!(v.on_frame(&p.frame(&source(), 3)).is_empty());
        assert_eq!(v.stats().frames, 1);
        assert_eq!(v.last_seq(), Some(5));
    }

    #[test]
    fn subjects_without_observations_are_lost_then_reacquired() {
        let p = SyntheticPattern::new(320, 180, 30.0, 1).unwrap();
        let mut v = vision();
        let f0 = p.frame(&source(), 0);
        v.on_detections(&f0, None, &truth(&p, &f0), &[QueryId("person".into())]);
        // A flat grey frame two seconds later: the tracker has nothing to
        // hold on to, and with no detection the subject goes lost.
        let blank = VideoFrame::new(
            source(),
            60,
            Duration::from_secs(2),
            320,
            180,
            vec![128u8; 320 * 180 * 4].into(),
        )
        .unwrap();
        let events = v.on_frame(&blank);
        assert!(events.contains(&StoreEvent::Lost { id: SubjectId(1) }));
        let f = p.frame(&source(), 61);
        let gt = p.ground_truth(f.pts)[0].bbox;
        let events = v.reacquire(SubjectId(1), gt, &f).unwrap();
        assert!(events.contains(&StoreEvent::Recovered { id: SubjectId(1) }));
        assert!(matches!(
            v.reacquire(SubjectId(9), gt, &f),
            Err(StoreError::UnknownSubject(_))
        ));
    }

    #[test]
    fn removing_the_query_retires_and_stops_trackers() {
        let p = SyntheticPattern::new(320, 180, 30.0, 2).unwrap();
        let mut v = vision();
        let f0 = p.frame(&source(), 0);
        let q = QueryId("person".into());
        v.on_detections(&f0, None, &truth(&p, &f0), std::slice::from_ref(&q));
        let events = v.remove_query(&q);
        assert!(v.store().is_empty());
        assert!(v.tracker.tracked().is_empty());
        assert_eq!(v.stats().retired, 2);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StoreEvent::NeedsTrackerStop { .. }))
        );
    }

    #[test]
    fn a_failed_tracker_start_is_retried_by_a_later_detection() {
        let p = SyntheticPattern::new(320, 180, 30.0, 1).unwrap();
        let mut v = vision();
        let q = [QueryId("person".into())];
        // First seen on a flat (faded to grey) frame: the box has no texture,
        // so the tracker cannot start.
        let flat = VideoFrame::new(
            source(),
            0,
            Duration::ZERO,
            320,
            180,
            vec![40u8; 320 * 180 * 4].into(),
        )
        .unwrap();
        let f0 = p.frame(&source(), 0);
        let truth0 = truth(&p, &f0);
        v.on_frame(&flat);
        v.on_detections(&flat, Some(&flat), &truth0, &q);
        let id = SubjectId(1);
        assert_eq!(v.stats().tracker_start_failures, 1);
        assert!(v.tracker.tracked().is_empty());
        assert!(!v.store().record(id).unwrap().tracker_running);
        // The picture comes back and the detector agrees with the store's
        // prediction: the tracker is started rather than the box re-anchored.
        let f1 = p.frame(&source(), 1);
        v.on_frame(&f1);
        let events = v.on_detections(&f1, Some(&f1), &truth(&p, &f1), &q);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StoreEvent::NeedsTrackerStart { id: i, .. } if *i == id)),
            "{events:?}"
        );
        assert_eq!(v.tracker.tracked(), vec![id]);
        assert!(v.store().record(id).unwrap().tracker_running);
        assert_eq!(v.stats().tracker_start_failures, 1);
        // From then on it follows the subject between detections.
        for seq in 2..20 {
            v.on_frame(&p.frame(&source(), seq));
        }
        let gt = p.ground_truth(p.frame_rate().pts(19))[0].bbox;
        let bbox = v.store().get(id).unwrap().bbox;
        assert!(bbox.iou(&gt) > 0.5, "{bbox:?} vs {gt:?}");
    }

    #[test]
    fn invalid_config_is_rejected() {
        let bad = StoreConfig {
            max_subjects: 0,
            ..StoreConfig::default()
        };
        let t = TemplateTrackerConfig::default();
        assert!(SourceVision::new(source(), bad, t, Duration::from_secs(3)).is_err());
    }
}
