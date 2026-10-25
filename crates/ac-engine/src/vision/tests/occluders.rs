//! Occluders through the store and the tracker together.
//!
//! While something covers part of a subject, the subject's match sinks with
//! its visible part, below `accept_score`: the tracker coasts where the
//! subject was heading and reports the match's own low score. Its template
//! learns only from strong matches, so it keeps the subject's clean look,
//! and when the occluder leaves the tracker finds the subject again within
//! a few frames. A restart during the occlusion would take the template
//! from a frame with the occluder in the box, and when the occluder leaves,
//! the box would follow it at near-full confidence. So the store restarts a
//! coasting tracker only when the tracker held its matches back for a
//! lasting reason (`TrackObservation::held`), never when their own low
//! score held them back, which is what an occluder usually does. A much
//! brighter occluder that looks partly like the subject does hold them
//! back for a lasting reason: it draws the match onto itself, where the
//! contrast check holds it back as it would under a followspot. Then the
//! tracker's report on the frame a pass was computed on, which is the frame
//! a restart would be seeded on, has the match held back on the occluder
//! (`TrackObservation::held_at`), while the pass boxes the subject where
//! its box is, and the store does not restart from such a frame either.
//!
//! The tests drive [`SourceVision`] as the light-cue tests do, with a pass
//! every 15 frames (two a second) applied 6 or 15 frames late on the frame
//! it was computed on. At one pass a second the subject is lost during a
//! long occlusion and the pass that recovers it seeds the tracker on the
//! occluded frame anyway, which these tests do not cover.
//!
//! Frames are 320 x 180 grey. The subject is a 44 x 120 px textured box
//! standing still at x = 140 under light 0.3 in front of a mottled
//! backdrop. The occluder, a textured box much brighter than the subject
//! and never reported by the detector, walks in from the left at 4 px a
//! frame, stands over part of the subject for 45 frames and walks back out
//! the way it came. Its texture is its own, or partly the subject's.

use rayon::prelude::*;

use super::light_cues::{FRAME_DT, H, W, value_noise};
use super::*;

/// Left edge of the subject, in pixels.
const SUBJECT_X: f64 = 140.0;
/// Light on the subject and on the backdrop.
const LIGHT: f64 = 0.3;
/// Frames the occluder stands still over the subject.
const DWELL: u64 = 45;
/// The occluder's walking speed, in pixels per frame.
const SPEED: f64 = 4.0;
/// Frames between detection passes.
const PASS: u64 = 15;

/// The normalized box of a 120 px tall box whose left edge is at `x` and
/// that is `w` px wide, standing where the subject stands.
fn rect(x: f64, w: f64) -> Rect {
    Rect::new(
        x / W as f64,
        40.0 / H as f64,
        w / W as f64,
        120.0 / H as f64,
    )
}

/// The subject's true box.
fn subject() -> Rect {
    rect(SUBJECT_X, 44.0)
}

/// An occluder `w` px wide under `light` that walks in to stand with its
/// left edge at `stop`.
#[derive(Debug, Clone, Copy)]
struct Occluder {
    /// Width in pixels.
    w: f64,
    /// Light on it.
    light: f64,
    /// Left edge while it stands over the subject, in pixels.
    stop: f64,
    /// Weight of the subject's texture in its own, `0..=1`: 0 for a
    /// texture unrelated to the subject's.
    blend: f64,
}

impl Occluder {
    /// First frame it stands at `stop`; it enters from x = -50 at frame 30.
    fn arrive(&self) -> u64 {
        ((self.stop + 50.0) / SPEED).ceil() as u64 + 30
    }

    /// First frame it walks back out.
    fn leave(&self) -> u64 {
        self.arrive() + DWELL
    }

    /// Its left edge at frame `seq`, in pixels.
    fn x(&self, seq: u64) -> f64 {
        if seq < 30 {
            -50.0
        } else if seq < self.arrive() {
            (-50.0 + SPEED * (seq - 30) as f64).min(self.stop)
        } else if seq < self.leave() {
            self.stop
        } else {
            self.stop - SPEED * (seq - self.leave()) as f64
        }
    }

