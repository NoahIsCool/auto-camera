//! Normalized cross-correlation (NCC) template matching on an image pyramid.
//!
//! For a template `T` and an equally sized image window `I` over `n` pixels,
//!
//! ```text
//! NCC = (Σ T·I − Σ T · Σ I / n) / sqrt((Σ T² − (Σ T)² / n) · (Σ I² − (Σ I)² / n))
//! ```
//!
//! which is the Pearson correlation of the two patches: `1` for a perfect match
//! under any brightness and contrast change, around `0` for unrelated texture,
//! and defined here as `0` when either side is flat (no variance).
//!
//! Window sums `Σ I` and `Σ I²` come from integral images in O(1) per
//! position, so the cost per position is one `n`-element dot product. The
//! template is stored zero-mean, which makes `Σ T = 0` for a full overlap and
//! avoids subtracting two large, nearly equal numbers.
//!
//! Placements may hang over the image border: the score is then computed on
//! the overlapping part only (template sub-sums also come from integral
//! images), provided at least a minimum fraction of the template is visible.
//! This keeps subjects trackable as they leave the frame.
//!
//! A [`ScoreMap`] evaluates every integer offset in a square window around a
//! predicted placement. Its peak is refined to sub-pixel precision with a
//! parabola through the peak and its neighbours on each axis, and its
//! strongest *separate* local maximum (outside an exclusion radius around the
//! peak) measures ambiguity: two similar peaks mean the match cannot be
//! trusted.

use rayon::prelude::*;

use crate::image::{GrayImage, dot};

/// Variance per pixel below which a patch counts as flat (about a quarter of
/// one 8-bit grey level of standard deviation).
const MIN_VARIANCE: f64 = 1e-6;

/// Summed-area table: entry `(x, y)` is the sum of all samples above and to
/// the left of `(x, y)`.
#[derive(Debug, Clone)]
pub(crate) struct Integral {
    /// Row length (`width + 1`).
    stride: usize,
    /// `(width + 1) * (height + 1)` partial sums.
    data: Vec<f64>,
}

impl Integral {
    /// Integral images of `values` and of their squares.
    pub fn pair(width: usize, height: usize, values: &[f32]) -> (Integral, Integral) {
        let stride = width + 1;
        let mut sum = vec![0.0f64; stride * (height + 1)];
        let mut sq = vec![0.0f64; stride * (height + 1)];
        for y in 0..height {
            let (mut row_sum, mut row_sq) = (0.0f64, 0.0f64);
            for x in 0..width {
                let v = values[y * width + x] as f64;
                row_sum += v;
                row_sq += v * v;
                let i = (y + 1) * stride + x + 1;
                sum[i] = sum[i - stride] + row_sum;
                sq[i] = sq[i - stride] + row_sq;
            }
        }
        (
            Integral { stride, data: sum },
            Integral { stride, data: sq },
        )
    }

