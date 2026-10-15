//! A subject as the store keeps it: the shared [`Subject`] plus bookkeeping.
//!
//! [`ac_core::Subject`] is the part every other crate needs (id, query, box,
//! state, pin). [`SubjectRecord`] wraps it with what only identity management
//! needs: the alias queries, operator flags, observation times and a small
//! motion model.
//!
//! # Motion model
//!
//! Detections arrive slowly (0.5–2 Hz) and late, so the store compares a new
//! detection with where the subject *probably is at the detection's PTS*, not
//! with its last box. Each record keeps:
//!
//! * the current box and the PTS of the observation that set it;
//! * the last **anchor**: the most recent authoritative box (a detection, the
//!   creating detection, or an operator reacquire) and its PTS;
//! * a centre velocity, smoothed with a time-aware exponential filter
//!   (`alpha = 1 - exp(-dt / tau)`), so 30 Hz tracker jitter averages out
//!   while a 2 s gap between detections still yields a usable estimate.
//!
//! A detection is compared with both references (see
//! [`crate::associate`]): its IoU with the constant-velocity prediction
//! keeps identities apart when two subjects cross, and its IoU with the
//! anchor recovers a subject whose fast tracker drifted onto someone else
//! while the person stood still. Predictions are limited to one second
//! (`MAX_PREDICTION`) either side of the box's PTS, and lost subjects do not
//! move at all.

use std::collections::BTreeSet;
use std::time::Duration;

use ac_core::{Point, QueryId, Rect, SourceId, Subject, SubjectId, TrackState};

/// Time constant of the velocity filter, in seconds.
const VELOCITY_TIME_CONSTANT: f64 = 0.3;
/// Largest time span a box is extrapolated over, in seconds.
pub(crate) const MAX_PREDICTION: f64 = 1.0;
/// Observations further apart than this, in seconds, reset the velocity.
const MAX_VELOCITY_GAP: f64 = 2.0;
/// Speed limit per axis, in source widths (or heights) per second.
const MAX_SPEED: f64 = 2.0;

/// Motion state of one subject.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Motion {
    /// PTS of the observation that set the current box.
    pub(crate) box_pts: Duration,
    /// Last authoritative box.
    pub(crate) anchor: Rect,
    /// PTS of `anchor`.
    pub(crate) anchor_pts: Duration,
    /// Smoothed centre velocity in normalized units per second.
    pub(crate) velocity: Point,
}

/// A subject plus the store's bookkeeping about it.
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectRecord {
    /// The shared view: id, source, primary query, box, confidence, state and
    /// pin.
    pub subject: Subject,
    /// Other queries this subject answers, sorted; never contains the primary
    /// query. Detections of these queries continue the subject instead of
    /// creating a new one.
    pub aliases: BTreeSet<QueryId>,
    /// Operator hid the subject from the shot list. It is still tracked, so
    /// the same entity is not re-admitted as a new visible subject, and it is
    /// never retired automatically.
    pub hidden: bool,
    /// On program or preview (from the switcher): never retired, evicted or
    /// handed to another entity.
    pub protected: bool,
    /// The primary query was removed while the subject was protected. It
    /// keeps tracking for the on-air shot and is retired as soon as it is no
    /// longer protected. Orphans never match detections.
    pub orphaned: bool,
    /// The subject's fast tracker is running (per the tracker events).
    pub tracker_running: bool,
    /// PTS at which the subject was admitted.
    pub created_at: Duration,
    /// PTS of the latest confident observation of any kind.
    pub last_seen: Duration,
    /// PTS of the latest detection that matched the subject.
    pub last_detected: Duration,
    /// PTS at which the subject became lost, while it is lost.
    pub lost_since: Option<Duration>,
    /// Motion model.
    pub(crate) motion: Motion,
    /// First detection batch at which the subject was unmatched while sitting
    /// on top of a matched subject (a likely duplicate), until that ends.
    pub(crate) shadowed_since: Option<Duration>,
}

/// Seconds of `pts` as a float, for signed time arithmetic.
fn secs(pts: Duration) -> f64 {
    pts.as_secs_f64()
}

