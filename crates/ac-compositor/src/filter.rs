//! Separable resampling filters.
//!
//! A crop maps an interval of source pixels onto `out_len` output pixels on
//! each axis independently. In source pixel units the crop starts at `start`
//! and is `len` pixels long, so each output pixel `i` covers the source
//! interval
//!
//! ```text
//! [start + i * s, start + (i + 1) * s)      with   s = len / out_len
//! ```
//!
//! and its centre sits at `start + (i + 0.5) * s` (pixel centres are at
//! half-integers, so source pixel `k` has its centre at `k + 0.5`).
//!
//! * **Magnification or 1:1 (`s <= 1`)** uses bilinear interpolation between
//!   the two source pixels whose centres bracket the output centre. When the
//!   output centre lands exactly on a source centre the single tap has weight
//!   one, so a 1:1 crop on whole pixels copies the source exactly.
//! * **Minification (`s > 1`)** uses an area-average (box) filter: each source
//!   pixel contributes in proportion to how much of it the output pixel's
//!   interval covers. Every source pixel therefore lands in the output with
//!   total weight `1 / s`, which suppresses the aliasing (shimmer on fine
//!   detail such as shirts, LED walls and text) that point or bilinear
//!   sampling would produce when shrinking a 4K picture to 1080p or to a
//!   multiview tile.
//!
//! Coordinates outside the source are clamped to the nearest edge pixel
//! ("clamp to edge"), so a crop that hangs over the frame repeats the border
//! instead of reading out of bounds. Clamped contributions are folded into the
//! edge tap analytically, so the work per output pixel is bounded by the
//! source size however far the crop reaches outside the frame.
//!
//! Invariants of every built table: each output sample has at least one tap,
//! its taps cover consecutive source indices inside `0..src_len`, its weights
//! are non-negative and sum to one (to `f32` rounding), and tap start indices
//! never decrease along the axis.

/// Precomputed filter taps for one axis of one crop.
///
/// The table is rebuilt in place for every layer and frame; its vectors keep
/// their capacity, so steady-state rendering does not allocate.
#[derive(Debug, Default, Clone)]
pub(crate) struct AxisTaps {
    /// First source index read by each output sample.
    starts: Vec<u32>,
    /// Offset of each output sample's weights in `weights`; one entry longer
    /// than `starts`, so sample `i` owns `offsets[i]..offsets[i + 1]`.
    offsets: Vec<u32>,
    /// Concatenated tap weights of all samples.
    weights: Vec<f32>,
}

impl AxisTaps {
    /// Rebuilds the table for a crop of `len` source pixels starting at source
    /// coordinate `start`, resampled to `out_len` pixels, over a source axis of
    /// `src_len` pixels.
    ///
    /// The caller guarantees `src_len >= 1`, `out_len >= 1`, and that `start`
    /// and `len` are finite with `len > 0`.
    pub(crate) fn rebuild(&mut self, src_len: u32, start: f64, len: f64, out_len: u32) {
        self.starts.clear();
        self.offsets.clear();
        self.weights.clear();
        self.offsets.push(0);
        let scale = len / f64::from(out_len);
        for i in 0..out_len {
            let i = f64::from(i);
            if scale <= 1.0 {
                self.push_bilinear(src_len, start + (i + 0.5) * scale - 0.5);
            } else {
                self.push_box(src_len, start + i * scale, start + (i + 1.0) * scale);
            }
        }
    }

    /// Number of output samples.
    pub(crate) fn len(&self) -> usize {
        self.starts.len()
    }

    /// First source index and weights of output sample `i`.
    pub(crate) fn taps(&self, i: usize) -> (usize, &[f32]) {
        let from = self.offsets[i] as usize;
        let to = self.offsets[i + 1] as usize;
        (self.starts[i] as usize, &self.weights[from..to])
    }