    /// Sum over the half-open rectangle `[x0, x1) x [y0, y1)`.
    pub fn sum(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> f64 {
        let at = |x: usize, y: usize| self.data[y * self.stride + x];
        at(x1, y1) - at(x0, y1) - at(x1, y0) + at(x0, y0)
    }
}

/// One pyramid level with its integral images.
#[derive(Debug, Clone)]
pub(crate) struct Level {
    /// Pixels.
    pub img: GrayImage,
    /// Integral of the pixels.
    sum: Integral,
    /// Integral of the squared pixels.
    sq: Integral,
}

impl Level {
    /// Wraps `img` and computes its integral images.
    pub fn new(img: GrayImage) -> Self {
        let (sum, sq) = Integral::pair(img.width, img.height, &img.data);
        Self { img, sum, sq }
    }
}

/// A mean pyramid built by repeated 2x2 averaging.
///
/// Level `k` has about `1 / 2^k` of the base resolution. Construction costs
/// about `4/3` of the base pixel count, plus the integral images.
#[derive(Debug, Clone)]
pub(crate) struct Pyramid {
    /// Levels from finest (`0`) to coarsest.
    pub levels: Vec<Level>,
}

impl Pyramid {
    /// Builds up to `max_levels` levels, stopping before a level's shorter
    /// side would drop below `min_side` pixels. Always has at least one level.
    pub fn new(base: GrayImage, min_side: usize, max_levels: usize) -> Self {
        let mut images = vec![base];
        while images.len() < max_levels.max(1) {
            let last = &images[images.len() - 1];
            if last.width.min(last.height) / 2 < min_side.max(1) {
                break;
            }
            let next = last.downsample2();
            images.push(next);
        }
        Self {
            levels: images.into_par_iter().map(Level::new).collect(),
        }
    }
}

/// A zero-mean template ready for matching.
#[derive(Debug, Clone)]
pub(crate) struct Template {
    /// Width in pixels.
    pub w: usize,
    /// Height in pixels.
    pub h: usize,
    /// Zero-mean samples.
    data: Vec<f32>,
    /// Integral of `data` (for partial overlaps).
    sum: Integral,
    /// Integral of `data²` (for partial overlaps).
    sq: Integral,
    /// `Σ data²` over the full template.
    energy: f64,
}

impl Template {
    /// Prepares `raw` (`w * h` samples). Returns `None` for a flat patch.
    pub fn new(w: usize, h: usize, raw: &[f32]) -> Option<Self> {
        assert_eq!(raw.len(), w * h, "template size");
        let n = (w * h) as f64;
        let mean = raw.iter().map(|&v| v as f64).sum::<f64>() / n;
        let data: Vec<f32> = raw.iter().map(|&v| (v as f64 - mean) as f32).collect();
        let energy: f64 = data.iter().map(|&v| (v as f64) * (v as f64)).sum();
        if energy <= MIN_VARIANCE * n {
            return None;
        }
        let (sum, sq) = Integral::pair(w, h, &data);
        Some(Self {
            w,
            h,
            data,
            sum,
            sq,
            energy,
        })
    }

    /// NCC with the template's top-left corner at `(x, y)` in `level`.
    ///
    /// Returns `None` when less than `min_visible` of the template's area lies
    /// inside the image.
    pub fn ncc_at(&self, level: &Level, x: isize, y: isize, min_visible: f64) -> Option<f32> {
        let (iw, ih) = (level.img.width as isize, level.img.height as isize);
        let (tw, th) = (self.w as isize, self.h as isize);
        let (tx0, ty0) = ((-x).max(0), (-y).max(0));
        let (tx1, ty1) = ((iw - x).min(tw), (ih - y).min(th));
        if tx1 <= tx0 || ty1 <= ty0 {
            return None;
        }
        let n = ((tx1 - tx0) * (ty1 - ty0)) as usize;
        if (n as f64) < min_visible * (self.w * self.h) as f64 {
            return None;
        }
        let (tx0, ty0, tx1, ty1) = (tx0 as usize, ty0 as usize, tx1 as usize, ty1 as usize);
        let (ix0, iy0) = ((x + tx0 as isize) as usize, (y + ty0 as isize) as usize);
        let cols = tx1 - tx0;
        let mut cross = 0.0f64;
        for ty in ty0..ty1 {
            let t = &self.data[ty * self.w + tx0..ty * self.w + tx1];
            let row = iy0 + ty - ty0;
            let i =
                &level.img.data[row * level.img.width + ix0..row * level.img.width + ix0 + cols];
            cross += dot(t, i) as f64;
        }
        let (st, st2) = if n == self.w * self.h {
            (0.0, self.energy)
        } else {
            (
                self.sum.sum(tx0, ty0, tx1, ty1),
                self.sq.sum(tx0, ty0, tx1, ty1),
            )
        };
        let (ix1, iy1) = (ix0 + cols, iy0 + (ty1 - ty0));
        let si = level.sum.sum(ix0, iy0, ix1, iy1);
        let si2 = level.sq.sum(ix0, iy0, ix1, iy1);
        let nf = n as f64;
        let var_t = st2 - st * st / nf;
        let var_i = si2 - si * si / nf;
        if var_t <= MIN_VARIANCE * nf || var_i <= MIN_VARIANCE * nf {
            return Some(0.0);
        }
        let ncc = (cross - st * si / nf) / (var_t * var_i).sqrt();
        Some(ncc.clamp(-1.0, 1.0) as f32)
    }

