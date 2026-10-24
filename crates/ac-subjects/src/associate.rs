//! Detection association: which detection continues which subject.
//!
//! A detection batch is every box the slow detector produced on the frame at
//! one PTS. It is processed in fixed phases, each deterministic:
//!
//! 1. **Clean.** Boxes are [sanitized](crate::observation) and detections
//!    scoring below `min_score` are dropped.
//! 2. **Primary match.** For each query (in sorted order), detections are
//!    matched to subjects whose primary query it is with the gated Hungarian
//!    method (`crate::assignment`). Each pair has two pieces of evidence
//!    ([`crate::record`]): `p`, the IoU with the subject's predicted box, and
//!    `a`, the IoU with its anchor. A pair is allowed when `max(p, a)`
//!    reaches `match_iou`, and costs `1 - (0.75 p + 0.25 a)`: motion decides,
//!    and the anchor breaks ties, so a twin whose tracker drifted onto
//!    someone loses that person to the subject whose anchor is there too.
//!    Lost subjects pay a small penalty so that, when a detection overlaps a
//!    tracking and a lost subject about equally, the tracking one keeps it.
//! 3. **Alias match.** Detections still unmatched are matched the same way to
//!    unmatched subjects that answer the query through an alias.
//! 4. **Aliasing.** A detection still unmatched that is the same entity as a
//!    subject is that subject seen through another prompt: its query becomes
//!    an alias and, if the subject was not observed yet in this batch, the
//!    detection counts as an observation of it. A box is the same entity as
//!    a subject when it overlaps above `alias_iou` either the subject's
//!    predicted box or the box another query's detection applied to it in
//!    this same batch. The second reference matters for late results: the
//!    tracker may hold a newer, differently sized box that the detection
//!    agrees with but does not replace, and the identical boxes one pass
//!    gives one entity for several prompts must still alias. A detection of
//!    the *same* query there is a duplicate box and is dropped.
//! 5. **Shadows.** An unprotected tracking subject left unmatched while it
//!    sits above `alias_iou` on a matched subject is probably a duplicate (its
//!    tracker drifted onto someone else). If that lasts for `lost_after` it
//!    becomes lost, conservatively, rather than continuing as a twin.
//! 6. **Admission.** The remaining detections may create subjects. Each slot
//!    goes to the query with the fewest subjects so far (ties by query id),
//!    which takes its best-scoring candidate: round-robin across prompts in
//!    score order, so one prompt cannot take every slot. At capacity, the
//!    longest-lost subject that is not retained is evicted to make room;
//!    without one, the candidate is dropped.
//!
//! A matched detection is authoritative. If it overlaps the subject's current
//! box by less than `reseed_iou`, the fast tracker drifted: the box jumps to
//! the detection and the tracker is restarted there. Otherwise the detection
//! becomes the new anchor and box, unless the tracker has already reported a
//! newer box that agrees with it, which is kept because it is more recent.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use ac_core::{QueryId, Rect, SubjectId, TrackState};

use crate::assignment::match_pairs;
use crate::event::{RetireReason, StoreEvent};
use crate::observation::{Detection, sanitize};
use crate::record::SubjectRecord;
use crate::store::SubjectStore;

/// Extra matching cost for a lost subject, in IoU units.
const LOST_PENALTY: f64 = 0.1;
/// Weight of the prediction (against the anchor) in the matching cost.
const PREDICTION_WEIGHT: f64 = 0.75;

/// A detection that passed cleaning.
#[derive(Debug, Clone)]
struct Clean<'a> {
    /// The detection's query.
    query: &'a QueryId,
    /// The sanitized box.
    bbox: Rect,
    /// Detector confidence.
    score: f32,
}

/// Per-batch matching state shared by the phases.
#[derive(Debug, Default)]
struct Batch {
    /// Detections already consumed, by index into the clean list.
    used: Vec<bool>,
    /// Subjects observed in this batch.
    observed: BTreeSet<SubjectId>,
    /// The box of this batch's detection that each subject was matched,
    /// aliased, recovered or admitted with (the first one, when several
    /// detections reached it). Aliasing compares with it besides the
    /// predicted box, because a late result leaves the tracker's newer box
    /// in place.
    applied: BTreeMap<SubjectId, Rect>,
    /// Events in the order they happened.
    events: Vec<StoreEvent>,
}

