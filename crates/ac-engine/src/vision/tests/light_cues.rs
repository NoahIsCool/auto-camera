//! Light cues through the store and the tracker together.
//!
//! The tracker's contrast check holds back a weak match whose window is far
//! more (or less) contrasty than the template when it also lands far from
//! where the subject was heading, so that a faint shadow's box does not jump
//! onto a performer. A light cue changes a real performer's contrast just as
//! much in one frame, and a followspot that lights them and not the
//! backdrop around them also weakens their match. These tests drive
//! [`SourceVision`] the way the core does, with detections from the
//! subject's true box applied a few frames late on the frame they were
//! computed on, and check that a cue costs the subject neither its tracking
//! state nor its identity: an agreeing detection only re-anchors the box and
//! never restarts the tracker, so a tracker that refused every match after a
//! cue would leave the subject running on detections alone (or losing it
//! between passes).
//!
//! Frames are 320 x 180 grey, luma `light x albedo`, and nothing clips.
//! Under a wash the subject's texture fills its box and the cue scales every
//! grey level by the same ratio. Under a spot the subject is a head over a
//! torso in the middle of its box, with backdrop at the sides and above the
//! shoulders, and the cue lights the figure alone.

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
/// Frame at which the light steps, unless a test says otherwise.
const CUE: u64 = 45;
/// The wash's level before the cue, and on the backdrop under a spot.
const BEFORE: f64 = 0.3;
/// The light on the subject from the cue on (x3.3).
const AFTER: f64 = 0.99;

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

/// How a light cue falls on the scene.
#[derive(Debug, Clone, Copy)]
enum Cue {
    /// A wash on everything. The performer's texture fills their box.
    Wash,
    /// A followspot on the performer's figure alone: a head over a torso in
    /// the middle of the box, with backdrop at its sides and above the
    /// shoulders that stays at the wash's level.
    Spot,
}

/// Frame `seq` with a textured performer at `x`, in front of a mottled
/// backdrop, lit by `cue` coming on at frame `at`.
fn frame(seq: u64, x: f64, (cue, at): (Cue, u64)) -> VideoFrame {
    let (x0, y0, w, h) = performer(x);
    let light = if seq < at { BEFORE } else { AFTER };
    let mut data = vec![0u8; (W * H * 4) as usize];
    data.par_chunks_mut((W * 4) as usize)
        .enumerate()
        .for_each(|(y, row)| {
            for px in 0..W as usize {
                let (fx, fy) = (px as f64 + 0.5, y as f64 + 0.5);
                let (u, v) = ((fx - x0) / w, (fy - y0) / h);
                let in_box = (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v);
                let head = ((u - 0.5) / 0.2).powi(2) + ((v - 0.1) / 0.1).powi(2) <= 1.0;
                let torso = (0.1..0.9).contains(&u) && (0.2..1.0).contains(&v);
                let (on_performer, on_backdrop) = match cue {
                    Cue::Wash => (in_box, light),
                    Cue::Spot => (head || torso, BEFORE),
                };
                let luma = if on_performer {
                    light * (0.1 + 0.6 * value_noise(u * 4.0, v * 11.0, 7))
                } else {
                    on_backdrop * (0.35 + 0.15 * (value_noise(fx / 12.0, fy / 12.0, 21) - 0.5))
                };
                let g = (luma.clamp(0.0, 1.0) * 255.0).round() as u8;
                row[px * 4..px * 4 + 4].copy_from_slice(&[g, g, g, 255]);
            }
        });
    VideoFrame::new(source(), seq, FRAME_DT * seq as u32, W, H, data.into()).unwrap()
}

/// Plays `frames` frames of a performer at `x_at(seq)` whose light steps
/// from [`BEFORE`] to [`AFTER`] at frame `light.1`, falling as `light.0`
/// describes, with a detection pass every `pass` frames applied [`LATENCY`]
/// frames late. Calls `check` after every frame from the cue on, from the
/// first frame the tracker updates after the first pass, with the frame
/// number and the vision state.
fn play(
    frames: u64,
    pass: u64,
    light: (Cue, u64),
    x_at: impl Fn(u64) -> f64,
    mut check: impl FnMut(u64, &SourceVision),
) -> SourceVision {
    let mut v = vision();
    let q = [QueryId("person".into())];
    let mut pending: Vec<(u64, VideoFrame)> = Vec::new();
    for seq in 0..frames {
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
        if seq >= light.1 && seq > LATENCY {
            check(seq, &v);
        }
    }
    v
}

