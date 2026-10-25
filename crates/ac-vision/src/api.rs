//! Model-runtime-independent detector and tracker contracts.
//!
//! Vision is split in two tiers (see the rebuild specification, M4):
//!
//! * A slow, open-vocabulary [`Detector`] finds every instance that matches a
//!   text prompt. It runs at about 0.5 to 2 Hz per source.
//! * A fast [`Tracker`] follows already-identified subjects on every frame,
//!   at video rate, between detector updates. It never creates identities; the
//!   subject store starts and stops tracks from detector results and operator
//!   commands.
//!
//! Contracts shared by every implementation:
//!
//! * All boxes are in normalized source coordinates (see [`ac_core::Rect`]),
//!   finite, non-empty and inside `Rect::FULL`.
//! * Scores and confidences are in `0.0..=1.0`.
//! * A detector answers only the queries it was asked about.
//! * Every detection says what its box covers ([`Extent`]): a whole person
//!   (the people-first OWL matcher, a script standing in for people) or the
//!   thing the prompt named (any other box). Framing composes the two
//!   differently, so a detector must not leave a whole person marked as an
//!   object.
//! * Results describe the frame that was passed in; the caller keeps the
//!   frame's source, sequence number and PTS alongside them.

use ac_core::{Extent, QueryId, Rect, SubjectId};
use ac_media::VideoFrame;
use serde::{Deserialize, Serialize};

use crate::boxes::clamp_box;
use crate::error::VisionError;

/// A text prompt to detect, tagged with the query it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PromptQuery {
    /// Query the prompt belongs to; detections carry it back.
    pub id: QueryId,
    /// Open-vocabulary prompt, such as "a person playing guitar". The
    /// engine sends a query's parsed detector phrase here, never the
    /// operator's prompt: "speaker upper body" arrives as "a person
    /// speaking" (`ac_core::Query::detector_phrase`).
    pub prompt: String,
}

impl PromptQuery {
    /// Creates a query from an id string and a prompt.
    pub fn new(id: &str, prompt: &str) -> Self {
        Self {
            id: QueryId(id.into()),
            prompt: prompt.into(),
        }
    }
}

/// One instance found by a detector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    /// Query whose prompt matched.
    pub query: QueryId,
    /// Box in normalized source coordinates.
    pub bbox: Rect,
    /// Match score in `0.0..=1.0`.
    pub score: f32,
    /// What `bbox` covers. Absent in older data, where it reads as
    /// [`Extent::Object`].
    #[serde(default)]
    pub extent: Extent,
}

/// An open-vocabulary detector.
///
/// Implementations may be slow (hundreds of milliseconds); callers run them
/// off the frame path and drop results that arrive for stale frames.
pub trait Detector: Send {
    /// Short human-readable name, such as `"owlv2-onnx"`.
    fn name(&self) -> &str;

    /// Finds every instance of every query in `frame`.
    ///
    /// Returns an empty list when `queries` is empty. Results satisfy the
    /// module-level contract: only requested queries, boxes inside the frame,
    /// scores in `0.0..=1.0`.
    fn detect(
        &mut self,
        frame: &VideoFrame,
        queries: &[PromptQuery],
    ) -> Result<Vec<Detection>, VisionError>;
}

/// The tracker's estimate of one subject on one frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackObservation {
    /// Subject being tracked.
    pub id: SubjectId,
    /// Visible extent of the subject, in normalized source coordinates.
    pub bbox: Rect,
    /// Confidence in `0.0..=1.0` that `bbox` is on the subject.
    pub confidence: f32,
    /// The tracker held the box back from a match whose score alone would
    /// have moved it, for a reason that outlasts the frame: the
    /// [`TemplateTracker`](crate::TemplateTracker)'s contrast check refused
    /// a surprising match far off the template's contrast (under a
    /// followspot the template never learned, say), whether or not the
    /// match was also ambiguous. A coast for that reason lasts as long as
    /// its cause, so the subject store restarts a tracker that reported
    /// such observations and no confident one between two detections,
    /// unless its report on the second detection's frame had the match held
    /// back ([`Self::held_at`]) overlapping the detection, but no more than
    /// the subject's box. False for a trusted
    /// match, for one held back by its own low score (an occluder over the
    /// subject, which the old template finds again once it leaves) and for
    /// one held back by its ambiguity alone (a narrow occluder in front of
    /// the subject makes those too). Absent in older data, where it reads
    /// as false.
    #[serde(default)]
    pub held: bool,
    /// Where the match held back lay, when [`Self::held`] is set: the
    /// visible part of a box of the tracked size centred on it, in
    /// normalized source coordinates; `None` otherwise. The box itself
    /// ([`Self::bbox`]) stays where the tracker coasts. A performer who
    /// sidestepped under a followspot is where the match was, so a
    /// detection computed on the same frame lands on this box; a brighter
    /// occluder that looks partly like the subject draws the match onto
    /// itself, and the subject's detection lands where its box already was
    /// instead. The subject store restarts the tracker from such a
    /// detection only in the first case: the restart is seeded on that
    /// frame. Absent in older data, where it reads as `None`.
    #[serde(default)]
    pub held_at: Option<Rect>,
}