impl SubjectRecord {
    /// A tracking subject admitted from a detection at `pts`.
    pub(crate) fn new(
        id: SubjectId,
        source: SourceId,
        query: QueryId,
        bbox: Rect,
        score: f32,
        pts: Duration,
    ) -> Self {
        Self {
            subject: Subject {
                id,
                source,
                query,
                bbox,
                confidence: f64::from(score),
                state: TrackState::Tracking,
                pinned: false,
            },
            aliases: BTreeSet::new(),
            hidden: false,
            protected: false,
            orphaned: false,
            tracker_running: true,
            created_at: pts,
            last_seen: pts,
            last_detected: pts,
            lost_since: None,
            motion: Motion {
                box_pts: pts,
                anchor: bbox,
                anchor_pts: pts,
                velocity: Point::default(),
            },
            shadowed_since: None,
        }
    }

    /// The subject's id.
    pub fn id(&self) -> SubjectId {
        self.subject.id
    }

    /// True while the subject is tracking (not lost).
    pub fn is_tracking(&self) -> bool {
        self.subject.state == TrackState::Tracking
    }

    /// True when automatic retirement (expiry or eviction) must skip the
    /// subject: it is protected, pinned or hidden.
    pub fn is_retained(&self) -> bool {
        self.protected || self.subject.pinned || self.hidden
    }

    /// True when the subject answers `query`, as primary query or alias.
    pub fn answers(&self, query: &QueryId) -> bool {
        self.subject.query == *query || self.aliases.contains(query)
    }

    /// Every query the subject answers: the primary first, then the aliases
    /// in sorted order.
    pub fn queries(&self) -> impl Iterator<Item = &QueryId> {
        std::iter::once(&self.subject.query).chain(&self.aliases)
    }

    /// Smoothed centre velocity, in normalized units per second.
    pub fn velocity(&self) -> Point {
        self.motion.velocity
    }

    /// Where the box probably is at `pts`, by constant-velocity extrapolation.
    pub(crate) fn predicted(&self, pts: Duration) -> Rect {
        if !self.is_tracking() {
            return self.subject.bbox;
        }
        let dt = (secs(pts) - secs(self.motion.box_pts)).clamp(-MAX_PREDICTION, MAX_PREDICTION);
        let v = self.motion.velocity;
        Rect {
            x: self.subject.bbox.x + v.x * dt,
            y: self.subject.bbox.y + v.y * dt,
            ..self.subject.bbox
        }
    }

    /// Association evidence for a box observed at `pts`: its IoU with the
    /// predicted box and its IoU with the anchor.
    pub(crate) fn evidence(&self, bbox: &Rect, pts: Duration) -> (f64, f64) {
        (
            bbox.iou(&self.predicted(pts)),
            bbox.iou(&self.motion.anchor),
        )
    }

    /// Moves the box to an observation at `pts` that continues the current
    /// motion, updating the velocity estimate.
    pub(crate) fn move_to(&mut self, bbox: Rect, pts: Duration) {
        let dt = secs(pts) - secs(self.motion.box_pts);
        if dt > MAX_VELOCITY_GAP {
            self.motion.velocity = Point::default();
        } else if dt > 0.0 {
            let (old, new) = (self.subject.bbox.center(), bbox.center());
            let alpha = 1.0 - (-dt / VELOCITY_TIME_CONSTANT).exp();
            let v = &mut self.motion.velocity;
            let vx = ((new.x - old.x) / dt).clamp(-MAX_SPEED, MAX_SPEED);
            let vy = ((new.y - old.y) / dt).clamp(-MAX_SPEED, MAX_SPEED);
            v.x += alpha * (vx - v.x);
            v.y += alpha * (vy - v.y);
        }
        self.subject.bbox = bbox;
        self.motion.box_pts = self.motion.box_pts.max(pts);
    }

    /// Makes `bbox` at `pts` the new anchor and current box, continuing the
    /// motion estimate.
    pub(crate) fn reanchor(&mut self, bbox: Rect, pts: Duration) {
        self.move_to(bbox, pts);
        self.motion.anchor = bbox;
        self.motion.anchor_pts = pts;
    }

