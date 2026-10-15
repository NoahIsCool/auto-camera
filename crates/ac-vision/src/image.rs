//! Grayscale images and separable resampling.
//!
//! Pixel `k` covers the continuous interval `[k, k + 1)`, so its centre is at
//! `k + 0.5`. Resampling from `src` to `dst` samples maps output pixel `i` to
//! the source interval centred on `(i + 0.5) * src / dst`.
//!
//! Two kernels are offered:
//!
//! * [`Kernel::Area`] averages exactly the source area under each output pixel
//!   (fractional coverage at the edges). It is the cheapest anti-aliased
//!   downscale and is what the tracker uses: about `src / dst + 1` taps per
//!   output sample per axis. When enlarging it blends the one or two source
//!   samples under a one-pixel footprint.
//! * [`Kernel::Triangle`] is a tent filter whose support widens with the
//!   downscale factor. It anti-aliases like `Area` when shrinking and becomes
//!   plain bilinear interpolation when enlarging, which the detector needs for
//!   small sources.
//!
//! Both are applied separably: a horizontal pass over every source row, then a
//! vertical pass over every output row. Rows are independent, so both passes
//! run in parallel with rayon. Values are `f32` in `0.0..=1.0`.

use ac_media::VideoFrame;
use rayon::prelude::*;

/// Resampling filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel {
    /// Exact area average of the source under each output pixel.
    Area,
    /// Tent filter: anti-aliased when shrinking, bilinear when enlarging.
    Triangle,
}

/// Source indices and normalized weights contributing to one output sample.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Taps {
    /// First contributing source index.
    pub start: usize,
    /// Weights for `start..start + weights.len()`, summing to one.
    pub weights: Vec<f32>,
}

/// Precomputes the taps that map `src` samples onto `dst` samples.
pub(crate) fn axis_taps(src: usize, dst: usize, kernel: Kernel) -> Vec<Taps> {
    let scale = src as f64 / dst as f64;
    // Footprint half-width in source samples: half an output pixel for the
    // area kernel, a whole one for the tent; never narrower than one sample.
    let support = scale.max(1.0);
    let reach = match kernel {
        Kernel::Area => support / 2.0,
        Kernel::Triangle => support,
    };
    (0..dst)
        .map(|i| {
            let center = (i as f64 + 0.5) * scale;
            let (lo, hi) = (center - reach, center + reach);
            let weight = |k: usize| match kernel {
                Kernel::Area => (hi.min(k as f64 + 1.0) - lo.max(k as f64)).max(0.0),
                Kernel::Triangle => (1.0 - ((k as f64 + 0.5 - center) / support).abs()).max(0.0),
            };
            let start = (lo.floor().max(0.0) as usize).min(src - 1);
            let end = (hi.ceil() as usize).clamp(start + 1, src);
            let mut weights: Vec<f64> = (start..end).map(weight).collect();
            let total: f64 = weights.iter().sum();
            if total > 0.0 {
                weights.iter_mut().for_each(|w| *w /= total);
            } else {
                // Degenerate footprint (cannot happen for valid sizes): nearest.
                weights.iter_mut().for_each(|w| *w = 0.0);
                weights[0] = 1.0;
            }
            Taps {
                start,
                weights: weights.into_iter().map(|w| w as f32).collect(),
            }
        })
        .collect()
}

/// Resamples a `sw x sh` image to `dw x dh`.
///
/// `fill_row(y, row)` writes source row `y` (`sw` values) into `row`; this lets
/// callers convert pixel formats on the fly without materializing the full
/// source image.
pub(crate) fn resize_rows<F>(
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
    kernel: Kernel,
    fill_row: F,
) -> Vec<f32>
where
    F: Fn(usize, &mut [f32]) + Sync,
{
    assert!(sw > 0 && sh > 0 && dw > 0 && dh > 0, "non-empty images");
    let xt = axis_taps(sw, dw, kernel);
    let yt = axis_taps(sh, dh, kernel);
    let mut tmp = vec![0.0f32; sh * dw];
    tmp.par_chunks_mut(dw).enumerate().for_each_init(
        || vec![0.0f32; sw],
        |row, (y, out)| {
            fill_row(y, row);
            for (o, t) in out.iter_mut().zip(&xt) {
                *o = dot(&row[t.start..t.start + t.weights.len()], &t.weights);
            }
        },
    );
    let mut out = vec![0.0f32; dw * dh];
    out.par_chunks_mut(dw)
        .zip(yt.par_iter())
        .for_each(|(o, t)| {
            for (k, &w) in t.weights.iter().enumerate() {
                let src = &tmp[(t.start + k) * dw..(t.start + k + 1) * dw];
                o.iter_mut().zip(src).for_each(|(a, &s)| *a += w * s);
            }
        });
    out
}

