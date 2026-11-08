//! The coasting evidence of one record: its tracker report for the current
//! window and the tracker's report on each recent frame, with the decision
//! they make together (see the [parent module](super) for the rule and its
//! reasons).
//!
//! A restart is seeded on the frame the detection was computed on, so
//! whether a coasting tracker should be restarted from a detection depends
//! on that frame: the tracker's own report on it says where the match it
//! held back lay. [`FrameLog`] keeps those reports for the frames an
//! applied detection can still have been computed on, and answers for a
//! detection's PTS with the report of the latest frame at or before it.
//!
//! # Invariants
//!
//! * Log entries are in strictly increasing PTS order. A report for the PTS
//!   of the newest entry replaces it; one older than the newest is ignored
//!   (the engine feeds frames in order).
//! * A run of trusted frames, or of sunk ones, is one entry, the run's
//!   first: the latest entry at or before any PTS in the run says the same.
//!   Held frames each keep their entry, since each says where its match lay.
//! * At most [`CAPACITY`] entries; the oldest goes first. Pruned at every
//!   applied detection to the latest entry at or before its PTS and those
//!   after: detections older than the anchor are not applied, so no later
//!   one can be on an earlier frame. Cleared when the tracker (re)starts or
//!   stops.

use std::collections::VecDeque;
use std::time::Duration;

use ac_core::Rect;

use super::SubjectRecord;

/// Most frame reports a subject keeps: eight seconds of held frames at 30
/// fps, where the log between two applied detections holds at most a pass
/// interval plus its latency of them (runs of trusted or sunk frames take
/// one entry), and a coasting subject is lost `lost_after` (1 s by default)
/// after its latest detection. A detection older than every entry kept
/// finds no report for its frame.
pub(crate) const CAPACITY: usize = 256;

/// What a subject's fast tracker reported since the subject's latest applied
/// detection or tracker (re)start: the coasting evidence (see the
/// [parent module](super)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TrackerReport {
    /// No counted (confident or held) observation yet.
    #[default]
    Silent,
    /// Held observations and no confident one: the contrast check held the
    /// tracker back, and it trusted no match since.
    Coasting,
    /// At least one confident observation.
    Following,
}

/// The tracker's report on one frame of a subject.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FrameReport {
    /// A confident observation: the tracker trusted its match there.
    Trusted,
    /// Below `track_min_confidence` and held back by the tracker's contrast
    /// check, with where the match held back lay when the tracker said so
    /// (a sanitized box).
    Held(Option<Rect>),
    /// Below `track_min_confidence` and not held back: the match's own
    /// score sank, as when something covers the subject.
    Sunk,
}

impl FrameReport {
    /// True when a report of `self` right after one of `prev` adds nothing:
    /// trusted after trusted, sunk after sunk.
    fn continues(self, prev: Self) -> bool {
        matches!(
            (prev, self),
            (Self::Trusted, Self::Trusted) | (Self::Sunk, Self::Sunk)
        )
    }
}

/// Frame reports of one subject's tracker, oldest first; see the
/// [module docs](self).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FrameLog {
    /// `(pts, report)` in increasing PTS order.
    entries: VecDeque<(Duration, FrameReport)>,
}

impl FrameLog {
    /// Records the tracker's `report` on the frame at `pts`.
    pub(crate) fn push(&mut self, pts: Duration, report: FrameReport) {
        if let Some(&(last, prev)) = self.entries.back() {
            if pts < last {
                return;
            }
            if pts == last {
                self.entries.pop_back();
                return self.push(pts, report);
            }
            if report.continues(prev) {
                return;
            }
        }
        if self.entries.len() == CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((pts, report));
    }

    /// The report on the latest frame at or before `pts`, if one is kept.
    pub(crate) fn at(&self, pts: Duration) -> Option<FrameReport> {
        self.entries
            .iter()
            .rev()
            .find(|(t, _)| *t <= pts)
            .map(|&(_, r)| r)
    }

    /// Forgets the frames no detection applied from now on can be on: all
    /// but the latest one at or before `anchor_pts` and those after it.
    pub(crate) fn forget_before(&mut self, anchor_pts: Duration) {
        while self.entries.get(1).is_some_and(|(t, _)| *t <= anchor_pts) {
            self.entries.pop_front();
        }
    }

    /// Forgets every frame (the tracker restarted or stopped).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of entries kept.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

impl SubjectRecord {
    /// Counts the tracker's `report` on its usable observation at `pts` in
    /// the coasting evidence: a trusted one makes the report `Following`
    /// for the rest of the window, a held one makes a `Silent` report
    /// `Coasting`, and a sunk one leaves it alone. Every one is kept in
    /// [`Self::frame_reports`].
    pub(crate) fn note_track(&mut self, pts: Duration, report: FrameReport) {
        self.tracker_report = match (self.tracker_report, report) {
            (_, FrameReport::Trusted) => TrackerReport::Following,
            (TrackerReport::Silent, FrameReport::Held(_)) => TrackerReport::Coasting,
            (window, _) => window,
        };
        self.frame_reports.push(pts, report);
    }