    /// NCC with an equally sized `patch` (row-major, `w * h` samples).
    pub fn ncc_patch(&self, patch: &[f32]) -> f32 {
        assert_eq!(patch.len(), self.data.len(), "patch size");
        let n = patch.len() as f64;
        let mean = patch.iter().map(|&v| v as f64).sum::<f64>() / n;
        let var: f64 = patch.iter().map(|&v| (v as f64 - mean).powi(2)).sum();
        if var <= MIN_VARIANCE * n {
            return 0.0;
        }
        // The template is zero-mean, so the patch mean cancels from Σ T·I.
        let cross = dot(&self.data, patch) as f64;
        (cross / (self.energy * var).sqrt()).clamp(-1.0, 1.0) as f32
    }
}

/// NCC scores for every integer offset in `-radius..=radius` on both axes
/// around an origin placement. Invalid placements score `-inf`.
#[derive(Debug, Clone)]
pub(crate) struct ScoreMap {
    /// Half-width of the window.
    pub radius: usize,
    /// Row-major scores, `(2 * radius + 1)²` entries.
    scores: Vec<f32>,
}

/// The best placement found in a [`ScoreMap`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Peak {
    /// Horizontal offset from the origin, in pixels (integer part).
    pub dx: isize,
    /// Vertical offset from the origin, in pixels (integer part).
    pub dy: isize,
    /// NCC at the peak.
    pub score: f32,
}

impl ScoreMap {
    /// Scores `template` at top-left placements `(ox + dx, oy + dy)`.
    ///
    /// Costs `(2 * radius + 1)²` dot products of the template's size; rows of
    /// the window are evaluated in parallel.
    pub fn compute(
        level: &Level,
        template: &Template,
        ox: isize,
        oy: isize,
        radius: usize,
        min_visible: f64,
    ) -> Self {
        let side = 2 * radius + 1;
        let r = radius as isize;
        let mut scores = vec![f32::NEG_INFINITY; side * side];
        scores
            .par_chunks_mut(side)
            .enumerate()
            .for_each(|(j, row)| {
                let y = oy + j as isize - r;
                for (i, s) in row.iter_mut().enumerate() {
                    let x = ox + i as isize - r;
                    if let Some(v) = template.ncc_at(level, x, y, min_visible) {
                        *s = v;
                    }
                }
            });
        Self { radius, scores }
    }

    /// A map from precomputed row-major scores.
    pub fn from_scores(radius: usize, scores: Vec<f32>) -> Self {
        assert_eq!(scores.len(), (2 * radius + 1).pow(2), "score map size");
        Self { radius, scores }
    }

    /// Score at offset `(dx, dy)`; `-inf` outside the window.
    pub fn get(&self, dx: isize, dy: isize) -> f32 {
        let r = self.radius as isize;
        if dx.abs() > r || dy.abs() > r {
            return f32::NEG_INFINITY;
        }
        let side = 2 * self.radius + 1;
        self.scores[(dy + r) as usize * side + (dx + r) as usize]
    }