    /// Frame `seq`: the occluder in front of the subject in front of the
    /// backdrop.
    fn frame(&self, seq: u64) -> VideoFrame {
        let ox = self.x(seq);
        let mut data = vec![0u8; (W * H * 4) as usize];
        data.par_chunks_mut((W * 4) as usize)
            .enumerate()
            .for_each(|(y, row)| {
                for px in 0..W as usize {
                    let (fx, fy) = (px as f64 + 0.5, y as f64 + 0.5);
                    let inside = |x0: f64, w: f64| {
                        let (u, v) = ((fx - x0) / w, (fy - 40.0) / 120.0);
                        ((0.0..1.0).contains(&u) && (0.0..1.0).contains(&v)).then_some((u, v))
                    };
                    let luma = if let Some((u, v)) = inside(ox, self.w) {
                        let (u, v) = (u * 4.0 * self.w / 44.0, v * 11.0);
                        let n = self.blend * value_noise(u, v, 7)
                            + (1.0 - self.blend) * value_noise(u, v, 33);
                        self.light * (0.1 + 0.6 * n)
                    } else if let Some((u, v)) = inside(SUBJECT_X, 44.0) {
                        LIGHT * (0.1 + 0.6 * value_noise(u * 4.0, v * 11.0, 7))
                    } else {
                        LIGHT * (0.35 + 0.15 * (value_noise(fx / 12.0, fy / 12.0, 21) - 0.5))
                    };
                    let g = (luma.clamp(0.0, 1.0) * 255.0).round() as u8;
                    row[px * 4..px * 4 + 4].copy_from_slice(&[g, g, g, 255]);
                }
            });
        VideoFrame::new(source(), seq, FRAME_DT * seq as u32, W, H, data.into()).unwrap()
    }

    /// The subject's box as a segmenting detector gives it on frame `seq`:
    /// the span of its visible columns, or nothing when fewer than 18 of its
    /// 44 columns show.
    fn visible_part(&self, seq: u64) -> Option<Rect> {
        let ox = self.x(seq);
        let cols: Vec<f64> = (0..44)
            .map(|i| SUBJECT_X + i as f64 + 0.5)
            .filter(|c| !(ox..ox + self.w).contains(c))
            .collect();
        let (first, last) = (cols.first()?, cols.last()?);
        (cols.len() >= 18).then(|| rect(first - 0.5, last - first + 1.0))
    }
}

/// Plays the occluder's walk until 60 frames after it starts back out, with
/// a pass every [`PASS`] frames applied `latency` frames late that boxes the
/// subject as `detect` says. Calls `check` after every frame from frame 40
/// on with the frame number and the vision state.
fn play(
    occluder: Occluder,
    latency: u64,
    detect: impl Fn(u64) -> Option<Rect>,
    mut check: impl FnMut(u64, &SourceVision),
) {
    let mut v = vision();
    let q = [QueryId("person".into())];
    let mut pending: Vec<(u64, VideoFrame)> = Vec::new();
    for seq in 0..occluder.leave() + 60 {
        let f = occluder.frame(seq);
        v.on_frame(&f);
        if seq % PASS == 0 {
            pending.push((seq, f.clone()));
        }
        while pending.first().is_some_and(|(s, _)| seq >= s + latency) {
            let (s, seen) = pending.remove(0);
            let dets: Vec<_> = detect(s)
                .into_iter()
                .map(|bbox| ac_vision::Detection {
                    query: q[0].clone(),
                    bbox,
                    score: 0.9,
                    extent: ac_core::Extent::Person,
                })
                .collect();
            v.on_detections(&seen, Some(&f), &dets, &q);
        }
        if seq >= 40 {
            check(seq, &v);
        }
    }
    assert_eq!(v.stats().created, 1, "{:?}", v.stats());
}

/// Subject 1's box and its IoU with the subject at frame `seq`, checking
/// that it is tracking.
fn subject_box(seq: u64, v: &SourceVision) -> (Rect, f64) {
    let s = v.store().get(SubjectId(1)).expect("subject 1 is live");
    assert_eq!(s.state, ac_core::TrackState::Tracking, "frame {seq}");
    (s.bbox, s.bbox.iou(&subject()))
}

