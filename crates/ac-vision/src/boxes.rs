//! Box utilities: clipping, centre-size decoding and non-maximum suppression.
//!
//! Greedy NMS visits boxes from the highest score down and keeps a box unless
//! it overlaps an already kept box by more than the IoU threshold. Ties in
//! score keep input order, so results are deterministic. Class-aware NMS runs
//! the same procedure independently per query: a guitar never suppresses the
//! person holding it, and the subject store can alias overlapping boxes of
//! different queries itself.

use std::collections::BTreeMap;

use ac_core::{QueryId, Rect};

use crate::api::Detection;

/// Clips `bbox` to the frame.
///
/// Returns `None` when a coordinate is non-finite or nothing of the box lies
/// inside `Rect::FULL`.
pub fn clamp_box(bbox: Rect) -> Option<Rect> {
    let finite = [bbox.x, bbox.y, bbox.w, bbox.h]
        .iter()
        .all(|v| v.is_finite());
    if !finite || bbox.w <= 0.0 || bbox.h <= 0.0 {
        return None;
    }
    if Rect::FULL.contains_rect(&bbox) {
        // Returned bit-for-bit: recomputing edges would add rounding noise.
        return Some(bbox);
    }
    bbox.intersection(&Rect::FULL)
}

/// Converts a centre-size box `[cx, cy, w, h]` into a [`Rect`].
pub fn cxcywh_to_rect(b: [f32; 4]) -> Rect {
    let [cx, cy, w, h] = b.map(f64::from);
    Rect::new(cx - w / 2.0, cy - h / 2.0, w, h)
}

/// Class-agnostic greedy NMS.
///
/// Returns indices into `boxes` of the kept boxes, highest score first, at
/// most `max_keep` of them. Boxes with a non-finite score are dropped. A box is
/// suppressed when its IoU with a kept box is greater than `iou_threshold`.
///
/// # Panics
///
/// Panics if `boxes` and `scores` differ in length.
pub fn nms_indices(
    boxes: &[Rect],
    scores: &[f32],
    iou_threshold: f64,
    max_keep: usize,
) -> Vec<usize> {
    assert_eq!(boxes.len(), scores.len(), "one score per box");
    let mut order: Vec<usize> = (0..boxes.len())
        .filter(|&i| scores[i].is_finite())
        .collect();
    // Stable sort: equal scores keep input order.
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    let mut kept: Vec<usize> = Vec::new();
    for i in order {
        if kept.len() >= max_keep {
            break;
        }
        if kept
            .iter()
            .all(|&k| boxes[k].iou(&boxes[i]) <= iou_threshold)
        {
            kept.push(i);
        }
    }
    kept
}