    /// Jumps to `bbox` at `pts`: a fresh start for the box, the anchor and
    /// the motion estimate (tracker restart, recovery or operator fix).
    pub(crate) fn reseed(&mut self, bbox: Rect, pts: Duration) {
        self.subject.bbox = bbox;
        self.motion = Motion {
            box_pts: pts,
            anchor: bbox,
            anchor_pts: pts,
            velocity: Point::default(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(bbox: Rect) -> SubjectRecord {
        SubjectRecord::new(
            SubjectId(1),
            SourceId("cam".into()),
            QueryId("person".into()),
            bbox,
            0.9,
            Duration::ZERO,
        )
    }

    fn at(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn velocity_converges_to_constant_motion() {
        let mut r = record(Rect::new(0.1, 0.4, 0.1, 0.2));
        for i in 1..=30 {
            let t = i as f64 / 30.0;
            r.move_to(Rect::new(0.1 + 0.2 * t, 0.4, 0.1, 0.2), at(t));
        }
        assert!((r.velocity().x - 0.2).abs() < 0.01, "{:?}", r.velocity());
        assert!(r.velocity().y.abs() < 1e-9);
        let p = r.predicted(at(1.5));
        assert!((p.x - (0.3 + 0.2 * 0.5)).abs() < 0.01);
    }

    #[test]
    fn prediction_is_capped_and_symmetric() {
        let mut r = record(Rect::new(0.1, 0.4, 0.1, 0.2));
        r.motion.velocity = Point { x: 0.1, y: 0.0 };
        let far = r.predicted(at(10.0));
        assert!((far.x - (0.1 + 0.1 * MAX_PREDICTION)).abs() < 1e-12);
        r.motion.box_pts = at(2.0);
        let back = r.predicted(at(1.5));
        assert!((back.x - 0.05).abs() < 1e-12);
    }

    #[test]
    fn lost_subjects_do_not_move() {
        let mut r = record(Rect::new(0.1, 0.4, 0.1, 0.2));
        r.motion.velocity = Point { x: 0.5, y: 0.5 };
        r.subject.state = TrackState::Lost;
        assert_eq!(r.predicted(at(0.5)), r.subject.bbox);
    }

    #[test]
    fn long_gaps_and_outliers_do_not_produce_wild_velocities() {
        let mut r = record(Rect::new(0.1, 0.4, 0.1, 0.2));
        r.move_to(Rect::new(0.8, 0.4, 0.1, 0.2), at(1.0 / 30.0));
        assert!(r.velocity().x <= MAX_SPEED);
        r.move_to(Rect::new(0.2, 0.4, 0.1, 0.2), at(10.0));
        assert_eq!(r.velocity(), Point::default());
        // Same-PTS updates leave the estimate alone.
        r.move_to(Rect::new(0.3, 0.4, 0.1, 0.2), at(10.0));
        assert_eq!(r.velocity(), Point::default());
    }

    #[test]
    fn evidence_compares_with_prediction_and_anchor() {
        let mut r = record(Rect::new(0.1, 0.4, 0.1, 0.2));
        // The tracker drifted far away; the anchor still sits at the start.
        r.move_to(Rect::new(0.6, 0.4, 0.1, 0.2), at(0.5));
        r.motion.velocity = Point::default();
        let (p, a) = r.evidence(&Rect::new(0.1, 0.4, 0.1, 0.2), at(0.6));
        assert!(p == 0.0 && a > 0.99);
        let (p, a) = r.evidence(&Rect::new(0.6, 0.4, 0.1, 0.2), at(0.6));
        assert!(p > 0.99 && a == 0.0);
        let (p, a) = r.evidence(&Rect::new(0.35, 0.4, 0.1, 0.2), at(0.6));
        assert!(p == 0.0 && a == 0.0);
        // Re-anchoring moves both references.
        r.reanchor(Rect::new(0.35, 0.4, 0.1, 0.2), at(0.7));
        let (p, a) = r.evidence(&Rect::new(0.35, 0.4, 0.1, 0.2), at(0.7));
        assert!(p > 0.99 && a > 0.99);
    }

    #[test]
    fn queries_list_primary_then_sorted_aliases() {
        let mut r = record(Rect::FULL);
        r.aliases.insert(QueryId("speaker".into()));
        r.aliases.insert(QueryId("guitarist".into()));
        let q: Vec<&str> = r.queries().map(|q| q.0.as_str()).collect();
        assert_eq!(q, ["person", "guitarist", "speaker"]);
        assert!(r.answers(&QueryId("speaker".into())));
        assert!(!r.answers(&QueryId("drum".into())));
    }
}