impl SubjectStore {
    /// Associates the detections made on the frame at `pts` with subjects,
    /// admitting new subjects for unmatched ones while there is room.
    ///
    /// See the [module documentation](crate::associate) for the phases.
    pub fn ingest_detections(&mut self, pts: Duration, dets: &[Detection]) -> Vec<StoreEvent> {
        let min_score = self.config.min_score;
        let clean: Vec<Clean<'_>> = dets
            .iter()
            .filter(|d| d.score >= min_score)
            .filter_map(|d| {
                Some(Clean {
                    query: &d.query,
                    bbox: sanitize(&d.bbox)?,
                    score: d.score,
                })
            })
            .collect();
        let mut by_query: BTreeMap<&QueryId, Vec<usize>> = BTreeMap::new();
        for (i, d) in clean.iter().enumerate() {
            by_query.entry(d.query).or_default().push(i);
        }
        let mut batch = Batch {
            used: vec![false; clean.len()],
            ..Batch::default()
        };
        for (query, idxs) in &by_query {
            let subjects = self.candidates(&batch, |r| r.subject.query == **query);
            self.match_group(pts, &clean, idxs, &subjects, &mut batch);
        }
        for (query, idxs) in &by_query {
            let subjects = self.candidates(&batch, |r| r.aliases.contains(*query));
            self.match_group(pts, &clean, idxs, &subjects, &mut batch);
        }
        let mut fresh = Vec::new();
        for (i, det) in clean.iter().enumerate() {
            if batch.used[i] {
                continue;
            }
            if let Some(id) = self.overlapping(&det.bbox, pts, &batch) {
                self.alias(id, det, pts, &mut batch);
            } else {
                fresh.push(i);
            }
        }
        self.update_shadows(pts, &mut batch);
        self.admit(pts, &clean, fresh, &mut batch);
        batch.events
    }

    /// Ids of subjects not yet observed in `batch`, not orphaned, for which
    /// `wanted` holds, in id order.
    fn candidates(&self, batch: &Batch, wanted: impl Fn(&SubjectRecord) -> bool) -> Vec<SubjectId> {
        self.subjects
            .values()
            .filter(|r| !r.orphaned && !batch.observed.contains(&r.id()) && wanted(r))
            .map(SubjectRecord::id)
            .collect()
    }