#[test]
fn a_bright_occluder_leaves_the_subject_where_it_was() {
    // The occluder, three times as bright, stands over the left half of the
    // subject, which the detector boxes whole on every pass. The subject's
    // tracker coasts at its own score (about 0.44) from frame 69, and the
    // pass from frame 90 closes a window of nothing but that coasting. When
    // it restarted the tracker, the template took the occluder in half the
    // box, and once the occluder walked off the box followed it at 0.99
    // (frame 120: x = 128, IoU 0.57 with the subject), off the subject for
    // 19 frames at a latency of 6 and 16 to 28 at 15.
    let occluder = Occluder {
        w: 44.0,
        light: 0.9,
        stop: 118.0,
        blend: 0.0,
    };
    for latency in [6, 15] {
        play(
            occluder,
            latency,
            |_| Some(subject()),
            |seq, v| {
                let (bbox, iou) = subject_box(seq, v);
                assert!(
                    iou > 0.6,
                    "latency {latency}, frame {seq}: box at x = {:.1}, IoU {iou:.2} with the \
                     subject, {:.2} with the occluder, confidence {:.2}",
                    bbox.x * W as f64,
                    bbox.iou(&rect(occluder.x(seq), occluder.w)),
                    v.confidence(SubjectId(1))
                );
            },
        );
    }
}

#[test]
fn a_narrow_occluder_in_the_middle_leaves_the_subject_where_it_was() {
    // A narrow occluder, twice as bright, stands in the middle of the
    // subject, and the detector boxes the span of the subject's visible
    // columns, as a segmenting detector does: the part right of the
    // occluder while it covers the subject's left edge, the whole box while
    // it stands in the middle. Its matches during the occlusion are weak,
    // some of them ambiguous, and none is held back by the contrast check.
    // Restarted in the occlusion (on every low observation, or on the
    // ambiguous ones too), the box followed the occluder off the subject for
    // 11 to 21 frames after it started back out, down to IoU 0.00; without
    // those restarts it stays at IoU 0.64 or more. At three times the light
    // the tracker loses the subject to a 16 px occluder with no restart for
    // coasting at all, so that case is not here.
    for (w, stop) in [(16.0, 154.0), (20.0, 152.0), (24.0, 150.0)] {
        for latency in [6, 15] {
            let occluder = Occluder {
                w,
                light: 2.0 * LIGHT,
                stop,
                blend: 0.0,
            };
            play(
                occluder,
                latency,
                |seq| occluder.visible_part(seq),
                |seq, v| {
                    if seq < occluder.leave() {
                        return;
                    }
                    let (bbox, iou) = subject_box(seq, v);
                    assert!(
                        iou > 0.5,
                        "{w} px, latency {latency}, frame {seq}: box at x = {:.1} w = {:.1}, \
                         IoU {iou:.2}, confidence {:.2}",
                        bbox.x * W as f64,
                        bbox.w * W as f64,
                        v.confidence(SubjectId(1))
                    );
                },
            );
        }
    }
}

#[test]
fn a_bright_occluder_that_looks_partly_like_the_subject_leaves_it_where_it_was() {
    // The bright occluder over the left half of the subject, as in the
    // first test, with 40 % of the subject's texture in its own. The
    // subject's match does not merely sink with its visible part any more:
    // the occluder draws it onto itself, where the contrast check holds it
    // back, so the coast is flagged as held. When any pass after a window
    // of that restarted the tracker, the pass from frame 90 seeded it on
    // its occluded frame, and once the occluder walked off the box followed
    // it at 0.99 (frame 120: x = 128.2, IoU 0.57 with the subject, 0.33
    // with the occluder), off the subject for 19 frames, down to IoU 0.00,
    // at a latency of 6, and 28 at 15. On the frame of each pass that
    // closes a window of such coasting, the match held back lies on the
    // occluder while the pass boxes the subject where its box is, so none
    // restarts the tracker, and the old template finds the subject again
    // when the occluder leaves (IoU 0.82 or more after it).
    let occluder = Occluder {
        w: 44.0,
        light: 0.9,
        stop: 118.0,
        blend: 0.4,
    };
    for latency in [6, 15] {
        play(
            occluder,
            latency,
            |_| Some(subject()),
            |seq, v| {
                let (bbox, iou) = subject_box(seq, v);
                assert!(
                    iou > 0.6,
                    "latency {latency}, frame {seq}: box at x = {:.1}, IoU {iou:.2} with the \
                     subject, {:.2} with the occluder, confidence {:.2}",
                    bbox.x * W as f64,
                    bbox.iou(&rect(occluder.x(seq), occluder.w)),
                    v.confidence(SubjectId(1))
                );
            },
        );
    }
}
