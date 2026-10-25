//! Inputs to the store: slow detections and fast track observations.
//!
//! Both arrive tagged with the media timestamp of the frame they were
//! computed on (passed alongside, not inside, so one batch shares one PTS).
//! They are untrusted numbers from models: every box is checked with
//! `sanitize` before it can touch a subject. A box must be finite and have a
//! positive area; a box that sticks out of the frame (detectors do this at the
//! edges) is clipped to it, and one entirely outside is dropped.
//!
//! The store does not depend on the vision crate. The engine converts the
//! detector's output into [`Detection`]s and the tracker's into
//! [`TrackObservation`]s, which keep whether the tracker held its match
//! back. A tuple `(id, bbox, confidence)` converts to an observation that
//! was not held back.
//!
//! A detection says what its box covers ([`Extent`]); a track observation
//! does not, because the tracker follows the box it was started on at that
//! box's scale, so the subject keeps the extent of its newest detection.

use ac_core::{Extent, QueryId, Rect, SubjectId};
use serde::{Deserialize, Serialize};

/// One box from the slow open-vocabulary detector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    /// Query (prompt) that produced the box.
    pub query: QueryId,
    /// Box in normalized source coordinates.
    pub bbox: Rect,
    /// Detector confidence in `0.0..=1.0`.
    pub score: f32,
    /// What `bbox` covers. Absent in older data, where it reads as
    /// [`Extent::Object`].
    #[serde(default)]
    pub extent: Extent,
}

/// One box from a subject's fast per-frame tracker.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackObservation {
    /// Subject whose tracker produced the box.
    pub id: SubjectId,
    /// Box in normalized source coordinates.
    pub bbox: Rect,
    /// Tracker confidence in `0.0..=1.0`.
    pub confidence: f32,
    /// The tracker held the box back from a match its score alone would
    /// have moved it to, for a reason that lasts as long as its cause (the
    /// template tracker's contrast check, under a followspot its template
    /// never learned, say). Besides confident observations, only these
    /// count as coasting evidence ([`crate::record`]); a low observation
    /// that is not held, such as an occluded subject's, is ignored. Absent
    /// in older data, where it reads as false.
    #[serde(default)]
    pub held: bool,
}

impl From<(SubjectId, Rect, f32)> for TrackObservation {
    fn from((id, bbox, confidence): (SubjectId, Rect, f32)) -> Self {
        Self {
            id,
            bbox,
            confidence,
            held: false,
        }
    }
}

/// Returns `bbox` clipped to the frame, or `None` when it is not finite, has
/// no area, or lies entirely outside the frame.
pub(crate) fn sanitize(bbox: &Rect) -> Option<Rect> {
    let finite = [bbox.x, bbox.y, bbox.w, bbox.h]
        .iter()
        .all(|v| v.is_finite());
    if !finite || bbox.w <= 0.0 || bbox.h <= 0.0 {
        return None;
    }
    if Rect::FULL.contains_rect(bbox) {
        // Returned bit-for-bit: re-deriving it from edges would round.
        return Some(*bbox);
    }
    bbox.intersection(&Rect::FULL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxes_inside_the_frame_pass_unchanged() {
        let r = Rect::new(0.1, 0.2, 0.3, 0.4);
        assert_eq!(sanitize(&r), Some(r));
    }

    #[test]
    fn boxes_at_the_edge_are_clipped() {
        let r = sanitize(&Rect::new(-0.1, 0.9, 0.3, 0.3)).unwrap();
        assert!((r.x - 0.0).abs() < 1e-12 && (r.w - 0.2).abs() < 1e-12);
        assert!((r.bottom() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn degenerate_boxes_are_rejected() {
        let bad = [
            Rect::new(f64::NAN, 0.1, 0.1, 0.1),
            Rect::new(0.1, 0.1, f64::INFINITY, 0.1),
            Rect::new(0.1, 0.1, 0.0, 0.1),
            Rect::new(0.1, 0.1, 0.1, -0.2),
            Rect::new(1.5, 0.1, 0.1, 0.1),
        ];
        for r in bad {
            assert_eq!(sanitize(&r), None, "{r:?}");
        }
    }

    #[test]
    fn tuples_convert_to_track_observations() {
        let ob: TrackObservation = (SubjectId(2), Rect::FULL, 0.7).into();
        assert_eq!(ob.id, SubjectId(2));
        assert_eq!(ob.confidence, 0.7);
        assert!(!ob.held);
    }
}
