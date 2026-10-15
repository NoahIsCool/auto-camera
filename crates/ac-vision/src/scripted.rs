//! A detector driven by a script instead of a model, for tests and demos.
//!
//! [`ScriptedDetector`] answers from a closure or from a keyframe table keyed
//! by frame sequence number. Either way its output passes through
//! [`sanitize_detections`], so it honours the same contract as a real model:
//! only requested queries, boxes clipped to the frame, scores in `0..=1`.
//! That lets the subject store, director and evaluation be exercised
//! deterministically, without model files.
//!
//! Each detection keeps the [`Extent`](ac_core::Extent) the script gives
//! it. A script that stands in for people, as the engine's synthetic
//! ground truth does, marks its boxes `Extent::Person`, so they are framed
//! exactly like the people-first OWL detector's whole-person boxes; any
//! other box is `Extent::Object`.

use std::collections::BTreeMap;
use std::fmt;

use ac_media::VideoFrame;

use crate::api::{Detection, Detector, PromptQuery, sanitize_detections};
use crate::error::VisionError;

/// Signature of a detection script.
type Script = dyn FnMut(&VideoFrame, &[PromptQuery]) -> Result<Vec<Detection>, VisionError> + Send;

/// A [`Detector`] that returns scripted detections.
pub struct ScriptedDetector {
    /// Name reported by [`Detector::name`].
    name: String,
    /// The script.
    script: Box<Script>,
}

impl fmt::Debug for ScriptedDetector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedDetector")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl ScriptedDetector {
    /// A detector that calls `script` for every frame.
    pub fn from_fn<F>(name: &str, script: F) -> Self
    where
        F: FnMut(&VideoFrame, &[PromptQuery]) -> Result<Vec<Detection>, VisionError>
            + Send
            + 'static,
    {
        Self {
            name: name.into(),
            script: Box::new(script),
        }
    }

    /// A detector that returns the same detections for every frame.
    pub fn constant(name: &str, detections: Vec<Detection>) -> Self {
        Self::from_fn(name, move |_, _| Ok(detections.clone()))
    }

    /// A detector driven by keyframes: for a frame with sequence number `s`,
    /// it returns the entry with the greatest key `<= s`, or nothing before the
    /// first key. Add an empty entry to make subjects disappear.
    pub fn from_table(name: &str, table: impl IntoIterator<Item = (u64, Vec<Detection>)>) -> Self {
        let table: BTreeMap<u64, Vec<Detection>> = table.into_iter().collect();
        Self::from_fn(name, move |frame, _| {
            Ok(table
                .range(..=frame.seq)
                .next_back()
                .map(|(_, d)| d.clone())
                .unwrap_or_default())
        })
    }
}

impl Detector for ScriptedDetector {
    fn name(&self) -> &str {
        &self.name
    }

    fn detect(
        &mut self,
        frame: &VideoFrame,
        queries: &[PromptQuery],
    ) -> Result<Vec<Detection>, VisionError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let raw = (self.script)(frame, queries)?;
        Ok(sanitize_detections(raw, queries))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ac_core::{Extent, QueryId, Rect, SourceId};

    use super::*;

    fn frame(seq: u64) -> VideoFrame {
        VideoFrame::new(
            SourceId("cam".into()),
            seq,
            Duration::ZERO,
            2,
            2,
            Arc::from(vec![0u8; 16]),
        )
        .unwrap()
    }

    fn det(query: &str, x: f64) -> Detection {
        Detection {
            query: QueryId(query.into()),
            bbox: Rect::new(x, 0.2, 0.2, 0.3),
            score: 0.9,
            extent: Extent::Object,
        }
    }

    fn queries() -> Vec<PromptQuery> {
        vec![
            PromptQuery::new("p", "person"),
            PromptQuery::new("g", "guitar"),
        ]
    }

    #[test]
    fn constant_detector_repeats_its_detections() {
        let mut d = ScriptedDetector::constant("fixed", vec![det("p", 0.1)]);
        assert_eq!(d.name(), "fixed");
        for seq in 0..3 {
            assert_eq!(
                d.detect(&frame(seq), &queries()).unwrap(),
                vec![det("p", 0.1)]
            );
        }
    }

    #[test]
    fn table_uses_latest_keyframe_at_or_before_the_frame() {
        let mut d = ScriptedDetector::from_table(
            "table",
            [
                (10, vec![det("p", 0.1)]),
                (20, vec![det("g", 0.5)]),
                (30, vec![]),
            ],
        );
        let q = queries();
        assert!(d.detect(&frame(0), &q).unwrap().is_empty());
        assert_eq!(d.detect(&frame(10), &q).unwrap(), vec![det("p", 0.1)]);
        assert_eq!(d.detect(&frame(19), &q).unwrap(), vec![det("p", 0.1)]);
        assert_eq!(d.detect(&frame(25), &q).unwrap(), vec![det("g", 0.5)]);
        assert!(d.detect(&frame(31), &q).unwrap().is_empty());
    }

    #[test]
    fn closure_sees_frame_and_queries_and_output_is_sanitized() {
        let mut d = ScriptedDetector::from_fn("fn", |frame, queries| {
            let mut out: Vec<Detection> = queries
                .iter()
                .map(|q| det(&q.id.0, 0.1 * frame.seq as f64))
                .collect();
            out.push(det("unrequested", 0.1));
            out.push(det("p", 2.0));
            Ok(out)
        });
        let out = d.detect(&frame(3), &queries()).unwrap();
        assert_eq!(out, vec![det("p", 0.1 * 3.0), det("g", 0.1 * 3.0)]);
    }

    #[test]
    fn scripted_people_stay_people() {
        let person = Detection {
            extent: Extent::Person,
            ..det("p", 0.1)
        };
        let mut d = ScriptedDetector::constant("people", vec![person.clone(), det("g", 0.5)]);
        let out = d.detect(&frame(0), &queries()).unwrap();
        assert_eq!(out, vec![person, det("g", 0.5)]);
        assert_eq!(out[0].extent, Extent::Person);
        assert_eq!(out[1].extent, Extent::Object);
    }

    #[test]
    fn errors_propagate_and_empty_queries_skip_the_script() {
        let mut d =
            ScriptedDetector::from_fn("failing", |_, _| Err(VisionError::Model("boom".into())));
        assert!(matches!(
            d.detect(&frame(0), &queries()),
            Err(VisionError::Model(_))
        ));
        assert!(d.detect(&frame(0), &[]).unwrap().is_empty());
        assert!(format!("{d:?}").contains("failing"));
    }

    #[test]
    fn detector_is_usable_as_a_trait_object() {
        let mut boxed: Box<dyn Detector> =
            Box::new(ScriptedDetector::constant("x", vec![det("g", 0.3)]));
        assert_eq!(boxed.detect(&frame(0), &queries()).unwrap().len(), 1);
    }
}