    /// Ends the window for a detection with box `bbox` computed on the frame
    /// at `pts`, applied to the subject, and says whether it calls for a
    /// restart from that frame: the tracker reported a held observation and
    /// no confident one since the previous applied detection or (re)start,
    /// and its report on the detection's frame does not have the match it
    /// held back overlapping `bbox`, but no more than the box predicted at
    /// `pts` does (see the [parent module](super)). Forgets the frame
    /// reports no later detection can be on.
    pub(crate) fn take_coasting(&mut self, bbox: &Rect, pts: Duration) -> bool {
        let coasting = std::mem::take(&mut self.tracker_report) == TrackerReport::Coasting;
        let frame = self.frame_reports.at(pts);
        self.frame_reports.forget_before(pts);
        // What drew the match is partly inside the box the template would
        // be taken from, and the subject is not where the match is.
        let drawn_off = matches!(
            frame,
            Some(FrameReport::Held(Some(at)))
                if at.iou(bbox) > 0.0 && at.iou(bbox) <= self.predicted(pts).iou(bbox)
        );
        coasting && !drawn_off
    }
}

#[cfg(test)]
mod tests {
    use ac_core::{Extent, QueryId, SourceId, SubjectId};

    use super::*;

    /// `s` seconds as a PTS.
    fn at(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    /// A held report whose match lay at `x`.
    fn held_at_x(x: f64) -> FrameReport {
        FrameReport::Held(Some(Rect::new(x, 0.25, 0.125, 0.25)))
    }

    /// A tracking subject admitted at `bbox` at t = 0.
    fn record(bbox: Rect) -> SubjectRecord {
        SubjectRecord::new(
            SubjectId(1),
            SourceId("cam".into()),
            QueryId("person".into()),
            bbox,
            Extent::Object,
            0.9,
            Duration::ZERO,
        )
    }

    #[test]
    fn answers_with_the_latest_frame_at_or_before_a_pts() {
        let mut log = FrameLog::default();
        assert_eq!(log.at(at(1.0)), None);
        log.push(at(0.1), FrameReport::Trusted);
        log.push(at(0.2), FrameReport::Trusted);
        log.push(at(0.3), held_at_x(0.5));
        log.push(at(0.4), FrameReport::Sunk);
        log.push(at(0.5), FrameReport::Sunk);
        log.push(at(0.6), held_at_x(0.6));
        // The runs of trusted and sunk frames take one entry each.
        assert_eq!(log.len(), 4);
        assert_eq!(log.at(at(0.05)), None);
        assert_eq!(log.at(at(0.25)), Some(FrameReport::Trusted));
        assert_eq!(log.at(at(0.3)), Some(held_at_x(0.5)));
        assert_eq!(log.at(at(0.5)), Some(FrameReport::Sunk));
        assert_eq!(log.at(at(9.0)), Some(held_at_x(0.6)));
    }

    #[test]
    fn a_report_for_the_newest_frame_replaces_it_and_older_ones_are_ignored() {
        let mut log = FrameLog::default();
        log.push(at(0.1), FrameReport::Trusted);
        log.push(at(0.2), held_at_x(0.5));
        log.push(at(0.2), held_at_x(0.6));
        log.push(at(0.15), FrameReport::Sunk);
        assert_eq!(log.len(), 2);
        assert_eq!(log.at(at(0.15)), Some(FrameReport::Trusted));
        assert_eq!(log.at(at(0.2)), Some(held_at_x(0.6)));
        // A replacement that continues the run before it merges into it.
        log.push(at(0.2), FrameReport::Trusted);
        assert_eq!(log.len(), 1);
        assert_eq!(log.at(at(0.2)), Some(FrameReport::Trusted));
    }

    #[test]
    fn keeps_what_a_later_detection_can_be_on_and_no_more_than_its_capacity() {
        let mut log = FrameLog::default();
        for i in 0..10 {
            log.push(at(f64::from(i) * 0.1), held_at_x(f64::from(i) * 0.01));
        }
        // A detection at 0.45 can still be followed by one on the frame at
        // 0.4 or later.
        log.forget_before(at(0.45));
        assert_eq!(log.len(), 6);
        assert_eq!(log.at(at(0.45)), Some(held_at_x(0.04)));
        assert_eq!(log.at(at(0.35)), None);
        log.forget_before(at(9.0));
        assert_eq!(log.len(), 1);
        log.clear();
        assert_eq!(log.at(at(9.0)), None);
        for i in 0..CAPACITY + 5 {
            log.push(at(i as f64), held_at_x(0.5));
        }
        assert_eq!(log.len(), CAPACITY);
        assert_eq!(log.at(at(4.0)), None);
        assert_eq!(log.at(at(5.0)), Some(held_at_x(0.5)));
    }

    #[test]
    fn coasting_evidence_needs_held_observations_only() {
        let mut r = record(Rect::FULL);
        let held = FrameReport::Held(None);
        let take = |r: &mut SubjectRecord, s: f64| r.take_coasting(&Rect::FULL, at(s));
        // Nothing reported: no evidence.
        assert!(!take(&mut r, 0.1));
        // Held observations: coasting, and since they do not say where
        // their match lay, an agreeing detection restarts.
        r.note_track(at(0.2), held);
        r.note_track(at(0.3), held);
        assert_eq!(r.tracker_report, TrackerReport::Coasting);
        assert!(take(&mut r, 0.3));
        // Taking it ends the window.
        assert_eq!(r.tracker_report, TrackerReport::Silent);
        assert!(!take(&mut r, 0.4));
        // One confident observation, before or after held ones, is enough
        // to keep the window from counting as coasting.
        let trusted = FrameReport::Trusted;
        for (start, order) in [(1.0, [trusted, held, held]), (2.0, [held, held, trusted])] {
            for (i, report) in order.into_iter().enumerate() {
                r.note_track(at(start + 0.1 * i as f64), report);
            }
            assert_eq!(r.tracker_report, TrackerReport::Following);
            assert!(!take(&mut r, start + 0.2));
        }
        // Sunk observations do not start a window either.
        r.note_track(at(3.0), FrameReport::Sunk);
        assert_eq!(r.tracker_report, TrackerReport::Silent);
        // A fresh start ends the window and forgets the frames.
        r.note_track(at(3.1), held);
        r.reseed(Rect::FULL, at(3.2));
        assert_eq!(r.frame_reports, FrameLog::default());
        assert!(!take(&mut r, 3.2));
    }

    #[test]
    fn a_coasting_restart_reads_the_tracker_s_report_on_the_detection_s_frame() {
        // Exact binary fractions, so that the tie below is exact.
        let at_x = |x: f64| Rect::new(x, 0.25, 0.125, 0.25);
        let (coasting, between, sidestep, lookalike, clear) = (
            at_x(0.25),
            at_x(0.3125),
            at_x(0.375),
            at_x(0.1875),
            at_x(0.5),
        );
        let held = |at: Rect| FrameReport::Held(Some(at));
        let mut r = record(coasting);
        // The performer stepped to where the match was held back on the
        // detection's frame: restart.
        r.note_track(at(0.1), held(sidestep));
        assert!(r.take_coasting(&sidestep, at(0.1)));
        // A lookalike drew the match on that frame and the subject stayed
        // where its box was: no restart, and the window ends all the same.
        r.note_track(at(0.2), held(lookalike));
        assert!(!r.take_coasting(&coasting, at(0.2)));
        assert_eq!(r.tracker_report, TrackerReport::Silent);
        // A detection that overlaps both as much is no reason either.
        r.note_track(at(0.3), held(sidestep));
        assert_eq!(between.iou(&sidestep), between.iou(&coasting));
        assert!(!r.take_coasting(&between, at(0.3)));
        // A match held back clear of the detection did not draw on the box
        // the template would be taken from, wherever the subject is: restart.
        r.note_track(at(0.35), held(clear));
        assert_eq!(clear.iou(&coasting), 0.0);
        assert!(r.take_coasting(&coasting, at(0.35)));
        // The detection's own frame decides, not the newest one: the
        // latest frame at or before its PTS. (The newest match, clear of the
        // detection, would restart.)
        r.note_track(at(0.4), held(lookalike));
        r.note_track(at(0.5), held(sidestep));
        assert!(!r.take_coasting(&coasting, at(0.45)));
        r.note_track(at(0.6), held(sidestep));
        assert!(r.take_coasting(&sidestep, at(0.55)));
        // A trusted frame shows the subject as the template knows it: a
        // late detection from just before the tracker was held back
        // restarts it from there, wherever the later matches lay.
        r.note_track(at(1.0), FrameReport::Trusted);
        assert!(!r.take_coasting(&coasting, at(0.9)));
        r.note_track(at(1.1), held(lookalike));
        r.note_track(at(1.2), held(sidestep));
        assert!(r.take_coasting(&coasting, at(1.0)));
        // A frame on which the match's own score sank, one held back
        // without a position, or one no report is kept for, says nothing
        // about where the match lay: the held window restarts, as it did
        // before positions were reported.
        r.note_track(at(1.3), held(lookalike));
        r.note_track(at(1.4), FrameReport::Sunk);
        assert!(r.take_coasting(&coasting, at(1.4)));
        r.note_track(at(1.5), FrameReport::Held(None));
        assert!(r.take_coasting(&coasting, at(1.5)));
        r.reseed(coasting, at(1.6));
        r.note_track(at(1.7), held(lookalike));
        assert!(r.take_coasting(&coasting, at(1.6)));
        // A confident observation still ends the case for a restart.
        r.note_track(at(1.8), held(sidestep));
        r.note_track(at(1.9), FrameReport::Trusted);
        assert!(!r.take_coasting(&sidestep, at(1.8)));
    }
}