/// Asserts that subject 1 is tracking at frame `seq` with a tracker
/// confidence above `floor`.
fn assert_tracking(seq: u64, v: &SourceVision, floor: f32) {
    let id = SubjectId(1);
    let state = v.store().get(id).map(|s| s.state);
    assert_eq!(state, Some(ac_core::TrackState::Tracking), "frame {seq}");
    assert!(
        v.confidence(id) > floor,
        "frame {seq}: confidence {}",
        v.confidence(id)
    );
}

/// Asserts that subject 1's box overlaps the performer at `x` at frame
/// `seq` by an IoU above 0.6, and that it is tracking.
fn assert_on(seq: u64, v: &SourceVision, x: f64) {
    let bbox = v.store().get(SubjectId(1)).map(|s| s.bbox);
    let iou = bbox.map_or(0.0, |b| b.iou(&performer_rect(x)));
    assert!(iou > 0.6, "frame {seq}: IoU {iou:.2} ({bbox:?})");
    let state = v.store().get(SubjectId(1)).map(|s| s.state);
    assert_eq!(state, Some(ac_core::TrackState::Tracking), "frame {seq}");
}

#[test]
fn a_light_cue_keeps_a_still_subject_tracking_at_its_confidence() {
    // Two passes a second, each agreeing with the box, so none restarts the
    // tracker: only the tracker's own matches keep it going after the cue.
    let v = play(
        150,
        15,
        (Cue::Wash, CUE),
        |_| 140.0,
        |seq, v| assert_tracking(seq, v, 0.8),
    );
    assert_eq!(v.stats().created, 1);
}

#[test]
fn a_walker_under_a_light_cue_keeps_its_identity() {
    // One pass a second while the performer walks 1 px per frame: a box held
    // still after the cue falls behind until the subject is lost, and the
    // next pass then admits the performer as a new subject.
    let walk = |seq: u64| 40.0 + seq as f64;
    let v = play(200, 30, (Cue::Wash, CUE), walk, |seq, v| {
        assert_on(seq, v, walk(seq))
    });
    assert_eq!(v.stats().created, 1, "{:?}", v.stats());
    assert_eq!(v.store().len(), 1);
}

#[test]
fn a_followspot_keeps_a_still_subject_tracking() {
    // The spot weakens the subject's match to about 0.7 at a gain of about
    // 6, and the template never learns the new look (the match stays below
    // `update_score`). The match lands where the subject stands, so it is
    // still followed at its own score, between agreeing passes that never
    // restart the tracker.
    let v = play(
        200,
        15,
        (Cue::Spot, CUE),
        |_| 140.0,
        |seq, v| assert_tracking(seq, v, 0.5),
    );
    assert_eq!(v.stats().created, 1);
}

#[test]
fn a_walker_under_a_followspot_keeps_its_identity() {
    // One pass a second while the performer walks 1 px per frame under the
    // spot: the tracker must keep following at its own score, or the box
    // falls behind until the subject is lost and the next pass admits a
    // second one.
    let walk = |seq: u64| 40.0 + seq as f64;
    let v = play(200, 30, (Cue::Spot, CUE), walk, |seq, v| {
        assert_on(seq, v, walk(seq));
        assert_tracking(seq, v, 0.5)
    });
    assert_eq!(v.stats().created, 1, "{:?}", v.stats());
    assert_eq!(v.store().len(), 1);
}

#[test]
fn a_followspot_during_a_late_start_keeps_the_walker() {
    // The first pass, on frame 0, is applied on frame 6 and starts the
    // tracker on frame 0; its first update is frame 7. The performer walks
    // 2 px per frame, so that update catches up 14 px, a third of the box's
    // width, with no velocity yet to predict it, and the spot that came on
    // at frame 3 makes the match weak and its gain high. The surprising-move
    // distance widens with the seven-frame interval, so the catch-up is
    // judged on its score; at the nominal distance it was held back on that
    // frame and every frame after, and the performer came back as a second
    // subject.
    let walk = |seq: u64| 40.0 + 2.0 * seq as f64;
    let v = play(90, 30, (Cue::Spot, 3), walk, |seq, v| {
        assert_on(seq, v, walk(seq));
        assert_tracking(seq, v, 0.5)
    });
    assert_eq!(v.stats().created, 1, "{:?}", v.stats());
    assert_eq!(v.store().len(), 1);
}