/// Class-aware greedy NMS over detections.
///
/// Suppression only happens between detections of the same query; each query
/// keeps at most `max_per_query` detections. The result is sorted by
/// descending score, ties in input order.
pub fn nms(detections: Vec<Detection>, iou_threshold: f64, max_per_query: usize) -> Vec<Detection> {
    let mut groups: BTreeMap<&QueryId, Vec<usize>> = BTreeMap::new();
    for (i, d) in detections.iter().enumerate() {
        groups.entry(&d.query).or_default().push(i);
    }
    let mut keep: Vec<usize> = Vec::new();
    for members in groups.values() {
        let boxes: Vec<Rect> = members.iter().map(|&i| detections[i].bbox).collect();
        let scores: Vec<f32> = members.iter().map(|&i| detections[i].score).collect();
        keep.extend(
            nms_indices(&boxes, &scores, iou_threshold, max_per_query)
                .into_iter()
                .map(|k| members[k]),
        );
    }
    keep.sort_by(|&a, &b| {
        detections[b]
            .score
            .total_cmp(&detections[a].score)
            .then(a.cmp(&b))
    });
    let mut slots: Vec<Option<Detection>> = detections.into_iter().map(Some).collect();
    keep.into_iter().filter_map(|i| slots[i].take()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(query: &str, x: f64, score: f32) -> Detection {
        Detection {
            query: QueryId(query.into()),
            bbox: Rect::new(x, 0.1, 0.2, 0.2),
            score,
        }
    }

    #[test]
    fn clamp_box_clips_partially_outside_box() {
        let r = clamp_box(Rect::new(0.9, -0.2, 0.3, 0.4)).unwrap();
        assert!((r.x - 0.9).abs() < 1e-12 && r.y == 0.0);
        assert!((r.w - 0.1).abs() < 1e-12 && (r.h - 0.2).abs() < 1e-12);
    }

    #[test]
    fn clamp_box_rejects_outside_empty_and_nan() {
        assert!(clamp_box(Rect::new(1.2, 0.2, 0.1, 0.1)).is_none());
        assert!(clamp_box(Rect::new(0.2, 0.2, 0.0, 0.1)).is_none());
        assert!(clamp_box(Rect::new(0.2, 0.2, -0.1, 0.1)).is_none());
        assert!(clamp_box(Rect::new(f64::NAN, 0.2, 0.1, 0.1)).is_none());
        assert!(clamp_box(Rect::new(0.2, 0.2, f64::INFINITY, 0.1)).is_none());
    }

    #[test]
    fn clamp_box_keeps_inside_box_unchanged() {
        let r = Rect::new(0.1, 0.2, 0.3, 0.4);
        assert_eq!(clamp_box(r), Some(r));
    }

    #[test]
    fn cxcywh_decodes_to_corner_form() {
        let r = cxcywh_to_rect([0.5, 0.25, 0.2, 0.1]);
        assert!((r.x - 0.4).abs() < 1e-7 && (r.y - 0.2).abs() < 1e-7);
        assert!((r.w - 0.2).abs() < 1e-7 && (r.h - 0.1).abs() < 1e-7);
        assert!((r.center().x - 0.5).abs() < 1e-7);
    }

    #[test]
    fn nms_indices_suppresses_overlaps_and_orders_by_score() {
        let boxes = [
            Rect::new(0.0, 0.0, 0.2, 0.2),
            Rect::new(0.01, 0.0, 0.2, 0.2),
            Rect::new(0.6, 0.6, 0.2, 0.2),
        ];
        let kept = nms_indices(&boxes, &[0.5, 0.9, 0.7], 0.5, usize::MAX);
        assert_eq!(kept, vec![1, 2]);
    }

    #[test]
    fn nms_indices_threshold_is_strict_and_respects_max_keep() {
        // Two boxes with IoU exactly 1/3: kept at threshold 1/3, suppressed below.
        let a = Rect::new(0.0, 0.0, 0.2, 0.2);
        let b = Rect::new(0.1, 0.0, 0.2, 0.2);
        let iou = a.iou(&b);
        assert_eq!(nms_indices(&[a, b], &[0.9, 0.8], iou, 10), vec![0, 1]);
        assert_eq!(nms_indices(&[a, b], &[0.9, 0.8], iou - 1e-9, 10), vec![0]);
        let far = [
            a,
            Rect::new(0.5, 0.5, 0.1, 0.1),
            Rect::new(0.8, 0.8, 0.1, 0.1),
        ];
        assert_eq!(nms_indices(&far, &[0.1, 0.3, 0.2], 0.5, 2), vec![1, 2]);
    }

    #[test]
    fn nms_indices_handles_empty_nan_and_ties() {
        assert!(nms_indices(&[], &[], 0.5, 10).is_empty());
        let boxes = [Rect::new(0.0, 0.0, 0.1, 0.1), Rect::new(0.5, 0.5, 0.1, 0.1)];
        assert_eq!(nms_indices(&boxes, &[f32::NAN, 0.2], 0.5, 10), vec![1]);
        assert_eq!(nms_indices(&boxes, &[0.2, 0.2], 0.5, 10), vec![0, 1]);
    }

    #[test]
    #[should_panic(expected = "one score per box")]
    fn nms_indices_panics_on_length_mismatch() {
        nms_indices(&[Rect::FULL], &[], 0.5, 10);
    }

    #[test]
    fn nms_is_class_aware() {
        let dets = vec![
            det("person", 0.1, 0.8),
            det("guitar", 0.1, 0.6),
            det("person", 0.11, 0.7),
        ];
        let out = nms(dets, 0.5, 10);
        let summary: Vec<(&str, f32)> = out.iter().map(|d| (d.query.0.as_str(), d.score)).collect();
        assert_eq!(summary, vec![("person", 0.8), ("guitar", 0.6)]);
    }

    #[test]
    fn nms_limits_each_query_separately() {
        let dets = vec![
            det("a", 0.0, 0.9),
            det("a", 0.3, 0.8),
            det("a", 0.6, 0.7),
            det("b", 0.0, 0.5),
            det("b", 0.3, 0.4),
        ];
        let out = nms(dets, 0.5, 2);
        let scores: Vec<f32> = out.iter().map(|d| d.score).collect();
        assert_eq!(scores, vec![0.9, 0.8, 0.5, 0.4]);
    }

    #[test]
    fn nms_of_nothing_is_nothing() {
        assert!(nms(Vec::new(), 0.5, 10).is_empty());
    }
}
