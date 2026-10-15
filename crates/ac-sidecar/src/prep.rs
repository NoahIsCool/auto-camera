//! Frame preparation for the sidecar: RGBA to RGB8, downscaled with a box
//! filter, pure.
//!
//! # Theory
//!
//! Source frames are packed RGBA8 ([`ac_media::VideoFrame`]); protocol v1
//! sends RGB8, so alpha is dropped. The model resizes every image to its
//! own square input (672 px for SAM 3.1 as measured, `hello.input_side`),
//! so sending a 4K frame would only cost pipe bandwidth and the sidecar's
//! resize time. Frames are therefore shrunk until their longer side is at
//! most `max_side` (default [`DEFAULT_SEND_SIDE`], 1008 px), keeping the
//! aspect ratio, and never enlarged.
//!
//! The filter is an exact box (area-average) filter, applied separably:
//! each output pixel is the mean of the source pixels it covers, with
//! fractional weights for the pixels it covers partly. That is the
//! anti-aliasing a shrink needs (no moire on stage lighting rigs or
//! patterned clothes) and is exact for constant regions. It averages
//! gamma-encoded values, as common resizers do.
//!
//! Because the whole frame maps onto the whole image (no crop, no
//! letterbox), coordinates normalized to the image sent are coordinates
//! normalized to the source frame, so the sidecar's boxes need no mapping
//! back. Rounding the short side to whole pixels distorts the aspect by
//! less than half a pixel, which normalized coordinates absorb.
//!
//! # Invariants
//!
//! * Pure and deterministic: the same frame gives the same bytes.
//! * The output's longer side is `min(longer side, max_side)` and its
//!   shorter side at least 1; both sides are at least 1 for any input.
//! * A frame already small enough is copied pixel for pixel, minus alpha.
//! * A constant-colour frame stays exactly that colour.
//!
//! Cost: one pass per axis, a few multiply-adds per output channel. The
//! crate is built with `opt-level = 3` in dev builds (workspace manifest)
//! so debug runs keep up.

use ac_media::VideoFrame;

/// Default longest side of the image sent, in pixels. 1.5 times SAM 3.1's
/// measured 672 px input, so the sidecar's own resize still has detail to
/// work with.
pub const DEFAULT_SEND_SIDE: u32 = 1008;

/// Largest `max_side` a configuration may use: a square image this size
/// stays far below the protocol's payload limit.
pub const MAX_SEND_SIDE: u32 = 4096;

/// A packed RGB8 image, rows top to bottom, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 3` bytes.
    pub data: Vec<u8>,
}

/// The size a `width` by `height` frame is sent at: the longer side
/// limited to `max_side` (at least 1), the aspect ratio kept, rounded to
/// whole pixels, never enlarged.
pub fn send_size(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let max_side = max_side.max(1);
    let (width, height) = (width.max(1), height.max(1));
    let longer = width.max(height);
    if longer <= max_side {
        return (width, height);
    }
    let scale = |side: u32| -> u32 {
        let scaled =
            (u64::from(side) * u64::from(max_side) + u64::from(longer) / 2) / u64::from(longer);
        (scaled as u32).clamp(1, max_side)
    };
    if width >= height {
        (max_side, scale(height))
    } else {
        (scale(width), max_side)
    }
}

/// `frame` as RGB8 at [`send_size`]. `None` when the frame's pixel data
/// does not match its size (frames made with [`VideoFrame::new`] always
/// do).
pub fn prepare(frame: &VideoFrame, max_side: u32) -> Option<RgbImage> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if w == 0 || h == 0 || frame.data.len() != w * h * 4 {
        return None;
    }
    let (dw, dh) = send_size(frame.width, frame.height, max_side);
    let data = if (dw, dh) == (frame.width, frame.height) {
        frame
            .data
            .chunks_exact(4)
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect()
    } else {
        shrink(&frame.data, w, h, dw as usize, dh as usize)
    };
    Some(RgbImage {
        width: dw,
        height: dh,
        data,
    })
}

/// The source pixels one output pixel covers along one axis, with their
/// weights (summing to 1).
#[derive(Debug, Clone, PartialEq)]
struct Tap {
    /// First source pixel covered.
    first: usize,
    /// Weight of each covered pixel, from `first` on.
    weights: Vec<f32>,
}

