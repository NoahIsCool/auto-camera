//! OWL-ViT / OWLv2 pre- and post-processing, independent of the model runtime.
//!
//! OWLv2 ("Scaling Open-Vocabulary Object Detection", Minderer et al.) scores
//! every image patch against every text query. Its exported graph takes a
//! square image and padded token ids and returns, per patch, one logit per
//! query and one box.
//!
//! # Preprocessing
//!
//! OWLv2 was trained on images padded to a square at the bottom and right with
//! grey (`0.5` before normalization), then resized to the input side (960 for
//! the base model, 1008 for the large one). [`preprocess`] does the same in the
//! cheaper order: it resizes the frame so its longer side fills the input
//! (tent filter, anti-aliased when shrinking), places it at the top-left and
//! fills the rest with grey. Values are normalized with the CLIP mean and
//! standard deviation and laid out channel-planar (`[3, side, side]`). The
//! aspect ratio is preserved; nothing is stretched.
//!
//! # Postprocessing
//!
//! [`decode`] applies a sigmoid to each logit, keeps (patch, query) pairs that
//! reach that query's threshold, converts the patch's `(cx, cy, w, h)` box from
//! input-square coordinates to source-normalized coordinates (undoing the
//! padding via [`Letterbox`]), clips it to the frame, and finishes with
//! class-aware NMS. One patch may therefore yield detections for several
//! queries ("person" and "person's upper body"), which the subject store can
//! alias.
//!
//! # Text
//!
//! The CLIP tokenizer produces `[BOS, tokens…, EOS]`. OWL models expect 16
//! positions per query: [`pack_tokens`] truncates (keeping EOS last) and pads
//! with the pad id, building the attention mask alongside. The text tower pools
//! at the arg-max token id, which is EOS, so the pad id must be smaller than
//! EOS (OWL's own tokenizer pads with `!`, id 0).

use ac_core::Rect;
use ac_media::VideoFrame;

use crate::api::{Detection, PromptQuery};
use crate::boxes::{clamp_box, cxcywh_to_rect, nms};
use crate::error::VisionError;
use crate::image::{Kernel, resize_rows};

/// CLIP image mean per RGB channel, used by OWL-ViT and OWLv2.
pub const CLIP_MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
/// CLIP image standard deviation per RGB channel.
pub const CLIP_STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];
/// Grey used for padding, before normalization.
pub const PAD_VALUE: f32 = 0.5;
/// Token positions per query in OWL text towers.
pub const OWL_MAX_TOKENS: usize = 16;

/// Placement of a source frame in the square model input.
///
/// The frame occupies the top-left `content_w x content_h` pixels of a
/// `side x side` input; the rest is padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Letterbox {
    /// Side of the square input, in pixels.
    pub side: u32,
    /// Width of the resized frame inside the input.
    pub content_w: u32,
    /// Height of the resized frame inside the input.
    pub content_h: u32,
}

impl Letterbox {
    /// Fits a `src_w x src_h` frame into a `side x side` input.
    pub fn new(src_w: u32, src_h: u32, side: u32) -> Result<Self, VisionError> {
        if src_w == 0 || src_h == 0 || side == 0 {
            return Err(VisionError::InvalidConfig(format!(
                "cannot letterbox {src_w}x{src_h} into {side}x{side}"
            )));
        }
        let scale = side as f64 / src_w.max(src_h) as f64;
        let fit = |v: u32| ((v as f64 * scale).round() as u32).clamp(1, side);
        Ok(Self {
            side,
            content_w: fit(src_w),
            content_h: fit(src_h),
        })
    }

    /// Maps a box normalized to the input square into source-normalized
    /// coordinates, clipped to the frame. `None` when it lies in the padding.
    pub fn to_source(&self, r: Rect) -> Option<Rect> {
        let sx = self.side as f64 / self.content_w as f64;
        let sy = self.side as f64 / self.content_h as f64;
        clamp_box(Rect::new(r.x * sx, r.y * sy, r.w * sx, r.h * sy))
    }
}

/// Builds the normalized `[3, side, side]` input for `frame` into `out`.
pub fn preprocess_into(
    frame: &VideoFrame,
    side: u32,
    out: &mut Vec<f32>,
) -> Result<Letterbox, VisionError> {
    let lb = Letterbox::new(frame.width, frame.height, side)?;
    let (sw, sh) = (frame.width as usize, frame.height as usize);
    let (cw, ch, s) = (lb.content_w as usize, lb.content_h as usize, side as usize);
    out.clear();
    out.resize(3 * s * s, 0.0);
    for c in 0..3 {
        let (mean, std) = (CLIP_MEAN[c], CLIP_STD[c]);
        let plane = resize_rows(sw, sh, cw, ch, Kernel::Triangle, |y, row| {
            let src = &frame.data[y * sw * 4..(y + 1) * sw * 4];
            for (v, p) in row.iter_mut().zip(src.chunks_exact(4)) {
                *v = p[c] as f32 / 255.0;
            }
        });
        let dst = &mut out[c * s * s..(c + 1) * s * s];
        dst.fill((PAD_VALUE - mean) / std);
        for (y, row) in plane.chunks_exact(cw).enumerate() {
            for (d, &v) in dst[y * s..y * s + cw].iter_mut().zip(row) {
                *d = (v - mean) / std;
            }
        }
    }
    Ok(lb)
}