    /// Optimal gated matching of the unused detections `idxs` to `subjects`.
    fn match_group(
        &mut self,
        pts: Duration,
        clean: &[Clean<'_>],
        idxs: &[usize],
        subjects: &[SubjectId],
        batch: &mut Batch,
    ) {
        let rows: Vec<usize> = idxs.iter().copied().filter(|&i| !batch.used[i]).collect();
        let gate = self.config.match_iou;
        let pairs = match_pairs(rows.len(), subjects.len(), |r, c| {
            let rec = &self.subjects[&subjects[c]];
            let (predicted, anchor) = rec.evidence(&clean[rows[r]].bbox, pts);
            let score = PREDICTION_WEIGHT * predicted + (1.0 - PREDICTION_WEIGHT) * anchor;
            let penalty = if rec.is_tracking() { 0.0 } else { LOST_PENALTY };
            (predicted.max(anchor) >= gate).then_some(1.0 - score + penalty)
        });
        for (r, c) in pairs {
            batch.used[rows[r]] = true;
            self.observe(subjects[c], &clean[rows[r]], pts, batch);
        }
    }

    /// The non-orphan subject overlapping `bbox` the most above `alias_iou`
    /// (ties to the lower id).
    ///
    /// A box is the same entity as a subject when it overlaps above
    /// `alias_iou` either the subject's predicted box at `pts` or the box
    /// another query's detection applied to it in this same pass
    /// (`batch.applied`): the tracker may hold a newer, differently sized
    /// box.
    fn overlapping(&self, bbox: &Rect, pts: Duration, batch: &Batch) -> Option<SubjectId> {
        let mut best: Option<(f64, SubjectId)> = None;
        for rec in self.subjects.values().filter(|r| !r.orphaned) {
            let same_pass = batch.applied.get(&rec.id()).map_or(0.0, |b| bbox.iou(b));
            let iou = bbox.iou(&rec.predicted(pts)).max(same_pass);
            if iou > self.config.alias_iou && best.is_none_or(|(b, _)| iou > b) {
                best = Some((iou, rec.id()));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Phase 4: `det` overlaps subject `id` above `alias_iou`.
    fn alias(&mut self, id: SubjectId, det: &Clean<'_>, pts: Duration, batch: &mut Batch) {
        let rec = self
            .subjects
            .get_mut(&id)
            .expect("overlapping returns live ids");
        if rec.answers(det.query) {
            // A second box of the same query on the same entity.
            return;
        }
        rec.aliases.insert(det.query.clone());
        batch.events.push(StoreEvent::QueriesChanged { id });
        if !batch.observed.contains(&id) {
            self.observe(id, det, pts, batch);
        }
    }

    /// Applies a detection that matched subject `id`.
    fn observe(&mut self, id: SubjectId, det: &Clean<'_>, pts: Duration, batch: &mut Batch) {
        batch.observed.insert(id);
        batch.applied.insert(id, det.bbox);
        let reseed_iou = self.config.reseed_iou;
        let rec = self.subjects.get_mut(&id).expect("matched ids are live");
        rec.subject.confidence = f64::from(det.score);
        rec.last_seen = rec.last_seen.max(pts);
        rec.last_detected = rec.last_detected.max(pts);
        rec.shadowed_since = None;
        let events = &mut batch.events;
        if !rec.is_tracking() {
            rec.reseed(det.bbox, pts);
            rec.subject.state = TrackState::Tracking;
            rec.lost_since = None;
            rec.tracker_running = true;
            events.push(StoreEvent::Recovered { id });
            events.push(StoreEvent::NeedsTrackerStart {
                id,
                bbox: det.bbox,
                pts,
            });
            return;
        }
        events.push(StoreEvent::Updated { id });
        if pts < rec.motion.anchor_pts {
            // Older than the anchor we hold (out-of-order result, or from
            // before an operator fix): it confirms the subject, nothing more.
            return;
        }
        let agrees = rec.predicted(pts).iou(&det.bbox) >= reseed_iou;
        if !agrees || !rec.tracker_running {
            rec.reseed(det.bbox, pts);
            rec.tracker_running = true;
            events.push(StoreEvent::NeedsTrackerStart {
                id,
                bbox: det.bbox,
                pts,
            });
        } else if pts >= rec.motion.box_pts {
            rec.reanchor(det.bbox, pts);
        } else {
            rec.motion.anchor = det.bbox;
            rec.motion.anchor_pts = pts;
        }
    }

    /// Phase 5: starts, continues or clears each subject's shadow timer, and
    /// demotes subjects that shadowed a matched one for `lost_after`.
    fn update_shadows(&mut self, pts: Duration, batch: &mut Batch) {
        let alias_iou = self.config.alias_iou;
        let lost_after = self.config.lost_after;
        let observed: Vec<Rect> = batch
            .observed
            .iter()
            .map(|id| self.subjects[id].subject.bbox)
            .collect();
        let mut demote = Vec::new();
        for rec in self.subjects.values_mut() {
            if batch.observed.contains(&rec.id())
                || !rec.is_tracking()
                || rec.protected
                || rec.orphaned
            {
                continue;
            }
            let here = rec.predicted(pts);
            if observed.iter().any(|b| b.iou(&here) > alias_iou) {
                let since = *rec.shadowed_since.get_or_insert(pts);
                if pts >= since.saturating_add(lost_after) {
                    demote.push(rec.id());
                }
            } else {
                rec.shadowed_since = None;
            }
        }
        for id in demote {
            self.lose(id, pts, &mut batch.events);
        }
    }

    /// Phase 6: admits the `fresh` detections fairly across queries.
    fn admit(&mut self, pts: Duration, clean: &[Clean<'_>], fresh: Vec<usize>, batch: &mut Batch) {
        let mut queues: BTreeMap<&QueryId, VecDeque<usize>> = BTreeMap::new();
        for i in fresh {
            queues.entry(clean[i].query).or_default().push_back(i);
        }
        for queue in queues.values_mut() {
            // Best score first; ties keep detector order (stable sort).
            queue
                .make_contiguous()
                .sort_by(|&a, &b| clean[b].score.total_cmp(&clean[a].score));
        }
        let mut counts: BTreeMap<&QueryId, usize> = queues
            .keys()
            .map(|&q| {
                let n = self.subjects.values().filter(|r| r.subject.query == *q);
                (q, n.count())
            })
            .collect();
        loop {
            let next = queues
                .iter()
                .filter(|(_, queue)| !queue.is_empty())
                .map(|(&q, _)| q)
                .min_by_key(|&q| (counts[q], q));
            let Some(query) = next else {
                break;
            };
            let i = queues
                .get_mut(query)
                .and_then(VecDeque::pop_front)
                .expect("queue is non-empty");
            let det = &clean[i];
            if let Some(id) = self.overlapping(&det.bbox, pts, batch) {
                self.alias(id, det, pts, batch);
                continue;
            }
            let exhausted = self.next_id > u64::from(u32::MAX);
            let full = self.subjects.len() >= self.config.max_subjects;
            if exhausted || (full && !self.evict(batch)) {
                continue;
            }
            if self.create(det, pts, batch) {
                *counts.entry(query).or_default() += 1;
            }
        }
    }

    /// Retires the longest-lost subject that is not retained; false when
    /// there is none.
    fn evict(&mut self, batch: &mut Batch) -> bool {
        let victim = self
            .subjects
            .values()
            .filter(|r| !r.is_tracking() && !r.is_retained() && !r.orphaned)
            .min_by_key(|r| (r.lost_since, r.id()))
            .map(SubjectRecord::id);
        match victim {
            Some(id) => {
                self.retire(id, RetireReason::Evicted, &mut batch.events);
                true
            }
            None => false,
        }
    }

    /// Admits a new subject for `det`; false once ids are exhausted.
    fn create(&mut self, det: &Clean<'_>, pts: Duration, batch: &mut Batch) -> bool {
        let Ok(raw) = u32::try_from(self.next_id) else {
            return false;
        };
        self.next_id += 1;
        let id = SubjectId(raw);
        let rec = SubjectRecord::new(
            id,
            self.source.clone(),
            det.query.clone(),
            det.bbox,
            det.score,
            pts,
        );
        self.subjects.insert(id, rec);
        batch.observed.insert(id);
        batch.applied.insert(id, det.bbox);
        batch
            .events
            .push(StoreEvent::Created { id, bbox: det.bbox });
        batch.events.push(StoreEvent::NeedsTrackerStart {
            id,
            bbox: det.bbox,
            pts,
        });
        true
    }
}

#[cfg(test)]
mod tests;
