//! Measures [`TemplateTracker`] cost per frame on synthetic 1080p video.
//!
//! Renders textured squares drifting across a noisy background and reports
//! the mean wall time of `Tracker::update` for 1, 8 and 16 subjects. Run with
//! `cargo run --release -p ac-vision --example track_bench`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::{Rect, SourceId, SubjectId};
use ac_media::VideoFrame;
use ac_vision::{TemplateTracker, Tracker};

/// Source width in pixels.
const W: u32 = 1920;
/// Source height in pixels.
const H: u32 = 1080;
/// Side of each square in pixels.
const SIDE: u32 = 160;

/// Cheap integer hash used for textures.
fn hash(x: u32, y: u32, seed: u32) -> u8 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    (h >> 24) as u8
}

/// Top-left corner of square `i` on frame `seq`.
fn corner(i: u32, seq: u32) -> (u32, u32) {
    let x = 40 + (i % 8) * 220 + seq * 2;
    let y = 60 + (i / 8) * 400;
    (x, y)
}

/// Renders frame `seq` with `n` squares.
fn render(seq: u32, n: u32) -> VideoFrame {
    let mut data = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let v = 60 + hash(x / 6, y / 6, 1) / 3;
            let i = ((y * W + x) * 4) as usize;
            data[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    for s in 0..n {
        let (x0, y0) = corner(s, seq);
        for y in y0..y0 + SIDE {
            for x in x0..(x0 + SIDE).min(W) {
                let v = hash((x - x0) / 10, (y - y0) / 10, 7 + s);
                let i = ((y * W + x) * 4) as usize;
                data[i..i + 4].copy_from_slice(&[v, v / 2, 255 - v, 255]);
            }
        }
    }
    let pts = Duration::from_micros(33_333) * seq;
    VideoFrame::new(
        SourceId("bench".into()),
        seq as u64,
        pts,
        W,
        H,
        Arc::from(data),
    )
    .expect("valid frame")
}

fn main() {
    const FRAMES: u32 = 60;
    for n in [1u32, 8, 16] {
        let frames: Vec<VideoFrame> = (0..FRAMES).map(|s| render(s, n)).collect();
        let mut tracker = TemplateTracker::default();
        for s in 0..n {
            let (x, y) = corner(s, 0);
            let bbox = Rect::new(
                x as f64 / W as f64,
                y as f64 / H as f64,
                SIDE as f64 / W as f64,
                SIDE as f64 / H as f64,
            );
            tracker
                .start(&frames[0], SubjectId(s), bbox)
                .expect("start");
        }
        let started = Instant::now();
        let mut min_conf = 1.0f32;
        for f in &frames[1..] {
            for o in tracker.update(f) {
                min_conf = min_conf.min(o.confidence);
            }
        }
        let per_frame = started.elapsed() / (FRAMES - 1);
        println!(
            "{n:>2} subjects: {per_frame:?} per 1080p frame (lowest confidence {min_conf:.2})"
        );
    }
}