/// Allocating form of [`preprocess_into`].
pub fn preprocess(frame: &VideoFrame, side: u32) -> Result<(Vec<f32>, Letterbox), VisionError> {
    let mut out = Vec::new();
    let lb = preprocess_into(frame, side, &mut out)?;
    Ok((out, lb))
}

/// Logistic function.
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Raw model outputs for one image.
#[derive(Debug, Clone, Copy)]
pub struct OwlOutputs<'a> {
    /// Logits, `[num_boxes, num_queries]` row-major.
    pub logits: &'a [f32],
    /// Boxes, `[num_boxes, 4]` as `(cx, cy, w, h)` normalized to the input square.
    pub boxes: &'a [f32],
    /// Number of predicted boxes (image patches).
    pub num_boxes: usize,
    /// Number of text queries.
    pub num_queries: usize,
}

/// Decoding parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeParams {
    /// Score threshold for each query, in query order.
    pub thresholds: Vec<f32>,
    /// IoU above which a lower-scored box of the same query is suppressed.
    pub nms_iou: f64,
    /// Largest number of detections kept per query.
    pub max_per_query: usize,
}

/// Turns raw outputs into detections in source coordinates.
///
/// `queries[q]` names column `q` of the logits. Fails when the slice lengths
/// do not match the stated dimensions or the number of queries/thresholds.
pub fn decode(
    outputs: &OwlOutputs<'_>,
    queries: &[PromptQuery],
    letterbox: &Letterbox,
    params: &DecodeParams,
) -> Result<Vec<Detection>, VisionError> {
    let (nb, nq) = (outputs.num_boxes, outputs.num_queries);
    if outputs.logits.len() != nb * nq || outputs.boxes.len() != nb * 4 {
        return Err(VisionError::Shape(format!(
            "expected {nb}x{nq} logits and {nb}x4 boxes, got {} and {}",
            outputs.logits.len(),
            outputs.boxes.len()
        )));
    }
    if queries.len() != nq || params.thresholds.len() != nq {
        return Err(VisionError::Shape(format!(
            "{nq} logit columns but {} queries and {} thresholds",
            queries.len(),
            params.thresholds.len()
        )));
    }
    let mut detections = Vec::new();
    for (logits, bx) in outputs
        .logits
        .chunks_exact(nq.max(1))
        .zip(outputs.boxes.chunks_exact(4))
    {
        // Decoded lazily: most patches match no query.
        let mut rect = None;
        for (q, &logit) in logits.iter().enumerate() {
            let score = sigmoid(logit);
            if score.is_nan() || score < params.thresholds[q] {
                continue;
            }
            let r = *rect.get_or_insert_with(|| {
                letterbox.to_source(cxcywh_to_rect([bx[0], bx[1], bx[2], bx[3]]))
            });
            if let Some(bbox) = r {
                detections.push(Detection {
                    query: queries[q].id.clone(),
                    bbox,
                    score,
                });
            }
        }
    }
    Ok(nms(detections, params.nms_iou, params.max_per_query))
}

/// Packs token-id sequences into `[n, max_len]` id and attention-mask rows.
///
/// Longer sequences keep their first `max_len - 1` ids and their last id (the
/// end token); shorter ones are padded with `pad_id` and mask `0`.
pub fn pack_tokens(seqs: &[Vec<u32>], max_len: usize, pad_id: u32) -> (Vec<i64>, Vec<i64>) {
    let mut ids = Vec::with_capacity(seqs.len() * max_len);
    let mut mask = Vec::with_capacity(seqs.len() * max_len);
    for seq in seqs {
        let kept: Vec<u32> = if seq.len() > max_len && max_len > 0 {
            seq[..max_len - 1]
                .iter()
                .chain(seq.last())
                .copied()
                .collect()
        } else {
            seq.iter().copied().take(max_len).collect()
        };
        ids.extend(kept.iter().map(|&t| t as i64));
        mask.extend(std::iter::repeat_n(1i64, kept.len()));
        ids.extend(std::iter::repeat_n(pad_id as i64, max_len - kept.len()));
        mask.extend(std::iter::repeat_n(0i64, max_len - kept.len()));
    }
    (ids, mask)
}