    /// Offsets of the window in row-major order.
    fn offsets(&self) -> impl Iterator<Item = (isize, isize)> + '_ {
        let r = self.radius as isize;
        (-r..=r).flat_map(move |dy| (-r..=r).map(move |dx| (dx, dy)))
    }

    /// Highest valid score; the first in row-major order on ties.
    pub fn best(&self) -> Option<Peak> {
        let mut best: Option<Peak> = None;
        for (dx, dy) in self.offsets() {
            let score = self.get(dx, dy);
            if score.is_finite() && best.is_none_or(|b| score > b.score) {
                best = Some(Peak { dx, dy, score });
            }
        }
        best
    }

    /// Strongest local maximum farther than `exclusion` pixels (Chebyshev
    /// distance) from `peak`, or `-inf` when there is none.
    ///
    /// Only interior positions with all eight neighbours valid count, so the
    /// slope of the main peak running into the window edge is not mistaken
    /// for a second object.
    pub fn second_peak(&self, peak: Peak, exclusion: usize) -> f32 {
        let ex = exclusion as isize;
        let r = self.radius as isize;
        let mut second = f32::NEG_INFINITY;
        for (dx, dy) in self.offsets() {
            if (dx - peak.dx).abs().max((dy - peak.dy).abs()) <= ex
                || dx.abs() == r
                || dy.abs() == r
            {
                continue;
            }
            let s = self.get(dx, dy);
            if !s.is_finite() || s <= second {
                continue;
            }
            let neighbours = (-1..=1).flat_map(|j| (-1..=1).map(move |i| (i, j)));
            let is_max = neighbours.filter(|&(i, j)| i != 0 || j != 0).all(|(i, j)| {
                let n = self.get(dx + i, dy + j);
                n.is_finite() && n <= s
            });
            if is_max {
                second = s;
            }
        }
        second
    }

    /// Sub-pixel offset of `peak` from parabolas through its neighbours, each
    /// axis in `-0.5..=0.5`. Zero on an axis whose neighbours are missing.
    pub fn subpixel(&self, peak: Peak) -> (f64, f64) {
        let c = peak.score;
        let fit = |l: f32, r: f32| -> f64 {
            if !l.is_finite() || !r.is_finite() {
                return 0.0;
            }
            let curvature = l as f64 - 2.0 * c as f64 + r as f64;
            if curvature >= 0.0 {
                return 0.0;
            }
            (0.5 * (l as f64 - r as f64) / curvature).clamp(-0.5, 0.5)
        };
        let (x, y) = (peak.dx, peak.dy);
        (
            fit(self.get(x - 1, y), self.get(x + 1, y)),
            fit(self.get(x, y - 1), self.get(x, y + 1)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random texture in `0..1`.
    fn noise(w: usize, h: usize, seed: u64) -> GrayImage {
        let mut s = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let data = (0..w * h)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s % 1000) as f32 / 1000.0
            })
            .collect();
        GrayImage::new(w, h, data)
    }

    fn crop(img: &GrayImage, x: usize, y: usize, w: usize, h: usize) -> Vec<f32> {
        (y..y + h)
            .flat_map(|r| img.row(r)[x..x + w].to_vec())
            .collect()
    }

    #[test]
    fn integral_sums_rectangles() {
        let data: Vec<f32> = (1..=6).map(|v| v as f32).collect();
        let (sum, sq) = Integral::pair(3, 2, &data);
        assert_eq!(sum.sum(0, 0, 3, 2), 21.0);
        assert_eq!(sum.sum(1, 0, 3, 2), 2.0 + 3.0 + 5.0 + 6.0);
        assert_eq!(sum.sum(1, 1, 1, 2), 0.0);
        assert_eq!(sq.sum(0, 1, 1, 2), 16.0);
    }

    #[test]
    fn pyramid_halves_until_min_side() {
        let p = Pyramid::new(noise(64, 40, 1), 8, 10);
        let sizes: Vec<(usize, usize)> = p
            .levels
            .iter()
            .map(|l| (l.img.width, l.img.height))
            .collect();
        assert_eq!(sizes, vec![(64, 40), (32, 20), (16, 10)]);
        assert_eq!(Pyramid::new(noise(4, 4, 1), 8, 10).levels.len(), 1);
        assert_eq!(Pyramid::new(noise(64, 64, 1), 1, 2).levels.len(), 2);
    }

    #[test]
    fn flat_template_is_rejected() {
        assert!(Template::new(4, 4, &[0.5; 16]).is_none());
        assert!(Template::new(2, 1, &[0.0, 1.0]).is_some());
    }

    #[test]
    fn exact_match_scores_one_and_is_found() {
        let img = noise(60, 40, 7);
        let level = Level::new(img.clone());
        let t = Template::new(12, 10, &crop(&img, 30, 15, 12, 10)).unwrap();
        assert!((t.ncc_at(&level, 30, 15, 1.0).unwrap() - 1.0).abs() < 1e-5);
        let map = ScoreMap::compute(&level, &t, 27, 17, 5, 1.0);
        let peak = map.best().unwrap();
        assert_eq!((peak.dx, peak.dy), (3, -2));
        assert!(peak.score > 0.999);
        // Unrelated noise correlates weakly.
        assert!(map.get(-3, 0) < 0.5);
    }

    #[test]
    fn ncc_ignores_brightness_and_contrast() {
        let img = noise(30, 30, 3);
        let t = Template::new(8, 8, &crop(&img, 10, 10, 8, 8)).unwrap();
        let shifted = GrayImage::new(30, 30, img.data.iter().map(|v| 0.2 + 0.5 * v).collect());
        let s = t.ncc_at(&Level::new(shifted), 10, 10, 1.0).unwrap();
        assert!((s - 1.0).abs() < 1e-4);
    }

    #[test]
    fn flat_window_scores_zero() {
        let t = Template::new(4, 4, &crop(&noise(8, 8, 2), 0, 0, 4, 4)).unwrap();
        let flat = Level::new(GrayImage::new(10, 10, vec![0.3; 100]));
        assert_eq!(t.ncc_at(&flat, 2, 2, 1.0), Some(0.0));
    }

    #[test]
    fn partial_overlap_matches_visible_part() {
        let img = noise(40, 30, 11);
        let level = Level::new(img.clone());
        // Template of the 10x10 block at (0, 5); place it 4 px further left so
        // only 6 of its 10 columns are inside the image.
        let block = crop(&img, 0, 5, 10, 10);
        let shifted: Vec<f32> = (0..10)
            .flat_map(|r| {
                let row = &block[r * 10..r * 10 + 10];
                let mut v = vec![0.9; 4];
                v.extend_from_slice(&row[..6]);
                v
            })
            .collect();
        let t = Template::new(10, 10, &shifted).unwrap();
        let s = t.ncc_at(&level, -4, 5, 0.5).unwrap();
        assert!((s - 1.0).abs() < 1e-4, "{s}");
        assert_eq!(t.ncc_at(&level, -4, 5, 0.7), None);
        assert_eq!(t.ncc_at(&level, -10, 5, 0.0), None);
        assert_eq!(t.ncc_at(&level, 40, 5, 0.0), None);
    }

    #[test]
    fn second_peak_finds_a_distractor_but_not_the_main_slope() {
        let mut img = noise(80, 30, 5);
        let patch = crop(&img, 10, 10, 10, 10);
        // Paste an identical copy 30 px to the right.
        for r in 0..10 {
            for c in 0..10 {
                img.data[(10 + r) * 80 + 40 + c] = patch[r * 10 + c];
            }
        }
        let level = Level::new(img);
        let t = Template::new(10, 10, &patch).unwrap();
        let map = ScoreMap::compute(&level, &t, 25, 10, 18, 1.0);
        let peak = map.best().unwrap();
        assert!(peak.score > 0.999);
        assert!(map.second_peak(peak, 5) > 0.999);
        let near = ScoreMap::compute(&level, &t, 10, 10, 6, 1.0);
        let p = near.best().unwrap();
        assert_eq!((p.dx, p.dy), (0, 0));
        assert!(near.second_peak(p, 5) < 0.6);
    }

    #[test]
    fn subpixel_refines_towards_the_higher_neighbour() {
        let map = ScoreMap::from_scores(1, vec![0.0, 0.5, 0.0, 0.6, 1.0, 0.8, 0.0, 0.5, 0.0]);
        let peak = map.best().unwrap();
        let (sx, sy) = map.subpixel(peak);
        assert!(sx > 0.0 && sx < 0.5, "{sx}");
        assert!(sy.abs() < 1e-9);
        // A peak on the window edge has no right neighbour.
        let edge = Peak {
            dx: 1,
            dy: 0,
            score: 0.8,
        };
        assert_eq!(map.subpixel(edge).0, 0.0);
    }

    #[test]
    fn patch_ncc_matches_placement_ncc() {
        let img = noise(30, 30, 9);
        let block = crop(&img, 5, 6, 9, 7);
        let t = Template::new(9, 7, &block).unwrap();
        assert!((t.ncc_patch(&block) - 1.0).abs() < 1e-5);
        let other = crop(&img, 15, 16, 9, 7);
        let level = Level::new(img);
        let placed = t.ncc_at(&level, 15, 16, 1.0).unwrap();
        assert!((t.ncc_patch(&other) - placed).abs() < 1e-4);
        assert_eq!(t.ncc_patch(&[0.2; 63]), 0.0);
    }

    #[test]
    fn empty_map_has_no_peak() {
        let t = Template::new(4, 4, &crop(&noise(8, 8, 2), 0, 0, 4, 4)).unwrap();
        let level = Level::new(noise(10, 10, 4));
        let map = ScoreMap::compute(&level, &t, 100, 100, 2, 1.0);
        assert!(map.best().is_none());
        assert_eq!(map.get(5, 0), f32::NEG_INFINITY);
    }
}