    /// Half-open range of source indices read by any sample.
    pub(crate) fn span(&self) -> (usize, usize) {
        let first = self.starts.first().map_or(0, |&s| s as usize);
        let last = self.len().checked_sub(1).map_or(0, |i| {
            let (start, weights) = self.taps(i);
            start + weights.len()
        });
        (first, last)
    }

    /// Appends one sample made of `weights` starting at source index `start`.
    fn push_run(&mut self, start: u32, weights: impl IntoIterator<Item = f32>) {
        self.starts.push(start);
        self.weights.extend(weights);
        self.offsets.push(self.weights.len() as u32);
    }

    /// Appends a bilinear sample centred at continuous source index `center`
    /// (where index `k` is the centre of source pixel `k`).
    fn push_bilinear(&mut self, src_len: u32, center: f64) {
        let last = f64::from(src_len - 1);
        let floor = center.floor();
        let frac = (center - floor) as f32;
        let i0 = floor.clamp(0.0, last) as u32;
        let i1 = (floor + 1.0).clamp(0.0, last) as u32;
        if i0 == i1 || frac == 0.0 {
            self.push_run(i0, [1.0]);
        } else {
            self.push_run(i0, [1.0 - frac, frac]);
        }
    }

    /// Appends an area-average sample over the source interval `[a, b)`,
    /// clamping the parts that lie outside `0..src_len` onto the edge pixels.
    fn push_box(&mut self, src_len: u32, a: f64, b: f64) {
        let n = f64::from(src_len);
        let last = src_len - 1;
        let left = (b.min(0.0) - a).max(0.0);
        let right = (b - a.max(n)).max(0.0);
        let lo = a.clamp(0.0, n);
        let hi = b.clamp(0.0, n);
        let inside = hi > lo;
        let first = if left > 0.0 {
            0
        } else if inside {
            (lo.floor() as u32).min(last)
        } else {
            // Entirely past the right edge (or an interval too small to
            // resolve): everything lands on the nearest edge pixel.
            (a.floor().clamp(0.0, f64::from(last))) as u32
        };
        let end = if right > 0.0 {
            last
        } else if inside {
            ((hi.ceil() as u32).saturating_sub(1)).clamp(first, last)
        } else {
            first
        };
        let offset = self.weights.len();
        let mut sum = 0.0;
        for k in first..=end {
            let kf = f64::from(k);
            let mut w = if inside {
                (hi.min(kf + 1.0) - lo.max(kf)).max(0.0)
            } else {
                0.0
            };
            if k == 0 {
                w += left;
            }
            if k == last {
                w += right;
            }
            sum += w;
            self.weights.push(w as f32);
        }
        if sum > 0.0 && sum.is_finite() {
            let inv = 1.0 / sum;
            for w in &mut self.weights[offset..] {
                *w = (f64::from(*w) * inv) as f32;
            }
            self.starts.push(first);
            self.offsets.push(self.weights.len() as u32);
        } else {
            // The interval vanished in floating point: fall back to the
            // nearest pixel so every sample keeps a weight of one.
            self.weights.truncate(offset);
            let nearest = (0.5 * (a + b)).floor().clamp(0.0, f64::from(last)) as u32;
            self.push_run(nearest, [1.0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(src_len: u32, start: f64, len: f64, out_len: u32) -> AxisTaps {
        let mut t = AxisTaps::default();
        t.rebuild(src_len, start, len, out_len);
        t
    }

    /// Checks the documented invariants of a table.
    fn assert_invariants(t: &AxisTaps, src_len: u32, out_len: u32) {
        assert_eq!(t.len(), out_len as usize);
        let mut prev = 0;
        for i in 0..t.len() {
            let (start, w) = t.taps(i);
            assert!(!w.is_empty(), "sample {i} has no taps");
            assert!(
                start + w.len() <= src_len as usize,
                "sample {i} reads past the edge"
            );
            assert!(start >= prev, "starts must not decrease");
            prev = start;
            assert!(w.iter().all(|&x| x >= 0.0 && x.is_finite()));
            let sum: f32 = w.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "sample {i} weights sum to {sum}");
        }
    }

    #[test]
    fn one_to_one_is_a_single_unit_tap_per_pixel() {
        let t = built(8, 2.0, 4.0, 4);
        assert_invariants(&t, 8, 4);
        for i in 0..4 {
            assert_eq!(t.taps(i), (i + 2, &[1.0f32][..]));
        }
        assert_eq!(t.span(), (2, 6));
    }

    #[test]
    fn halving_averages_pixel_pairs() {
        let t = built(8, 0.0, 8.0, 4);
        assert_invariants(&t, 8, 4);
        for i in 0..4 {
            assert_eq!(t.taps(i), (2 * i, &[0.5f32, 0.5][..]));
        }
    }

    #[test]
    fn non_integer_minification_weights_by_coverage() {
        // Three output pixels over four source pixels: s = 4/3.
        let t = built(4, 0.0, 4.0, 3);
        assert_invariants(&t, 4, 3);
        let (start, w) = t.taps(0);
        assert_eq!(start, 0);
        assert!((w[0] - 0.75).abs() < 1e-6 && (w[1] - 0.25).abs() < 1e-6);
        let (start, w) = t.taps(1);
        assert_eq!(start, 1);
        assert!((w[0] - 0.5).abs() < 1e-6 && (w[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn magnification_interpolates_between_neighbours() {
        let t = built(2, 0.0, 2.0, 4);
        assert_invariants(&t, 2, 4);
        // Centres at 0.25, 0.75, 1.25, 1.75 source px -> indices -0.25 .. 1.25.
        assert_eq!(t.taps(0), (0, &[1.0f32][..]));
        let (start, w) = t.taps(1);
        assert_eq!(start, 0);
        assert!((w[0] - 0.75).abs() < 1e-6 && (w[1] - 0.25).abs() < 1e-6);
        assert_eq!(t.taps(3), (1, &[1.0f32][..]));
    }

    #[test]
    fn crops_outside_the_source_clamp_to_the_edge() {
        let left = built(10, -50.0, 20.0, 5);
        assert_invariants(&left, 10, 5);
        assert!((0..5).all(|i| left.taps(i) == (0, &[1.0f32][..])));
        let right = built(10, 40.0, 4.0, 8);
        assert_invariants(&right, 10, 8);
        assert!((0..8).all(|i| right.taps(i) == (9, &[1.0f32][..])));
    }

    #[test]
    fn huge_crops_stay_bounded_by_the_source() {
        let t = built(16, -1.0e7, 2.0e7, 4);
        assert_invariants(&t, 16, 4);
        let taps: usize = (0..4).map(|i| t.taps(i).1.len()).sum();
        assert!(taps <= 16 + 4);
    }

    #[test]
    fn partly_outside_crop_folds_overhang_into_edge_tap() {
        // Output pixel 0 covers [-2, 2): half clamped onto pixel 0.
        let t = built(8, -2.0, 8.0, 2);
        assert_invariants(&t, 8, 2);
        let (start, w) = t.taps(0);
        assert_eq!(start, 0);
        assert!((w[0] - 0.75).abs() < 1e-6 && (w[1] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn single_pixel_source_and_tiny_crops() {
        let t = built(1, 0.0, 1.0, 7);
        assert_invariants(&t, 1, 7);
        let t = built(100, 50.3, 1.0e-9, 3);
        assert_invariants(&t, 100, 3);
        let t = built(1, 0.0, 1.0, 1);
        assert_eq!(t.span(), (0, 1));
    }

    #[test]
    fn rebuild_reuses_the_table() {
        let mut t = built(64, 0.0, 64.0, 16);
        t.rebuild(4, 0.0, 4.0, 2);
        assert_invariants(&t, 4, 2);
    }
}