/// The box-filter taps that shrink `src` pixels to `dst` (`dst <= src`,
/// both at least 1).
fn taps(src: usize, dst: usize) -> Vec<Tap> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|i| {
            let lo = i as f64 * scale;
            let hi = ((i + 1) as f64 * scale).min(src as f64);
            let first = (lo.floor() as usize).min(src - 1);
            let last = (hi.ceil() as usize).clamp(first + 1, src);
            let mut weights: Vec<f32> = (first..last)
                .map(|j| {
                    let covered = ((j + 1) as f64).min(hi) - (j as f64).max(lo);
                    covered.max(0.0) as f32
                })
                .collect();
            let sum: f32 = weights.iter().sum();
            if sum > 0.0 {
                weights.iter_mut().for_each(|w| *w /= sum);
            } else {
                weights = vec![1.0; 1];
            }
            Tap { first, weights }
        })
        .collect()
}

/// Shrinks packed RGBA `src` (`w` by `h`) to RGB `dw` by `dh` with the box
/// filter: rows first into a float buffer, then columns.
fn shrink(src: &[u8], w: usize, h: usize, dw: usize, dh: usize) -> Vec<u8> {
    let across = taps(w, dw);
    let down = taps(h, dh);
    let mut rows = vec![0f32; h * dw * 3];
    for y in 0..h {
        let line = &src[y * w * 4..(y + 1) * w * 4];
        let out = &mut rows[y * dw * 3..(y + 1) * dw * 3];
        for (x, tap) in across.iter().enumerate() {
            let mut acc = [0f32; 3];
            for (k, weight) in tap.weights.iter().enumerate() {
                let px = &line[(tap.first + k) * 4..(tap.first + k) * 4 + 3];
                for c in 0..3 {
                    acc[c] += weight * f32::from(px[c]);
                }
            }
            out[x * 3..x * 3 + 3].copy_from_slice(&acc);
        }
    }
    let mut out = vec![0u8; dw * dh * 3];
    let mut acc = vec![0f32; dw * 3];
    for (y, tap) in down.iter().enumerate() {
        acc.fill(0.0);
        for (k, weight) in tap.weights.iter().enumerate() {
            let row = &rows[(tap.first + k) * dw * 3..(tap.first + k + 1) * dw * 3];
            for (a, v) in acc.iter_mut().zip(row) {
                *a += weight * v;
            }
        }
        let line = &mut out[y * dw * 3..(y + 1) * dw * 3];
        for (o, a) in line.iter_mut().zip(&acc) {
            *o = a.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ac_core::SourceId;

    use super::*;

    /// A `w` by `h` frame whose pixel `(x, y)` is `f(x, y)`.
    fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> VideoFrame {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&f(x, y));
            }
        }
        VideoFrame::new(
            SourceId("cam".into()),
            0,
            Duration::ZERO,
            w,
            h,
            Arc::from(data),
        )
        .unwrap()
    }

    #[test]
    fn sizes_keep_the_aspect_and_never_enlarge() {
        assert_eq!(send_size(1920, 1080, 1008), (1008, 567));
        assert_eq!(send_size(1080, 1920, 1008), (567, 1008));
        assert_eq!(send_size(3840, 2160, 1008), (1008, 567));
        assert_eq!(send_size(1280, 720, 1008), (1008, 567));
        assert_eq!(send_size(640, 360, 1008), (640, 360));
        assert_eq!(send_size(1008, 1008, 1008), (1008, 1008));
        assert_eq!(send_size(1009, 1009, 1008), (1008, 1008));
    }

    #[test]
    fn sizes_stay_positive_at_the_extremes() {
        assert_eq!(send_size(5000, 1, 1008), (1008, 1));
        assert_eq!(send_size(1, 5000, 1008), (1, 1008));
        assert_eq!(send_size(1, 1, 1008), (1, 1));
        assert_eq!(send_size(0, 0, 1008), (1, 1));
        assert_eq!(send_size(1920, 1080, 0), (1, 1));
        assert_eq!(send_size(1920, 1080, 1), (1, 1));
        assert_eq!(send_size(u32::MAX, u32::MAX / 2, 1008), (1008, 504));
    }

    #[test]
    fn small_frames_lose_only_their_alpha() {
        let f = frame(3, 2, |x, y| [x as u8, y as u8, 7, 255]);
        let img = prepare(&f, 1008).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(
            img.data,
            [0, 0, 7, 1, 0, 7, 2, 0, 7, 0, 1, 7, 1, 1, 7, 2, 1, 7]
        );
    }

    #[test]
    fn a_one_pixel_frame_is_one_pixel() {
        let img = prepare(&frame(1, 1, |_, _| [10, 20, 30, 0]), 1008).unwrap();
        assert_eq!((img.width, img.height, img.data), (1, 1, vec![10, 20, 30]));
        let img = prepare(&frame(1, 1, |_, _| [10, 20, 30, 0]), 1).unwrap();
        assert_eq!(img.data, [10, 20, 30]);
    }

    #[test]
    fn halving_averages_each_two_by_two_block() {
        let f = frame(4, 2, |x, _| {
            let v = [0, 100, 200, 250][x as usize];
            [v, 255 - v, 9, 0]
        });
        let img = prepare(&f, 2).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.data, [50, 205, 9, 225, 30, 9]);
    }

    #[test]
    fn odd_ratios_weigh_partly_covered_pixels() {
        // Three pixels to two: each output covers 1.5 source pixels.
        let f = frame(3, 1, |x, _| [[0, 90, 180][x as usize], 0, 0, 0]);
        let img = prepare(&f, 2).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        // (0 + 0.5 * 90) / 1.5 = 30; (0.5 * 90 + 180) / 1.5 = 150.
        assert_eq!(img.data, [30, 0, 0, 150, 0, 0]);
    }

    #[test]
    fn constant_frames_stay_exact_at_any_size() {
        for (w, h, side) in [(1920, 1080, 1008), (1009, 7, 1008), (17, 31, 5), (5, 1, 2)] {
            let f = frame(w, h, |_, _| [255, 1, 128, 3]);
            let img = prepare(&f, side).unwrap();
            assert_eq!((img.width, img.height), send_size(w, h, side));
            assert_eq!(img.data.len(), (img.width * img.height * 3) as usize);
            assert!(
                img.data.chunks_exact(3).all(|p| p == [255, 1, 128]),
                "{w}x{h} at {side}"
            );
        }
    }

    #[test]
    fn a_thin_strip_shrinks_along_its_length_only() {
        let f = frame(10, 1, |x, _| [(x * 20) as u8, 0, 0, 0]);
        let img = prepare(&f, 5).unwrap();
        assert_eq!((img.width, img.height), (5, 1));
        let reds: Vec<u8> = img.data.chunks_exact(3).map(|p| p[0]).collect();
        assert_eq!(reds, [10, 50, 90, 130, 170]);
    }

    #[test]
    fn the_top_row_stays_on_top() {
        // White top half, black bottom half, shrunk 4x.
        let f = frame(8, 8, |_, y| if y < 4 { [255; 4] } else { [0; 4] });
        let img = prepare(&f, 2).unwrap();
        assert_eq!(img.data, [255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn mismatched_frames_are_refused() {
        let mut f = frame(2, 2, |_, _| [0; 4]);
        f.data = Arc::from(vec![0u8; 15]);
        assert!(prepare(&f, 1008).is_none());
        f.width = 0;
        assert!(prepare(&f, 1008).is_none());
    }

    #[test]
    fn taps_cover_the_source_exactly_once() {
        for (src, dst) in [
            (1920, 1008),
            (1080, 567),
            (7, 3),
            (5, 5),
            (1009, 1008),
            (1000, 1),
        ] {
            let mut cover = vec![0f64; src];
            for t in taps(src, dst) {
                let sum: f32 = t.weights.iter().sum();
                assert!((sum - 1.0).abs() < 1e-5, "{src}->{dst}");
                for (k, w) in t.weights.iter().enumerate() {
                    cover[t.first + k] += f64::from(*w) * src as f64 / dst as f64;
                }
            }
            assert!(cover.iter().all(|c| (c - 1.0).abs() < 1e-3), "{src}->{dst}");
        }
    }

    #[test]
    fn preparation_is_deterministic() {
        let f = frame(1280, 720, |x, y| {
            [(x ^ y) as u8, (x * 3) as u8, (y * 5) as u8, 0]
        });
        assert_eq!(prepare(&f, 1008), prepare(&f, 1008));
    }
}
