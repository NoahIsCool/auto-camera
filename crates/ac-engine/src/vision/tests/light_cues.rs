//! Light cues through the store and the tracker together.
//!
//! The tracker's contrast check holds back weak matches whose window is far
//! more (or less) contrasty than the template, so that a faint shadow's box
//! does not jump onto a performer. A light cue changes a real performer's
//! contrast just as much in one frame. These tests drive [`SourceVision`] the
//! way the core does, with detections from the subject's true box applied a
//! few frames late on the frame they were computed on, and check that a cue
//! costs the subject neither its tracking state nor its identity: an
//! agreeing detection only re-anchors the box and never restarts the
//! tracker, so a tracker that refused every match after a cue would leave
//! the subject running on detections alone (or losing it between passes).
//!
//! Frames are 320 x 180 grey, luma `light x albedo` with the subject's
//! texture attached to its box, so the cue scales every grey level by the
//! same ratio and nothing clips.

use rayon::prelude::*;

use super::*;

/// Frame width in pixels.
const W: u32 = 320;
/// Frame height in pixels.
const H: u32 = 180;
/// Frame interval at 30 fps.
const FRAME_DT: Duration = Duration::from_micros(33_333);
/// Frames from a detection pass's frame to the frame its result is applied.
const LATENCY: u64 = 6;
/// Frame at which the light steps.
const CUE: u64 = 45;

/// Hash of a lattice point to `0..1`.
fn lattice(ix: i64, iy: i64, seed: u64) -> f64 {
    let mut h = (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    (h % 10_000) as f64 / 10_000.0
}

/// Smooth value noise in `0..1` with lattice spacing 1.
fn value_noise(u: f64, v: f64, seed: u64) -> f64 {
    let (x0, y0) = (u.floor(), v.floor());
    let (tx, ty) = (u - x0, v - y0);
    let (ix, iy) = (x0 as i64, y0 as i64);
    let a = lattice(ix, iy, seed) + (lattice(ix + 1, iy, seed) - lattice(ix, iy, seed)) * tx;
    let b = lattice(ix, iy + 1, seed)
        + (lattice(ix + 1, iy + 1, seed) - lattice(ix, iy + 1, seed)) * tx;
    a + (b - a) * ty
}

/// The subject's box in pixels: a performer 44 x 120 px whose left edge is
/// at `x`, standing on the same spot of the floor.
fn performer(x: f64) -> (f64, f64, f64, f64) {
    (x, 40.0, 44.0, 120.0)
}

/// The normalized box of a performer at `x`.
fn performer_rect(x: f64) -> Rect {
    let (x0, y0, w, h) = performer(x);
    Rect::new(x0 / W as f64, y0 / H as f64, w / W as f64, h / H as f64)
}

/// Frame `seq` with a textured performer at `x` under `light`, in front of
/// a mottled backdrop.
fn frame(seq: u64, x: f64, light: f64) -> VideoFrame {
    let (x0, y0, w, h) = performer(x);
    let mut data = vec![0u8; (W * H * 4) as usize];
    data.par_chunks_mut((W * 4) as usize)
        .enumerate()
        .for_each(|(y, row)| {
            for px in 0..W as usize {
                let (fx, fy) = (px as f64 + 0.5, y as f64 + 0.5);
                let (u, v) = ((fx - x0) / w, (fy - y0) / h);
                let albedo = if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                    0.1 + 0.6 * value_noise(u * 4.0, v * 11.0, 7)
                } else {
                    0.35 + 0.15 * (value_noise(fx / 12.0, fy / 12.0, 21) - 0.5)
                };
                let g = ((light * albedo).clamp(0.0, 1.0) * 255.0).round() as u8;
                row[px * 4..px * 4 + 4].copy_from_slice(&[g, g, g, 255]);
            }
        });
    VideoFrame::new(source(), seq, FRAME_DT * seq as u32, W, H, data.into()).unwrap()
}

/// Plays `frames` frames of a performer at `x_at(seq)` whose light steps
/// from `0.3` to `0.99` (x3.3) at [`CUE`], with a detection pass every
/// `pass` frames applied [`LATENCY`] frames late. Calls `check` after every
/// frame from the cue on, with the frame number and the vision state.
fn play(
    frames: u64,
    pass: u64,
    x_at: impl Fn(u64) -> f64,
    mut check: impl FnMut(u64, &SourceVision),
) -> SourceVision {
    let mut v = vision();
    let q = [QueryId("person".into())];
    let mut pending: Vec<(u64, VideoFrame)> = Vec::new();
    for seq in 0..frames {
        let light = if seq < CUE { 0.3 } else { 0.99 };
        let f = frame(seq, x_at(seq), light);
        v.on_frame(&f);
        if seq % pass == 0 {
            pending.push((seq, f.clone()));
        }
        while pending.first().is_some_and(|(s, _)| seq >= s + LATENCY) {
            let (s, seen) = pending.remove(0);
            let det = ac_vision::Detection {
                query: q[0].clone(),
                bbox: performer_rect(x_at(s)),
                score: 0.9,
                extent: ac_core::Extent::Person,
            };
            v.on_detections(&seen, Some(&f), &[det], &q);
        }
        if seq >= CUE {
            check(seq, &v);
        }
    }
    v
}

#[test]
fn a_light_cue_keeps_a_still_subject_tracking_at_its_confidence() {
    // Two passes a second, each agreeing with the box, so none restarts the
    // tracker: only the tracker's own matches keep it going after the cue.
    let id = SubjectId(1);
    let v = play(
        150,
        15,
        |_| 140.0,
        |seq, v| {
            let state = v.store().get(id).map(|s| s.state);
            assert_eq!(state, Some(ac_core::TrackState::Tracking), "frame {seq}");
            assert!(
                v.confidence(id) > 0.8,
                "frame {seq}: confidence {}",
                v.confidence(id)
            );
        },
    );
    assert_eq!(v.stats().created, 1);
}

#[test]
fn a_walker_under_a_light_cue_keeps_its_identity() {
    // One pass a second while the performer walks 1 px per frame: a box held
    // still after the cue falls behind until the subject is lost, and the
    // next pass then admits the performer as a new subject.
    let walk = |seq: u64| 40.0 + seq as f64;
    let id = SubjectId(1);
    let v = play(200, 30, walk, |seq, v| {
        let bbox = v.store().get(id).map(|s| s.bbox);
        let iou = bbox.map_or(0.0, |b| b.iou(&performer_rect(walk(seq))));
        assert!(iou > 0.6, "frame {seq}: IoU {iou:.2} ({bbox:?})");
    });
    assert_eq!(v.stats().created, 1, "{:?}", v.stats());
    assert_eq!(v.store().len(), 1);
}