/// A per-frame, per-source subject tracker.
///
/// A tracker owns per-subject appearance and motion state. It reports a low
/// confidence instead of guessing when the subject is occluded or ambiguous,
/// and never retires a subject by itself: the caller decides from the
/// confidence stream and calls [`Tracker::stop`].
pub trait Tracker: Send {
    /// Starts (or restarts) tracking `id` at `bbox` on `frame`.
    fn start(&mut self, frame: &VideoFrame, id: SubjectId, bbox: Rect) -> Result<(), VisionError>;

    /// Advances every tracked subject to `frame` and returns one observation
    /// per subject, ordered by id.
    fn update(&mut self, frame: &VideoFrame) -> Vec<TrackObservation>;

    /// Stops tracking `id`. Unknown ids are ignored.
    fn stop(&mut self, id: SubjectId);

    /// Ids currently tracked, in ascending order.
    fn tracked(&self) -> Vec<SubjectId>;
}

/// Enforces the detector output contract on raw detections.
///
/// Drops detections for queries not in `queries`, with non-finite scores, or
/// whose box is empty after clipping to the frame; clips boxes to the frame
/// and clamps scores into `0.0..=1.0`. Order and extents are preserved.
pub fn sanitize_detections(detections: Vec<Detection>, queries: &[PromptQuery]) -> Vec<Detection> {
    detections
        .into_iter()
        .filter(|d| d.score.is_finite() && queries.iter().any(|q| q.id == d.query))
        .filter_map(|d| {
            clamp_box(d.bbox).map(|bbox| Detection {
                query: d.query,
                bbox,
                score: d.score.clamp(0.0, 1.0),
                extent: d.extent,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(query: &str, bbox: Rect, score: f32) -> Detection {
        Detection {
            query: QueryId(query.into()),
            bbox,
            score,
            extent: Extent::Object,
        }
    }

    #[test]
    fn sanitize_keeps_valid_detections_in_order() {
        let queries = [
            PromptQuery::new("a", "person"),
            PromptQuery::new("b", "guitar"),
        ];
        let input = vec![
            det("b", Rect::new(0.1, 0.1, 0.2, 0.2), 0.9),
            det("a", Rect::new(0.5, 0.5, 0.2, 0.2), 0.4),
        ];
        assert_eq!(sanitize_detections(input.clone(), &queries), input);
    }

    #[test]
    fn sanitize_drops_unrequested_queries_and_bad_values() {
        let queries = [PromptQuery::new("a", "person")];
        let input = vec![
            det("z", Rect::new(0.1, 0.1, 0.2, 0.2), 0.9),
            det("a", Rect::new(0.1, 0.1, 0.2, 0.2), f32::NAN),
            det("a", Rect::new(1.5, 0.1, 0.2, 0.2), 0.9),
            det("a", Rect::new(f64::NAN, 0.1, 0.2, 0.2), 0.9),
        ];
        assert!(sanitize_detections(input, &queries).is_empty());
    }

    #[test]
    fn sanitize_clips_boxes_and_clamps_scores() {
        let queries = [PromptQuery::new("a", "person")];
        let out = sanitize_detections(
            vec![det("a", Rect::new(-0.1, 0.9, 0.3, 0.3), 1.7)],
            &queries,
        );
        assert_eq!(out.len(), 1);
        let b = out[0].bbox;
        assert!(Rect::FULL.contains_rect(&b));
        assert!((b.w - 0.2).abs() < 1e-12 && (b.h - 0.1).abs() < 1e-12);
        assert_eq!(out[0].score, 1.0);
    }

    #[test]
    fn sanitize_keeps_the_extent() {
        let queries = [PromptQuery::new("a", "person")];
        let person = Detection {
            extent: Extent::Person,
            ..det("a", Rect::new(-0.1, 0.2, 0.3, 0.5), 0.8)
        };
        let out = sanitize_detections(vec![person], &queries);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].extent, Extent::Person);
    }

    #[test]
    fn sanitize_with_no_queries_returns_nothing() {
        let out = sanitize_detections(vec![det("a", Rect::new(0.1, 0.1, 0.2, 0.2), 0.5)], &[]);
        assert!(out.is_empty());
    }
}