/// Dot product written with eight independent accumulators so the compiler
/// can vectorize it (float addition is not reassociated automatically).
#[inline]
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut acc = [0.0f32; 8];
    let mut ca = a.chunks_exact(8);
    let mut cb = b.chunks_exact(8);
    for (x, y) in (&mut ca).zip(&mut cb) {
        for k in 0..8 {
            acc[k] += x[k] * y[k];
        }
    }
    let tail: f32 = ca
        .remainder()
        .iter()
        .zip(cb.remainder())
        .map(|(x, y)| x * y)
        .sum();
    acc.iter().sum::<f32>() + tail
}

/// A single-channel image with values in `0.0..=1.0`, row-major.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GrayImage {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `width * height` samples.
    pub data: Vec<f32>,
}

impl GrayImage {
    /// Wraps `data`, which must hold `width * height` samples.
    pub fn new(width: usize, height: usize, data: Vec<f32>) -> Self {
        assert_eq!(data.len(), width * height, "image data size");
        Self {
            width,
            height,
            data,
        }
    }

    /// Converts `frame` to luma (BT.601 weights) resampled to `width x height`
    /// with the area kernel.
    pub fn from_frame(frame: &VideoFrame, width: usize, height: usize) -> Self {
        let (sw, sh) = (frame.width as usize, frame.height as usize);
        let px = &frame.data;
        let data = resize_rows(sw, sh, width, height, Kernel::Area, |y, row| {
            let src = &px[y * sw * 4..(y + 1) * sw * 4];
            for (v, p) in row.iter_mut().zip(src.chunks_exact(4)) {
                *v = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) / 255.0;
            }
        });
        Self::new(width, height, data)
    }

    /// Row `y`.
    pub fn row(&self, y: usize) -> &[f32] {
        &self.data[y * self.width..(y + 1) * self.width]
    }

    /// Half-resolution image: each pixel averages a 2x2 block. Odd trailing
    /// rows and columns are dropped.
    pub fn downsample2(&self) -> Self {
        let (w, h) = (self.width / 2, self.height / 2);
        let mut data = vec![0.0f32; w * h];
        data.par_chunks_mut(w.max(1))
            .enumerate()
            .for_each(|(y, out)| {
                let (r0, r1) = (self.row(2 * y), self.row(2 * y + 1));
                for (x, o) in out.iter_mut().enumerate() {
                    *o = 0.25 * (r0[2 * x] + r0[2 * x + 1] + r1[2 * x] + r1[2 * x + 1]);
                }
            });
        Self::new(w, h, data)
    }

    /// Bilinear sample at continuous position `(x, y)`, clamping at the borders.
    pub fn bilinear(&self, x: f64, y: f64) -> f32 {
        let fx = (x - 0.5).clamp(0.0, (self.width - 1) as f64);
        let fy = (y - 0.5).clamp(0.0, (self.height - 1) as f64);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = ((fx - x0 as f64) as f32, (fy - y0 as f64) as f32);
        let at = |x: usize, y: usize| self.data[y * self.width + x];
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
        let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
        top + (bottom - top) * ty
    }

    /// Resamples the region of size `w x h` centred at `(cx, cy)` (pixels)
    /// into an `out_w x out_h` patch.
    ///
    /// Each output pixel averages a grid of bilinear samples, about one per
    /// source pixel of its footprint (up to 4x4), so shrunk patches do not
    /// alias. Near-unit scales take a single bilinear sample.
    pub fn sample_patch(
        &self,
        cx: f64,
        cy: f64,
        w: f64,
        h: f64,
        out_w: usize,
        out_h: usize,
    ) -> Vec<f32> {
        let (sx, sy) = (w / out_w as f64, h / out_h as f64);
        let nx = (sx + 0.5).floor().clamp(1.0, 4.0) as usize;
        let ny = (sy + 0.5).floor().clamp(1.0, 4.0) as usize;
        let (x0, y0) = (cx - w / 2.0, cy - h / 2.0);
        let norm = 1.0 / (nx * ny) as f32;
        let mut out = Vec::with_capacity(out_w * out_h);
        for v in 0..out_h {
            for u in 0..out_w {
                let mut acc = 0.0f32;
                for b in 0..ny {
                    let y = y0 + (v as f64 + (b as f64 + 0.5) / ny as f64) * sy;
                    for a in 0..nx {
                        let x = x0 + (u as f64 + (a as f64 + 0.5) / nx as f64) * sx;
                        acc += self.bilinear(x, y);
                    }
                }
                out.push(acc * norm);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ac_core::SourceId;

    use super::*;

    fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> VideoFrame {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = f(x, y);
                data.extend_from_slice(&[v, v, v, 255]);
            }
        }
        VideoFrame::new(
            SourceId("t".into()),
            0,
            Duration::ZERO,
            w,
            h,
            Arc::from(data),
        )
        .unwrap()
    }

    #[test]
    fn taps_are_normalized_and_in_range() {
        for kernel in [Kernel::Area, Kernel::Triangle] {
            for (src, dst) in [
                (1920, 384),
                (384, 384),
                (100, 37),
                (37, 100),
                (1, 5),
                (5, 1),
            ] {
                let taps = axis_taps(src, dst, kernel);
                assert_eq!(taps.len(), dst);
                for t in &taps {
                    let sum: f32 = t.weights.iter().sum();
                    assert!((sum - 1.0).abs() < 1e-5, "{kernel:?} {src}->{dst}: {sum}");
                    assert!(t.start + t.weights.len() <= src);
                }
            }
        }
    }

    #[test]
    fn area_kernel_averages_exact_blocks() {
        let taps = axis_taps(6, 2, Kernel::Area);
        assert_eq!(taps[0].start, 0);
        assert!(
            taps[0]
                .weights
                .iter()
                .all(|&w| (w - 1.0 / 3.0).abs() < 1e-6)
        );
        assert_eq!(taps[0].weights.len(), 3);
        assert_eq!(taps[1].start, 3);
        // Fractional coverage: 5 -> 2 gives 2.5 samples per output.
        let taps = axis_taps(5, 2, Kernel::Area);
        assert_eq!(taps[0].weights, vec![0.4, 0.4, 0.2]);
    }

    #[test]
    fn identity_resize_preserves_values() {
        let src: Vec<f32> = (0..12).map(|v| v as f32 / 12.0).collect();
        for kernel in [Kernel::Area, Kernel::Triangle] {
            let out = resize_rows(4, 3, 4, 3, kernel, |y, row| {
                row.copy_from_slice(&src[y * 4..y * 4 + 4])
            });
            for (a, b) in out.iter().zip(&src) {
                assert!((a - b).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn constant_image_stays_constant_when_resized() {
        let f = frame(37, 23, |_, _| 128);
        let g = GrayImage::from_frame(&f, 10, 7);
        assert!(g.data.iter().all(|&v| (v - 128.0 / 255.0).abs() < 1e-5));
        let up = resize_rows(3, 2, 11, 5, Kernel::Triangle, |_, row| row.fill(0.25));
        assert!(up.iter().all(|&v| (v - 0.25).abs() < 1e-6));
    }

    #[test]
    fn from_frame_uses_luma_weights() {
        let data: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255];
        let f = VideoFrame::new(
            SourceId("t".into()),
            0,
            Duration::ZERO,
            3,
            1,
            Arc::from(data),
        )
        .unwrap();
        let g = GrayImage::from_frame(&f, 3, 1);
        let expected = [0.299, 0.587, 0.114];
        for (a, b) in g.data.iter().zip(expected) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn downsample2_averages_blocks_and_drops_odd_edge() {
        let g = GrayImage::new(3, 2, vec![0.0, 1.0, 0.5, 1.0, 0.0, 0.5]);
        let d = g.downsample2();
        assert_eq!((d.width, d.height), (1, 1));
        assert!((d.data[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn bilinear_interpolates_between_centres_and_clamps() {
        let g = GrayImage::new(2, 1, vec![0.0, 1.0]);
        assert!((g.bilinear(1.0, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(g.bilinear(0.5, 0.5), 0.0);
        assert_eq!(g.bilinear(-10.0, 0.5), 0.0);
        assert_eq!(g.bilinear(10.0, 99.0), 1.0);
    }

    #[test]
    fn sample_patch_reproduces_pixels_at_native_scale() {
        let data: Vec<f32> = (0..16).map(|v| v as f32 / 16.0).collect();
        let g = GrayImage::new(4, 4, data.clone());
        let p = g.sample_patch(2.0, 2.0, 4.0, 4.0, 4, 4);
        for (a, b) in p.iter().zip(&data) {
            assert!((a - b).abs() < 1e-6);
        }
        // Shrinking a checkerboard averages it instead of aliasing.
        let checker = GrayImage::new(
            4,
            4,
            (0..16).map(|i| ((i % 4 + i / 4) % 2) as f32).collect(),
        );
        let small = checker.sample_patch(2.0, 2.0, 4.0, 4.0, 2, 2);
        assert!(small.iter().all(|&v| (v - 0.5).abs() < 1e-6));
    }

    #[test]
    fn dot_matches_naive_sum() {
        let a: Vec<f32> = (0..19).map(|v| v as f32 * 0.5).collect();
        let b: Vec<f32> = (0..19).map(|v| 1.0 - v as f32 * 0.1).collect();
        let naive: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!((dot(&a, &b) - naive).abs() < 1e-4);
        assert_eq!(dot(&[], &[]), 0.0);
    }
}