/// Canonical form of a prompt, used as a cache key: trimmed, lower-cased,
/// inner whitespace collapsed to single spaces.
pub fn normalize_prompt(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ac_core::{QueryId, SourceId};

    use super::*;

    fn frame(w: u32, h: u32, rgba: [u8; 4]) -> VideoFrame {
        let data: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
        VideoFrame::new(
            SourceId("c".into()),
            0,
            Duration::ZERO,
            w,
            h,
            Arc::from(data),
        )
        .unwrap()
    }

    fn logit(p: f32) -> f32 {
        (p / (1.0 - p)).ln()
    }

    #[test]
    fn letterbox_fits_the_longer_side() {
        let lb = Letterbox::new(1920, 1080, 960).unwrap();
        assert_eq!((lb.content_w, lb.content_h), (960, 540));
        let tall = Letterbox::new(720, 1280, 960).unwrap();
        assert_eq!((tall.content_w, tall.content_h), (540, 960));
        let small = Letterbox::new(320, 240, 960).unwrap();
        assert_eq!((small.content_w, small.content_h), (960, 720));
        assert!(Letterbox::new(0, 10, 960).is_err());
        assert!(Letterbox::new(10, 10, 0).is_err());
    }

    #[test]
    fn letterbox_undoes_padding() {
        let lb = Letterbox::new(1920, 1080, 960).unwrap();
        // Content spans the full width and the top 540/960 of the height.
        let full = lb
            .to_source(Rect::new(0.0, 0.0, 1.0, 540.0 / 960.0))
            .unwrap();
        assert!((full.w - 1.0).abs() < 1e-9 && (full.h - 1.0).abs() < 1e-9);
        let r = lb.to_source(Rect::new(0.25, 0.125, 0.25, 0.25)).unwrap();
        assert!((r.x - 0.25).abs() < 1e-9 && (r.w - 0.25).abs() < 1e-9);
        assert!((r.y - 0.125 * 960.0 / 540.0).abs() < 1e-9);
        assert!((r.h - 0.25 * 960.0 / 540.0).abs() < 1e-9);
        // A box straddling the padding is clipped; one inside it vanishes.
        let straddle = lb.to_source(Rect::new(0.1, 0.5, 0.2, 0.2)).unwrap();
        assert!((straddle.bottom() - 1.0).abs() < 1e-9);
        assert!(lb.to_source(Rect::new(0.1, 0.7, 0.2, 0.2)).is_none());
    }

    #[test]
    fn preprocess_normalizes_content_and_pads_with_grey() {
        let f = frame(40, 20, [255, 128, 0, 255]);
        let (t, lb) = preprocess(&f, 16).unwrap();
        assert_eq!((lb.content_w, lb.content_h), (16, 8));
        assert_eq!(t.len(), 3 * 16 * 16);
        let at = |c: usize, x: usize, y: usize| t[c * 256 + y * 16 + x];
        for c in 0..3 {
            let v = [1.0, 128.0 / 255.0, 0.0][c];
            assert!((at(c, 3, 2) - (v - CLIP_MEAN[c]) / CLIP_STD[c]).abs() < 1e-5);
            assert!((at(c, 15, 7) - (v - CLIP_MEAN[c]) / CLIP_STD[c]).abs() < 1e-5);
            assert!((at(c, 0, 8) - (PAD_VALUE - CLIP_MEAN[c]) / CLIP_STD[c]).abs() < 1e-6);
            assert!((at(c, 15, 15) - (PAD_VALUE - CLIP_MEAN[c]) / CLIP_STD[c]).abs() < 1e-6);
        }
    }

    #[test]
    fn preprocess_reuses_the_buffer() {
        let mut buf = vec![9.0; 7];
        let lb = preprocess_into(&frame(8, 8, [0, 0, 0, 255]), 4, &mut buf).unwrap();
        assert_eq!(
            lb,
            Letterbox {
                side: 4,
                content_w: 4,
                content_h: 4
            }
        );
        assert_eq!(buf.len(), 48);
        assert!((buf[0] + CLIP_MEAN[0] / CLIP_STD[0]).abs() < 1e-6);
    }

    #[test]
    fn sigmoid_is_logistic() {
        assert_eq!(sigmoid(0.0), 0.5);
        assert!((sigmoid(logit(0.9)) - 0.9).abs() < 1e-6);
        assert!(sigmoid(-100.0) >= 0.0 && sigmoid(100.0) <= 1.0);
    }

    fn params(thresholds: Vec<f32>) -> DecodeParams {
        DecodeParams {
            thresholds,
            nms_iou: 0.3,
            max_per_query: 10,
        }
    }

    #[test]
    fn decode_thresholds_per_query_maps_boxes_and_suppresses() {
        let queries = [
            PromptQuery::new("p", "person"),
            PromptQuery::new("g", "guitar"),
        ];
        let lb = Letterbox::new(200, 100, 100).unwrap();
        #[rustfmt::skip]
        let boxes = [
            0.25, 0.25, 0.2, 0.2,  // patch 0
            0.26, 0.25, 0.2, 0.2,  // patch 1: overlaps patch 0
            0.75, 0.25, 0.1, 0.1,  // patch 2
        ];
        #[rustfmt::skip]
        let logits = [
            logit(0.8), logit(0.05),
            logit(0.6), logit(0.4),
            logit(0.2), logit(0.35),
        ];
        let out = OwlOutputs {
            logits: &logits,
            boxes: &boxes,
            num_boxes: 3,
            num_queries: 2,
        };
        let dets = decode(&out, &queries, &lb, &params(vec![0.5, 0.3])).unwrap();
        let summary: Vec<(&str, f32)> =
            dets.iter().map(|d| (d.query.0.as_str(), d.score)).collect();
        // person: patch 0 kept, patch 1 suppressed, patch 2 under threshold.
        // guitar: patches 1 and 2 (not overlapping) above 0.3.
        assert_eq!(summary.len(), 3);
        assert_eq!(summary[0].0, "p");
        assert!((summary[0].1 - 0.8).abs() < 1e-5);
        assert_eq!((summary[1].0, summary[2].0), ("g", "g"));
        let person = dets[0].bbox;
        // Input-square (0.15..0.35, 0.15..0.35) -> source x same, y doubled.
        assert!((person.x - 0.15).abs() < 1e-6 && (person.w - 0.2).abs() < 1e-6);
        assert!((person.y - 0.3).abs() < 1e-6 && (person.h - 0.4).abs() < 1e-6);
        assert_eq!(dets[2].query, QueryId("g".into()));
        assert!((dets[2].bbox.center().x - 0.75).abs() < 1e-6);
    }

    #[test]
    fn decode_drops_padding_boxes_and_nan_logits() {
        let queries = [PromptQuery::new("p", "person")];
        let lb = Letterbox::new(200, 100, 100).unwrap();
        let boxes = [0.5, 0.9, 0.1, 0.1, 0.5, 0.25, 0.1, 0.1];
        let logits = [logit(0.9), f32::NAN];
        let out = OwlOutputs {
            logits: &logits,
            boxes: &boxes,
            num_boxes: 2,
            num_queries: 1,
        };
        assert!(
            decode(&out, &queries, &lb, &params(vec![0.1]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn decode_rejects_mismatched_shapes() {
        let queries = [PromptQuery::new("p", "person")];
        let lb = Letterbox::new(10, 10, 10).unwrap();
        let bad_boxes = OwlOutputs {
            logits: &[0.0; 2],
            boxes: &[0.0; 7],
            num_boxes: 2,
            num_queries: 1,
        };
        assert!(matches!(
            decode(&bad_boxes, &queries, &lb, &params(vec![0.1])),
            Err(VisionError::Shape(_))
        ));
        let good = OwlOutputs {
            logits: &[0.0; 2],
            boxes: &[0.0; 8],
            num_boxes: 2,
            num_queries: 1,
        };
        assert!(matches!(
            decode(&good, &[], &lb, &params(vec![0.1])),
            Err(VisionError::Shape(_))
        ));
        assert!(matches!(
            decode(&good, &queries, &lb, &params(vec![])),
            Err(VisionError::Shape(_))
        ));
    }

    #[test]
    fn decode_with_no_boxes_is_empty() {
        let queries = [PromptQuery::new("p", "person")];
        let lb = Letterbox::new(10, 10, 10).unwrap();
        let out = OwlOutputs {
            logits: &[],
            boxes: &[],
            num_boxes: 0,
            num_queries: 1,
        };
        assert!(
            decode(&out, &queries, &lb, &params(vec![0.1]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn pack_tokens_pads_truncates_and_masks() {
        let seqs = vec![
            vec![49406, 1929, 49407],
            (0..20).collect::<Vec<u32>>(),
            vec![],
        ];
        let (ids, mask) = pack_tokens(&seqs, 6, 0);
        assert_eq!(&ids[0..6], &[49406, 1929, 49407, 0, 0, 0]);
        assert_eq!(&mask[0..6], &[1, 1, 1, 0, 0, 0]);
        assert_eq!(&ids[6..12], &[0, 1, 2, 3, 4, 19]);
        assert_eq!(&mask[6..12], &[1; 6]);
        assert_eq!(&ids[12..18], &[0; 6]);
        assert_eq!(&mask[12..18], &[0; 6]);
    }

    #[test]
    fn normalize_prompt_folds_case_and_space() {
        assert_eq!(
            normalize_prompt("  Person's   Upper\tBody "),
            "person's upper body"
        );
        assert_eq!(normalize_prompt(""), "");
    }
}
