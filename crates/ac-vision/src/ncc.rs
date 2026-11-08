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
//! Because NCC divides out both standard deviations, it cannot tell a faint
//! patch from a contrasty one of the same shape: a shadow on a wall and a
//! person in front of it correlate as well as the shadow with itself. The
//! [`Contrast`] of a placement keeps the two variances it divided out, on the
//! same visible pixels the score used, so a caller can ask how much stronger
//! the window is than the template (`sqrt(var_I / var_T)`, the gain) at O(1)
//! per placement from the same integral images.
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

    /// The part of the template placed with its top-left corner at `(x, y)`
    /// that lies inside `level`, or `None` when less than `min_visible` of
    /// the template's area does.
    fn overlap(&self, level: &Level, x: isize, y: isize, min_visible: f64) -> Option<Overlap> {
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
        Some(Overlap {
            tx0: tx0 as usize,
            ty0: ty0 as usize,
            tx1: tx1 as usize,
            ty1: ty1 as usize,
            ix0: (x + tx0) as usize,
            iy0: (y + ty0) as usize,
            n,
        })
    }

    /// `(Σ T, Σ T², Σ I, Σ I²)` over the overlap `o`, from integral images.
    fn sums(&self, level: &Level, o: &Overlap) -> (f64, f64, f64, f64) {
        let (st, st2) = if o.n == self.w * self.h {
            (0.0, self.energy)
        } else {
            (
                self.sum.sum(o.tx0, o.ty0, o.tx1, o.ty1),
                self.sq.sum(o.tx0, o.ty0, o.tx1, o.ty1),
            )
        };
        let (ix1, iy1) = (o.ix0 + o.tx1 - o.tx0, o.iy0 + o.ty1 - o.ty0);
        let si = level.sum.sum(o.ix0, o.iy0, ix1, iy1);
        let si2 = level.sq.sum(o.ix0, o.iy0, ix1, iy1);
        (st, st2, si, si2)
    }

    /// NCC with the template's top-left corner at `(x, y)` in `level`.
    ///
    /// Returns `None` when less than `min_visible` of the template's area lies
    /// inside the image.
    pub fn ncc_at(&self, level: &Level, x: isize, y: isize, min_visible: f64) -> Option<f32> {
        let o = self.overlap(level, x, y, min_visible)?;
        let cols = o.tx1 - o.tx0;
        let mut cross = 0.0f64;
        for ty in o.ty0..o.ty1 {
            let t = &self.data[ty * self.w + o.tx0..ty * self.w + o.tx1];
            let row = o.iy0 + ty - o.ty0;
            let start = row * level.img.width + o.ix0;
            cross += dot(t, &level.img.data[start..start + cols]) as f64;
        }
        let (st, st2, si, si2) = self.sums(level, &o);
        let nf = o.n as f64;
        let var_t = st2 - st * st / nf;
        let var_i = si2 - si * si / nf;
        if var_t <= MIN_VARIANCE * nf || var_i <= MIN_VARIANCE * nf {
            return Some(0.0);
        }
        let ncc = (cross - st * si / nf) / (var_t * var_i).sqrt();
        Some(ncc.clamp(-1.0, 1.0) as f32)
    }

    /// Contrast of the template and the image window at the placement
    /// `(x, y)`, both measured on the visible overlap, in O(1) from the
    /// integral images. `None` exactly where [`Template::ncc_at`] is.
    pub fn contrast_at(
        &self,
        level: &Level,
        x: isize,
        y: isize,
        min_visible: f64,
    ) -> Option<Contrast> {
        let o = self.overlap(level, x, y, min_visible)?;
        let (st, st2, si, si2) = self.sums(level, &o);
        let nf = o.n as f64;
        Some(Contrast {
            template: (st2 - st * st / nf) / nf,
            window: (si2 - si * si / nf) / nf,
        })
    }

    /// NCC with an equally sized `patch` (row-major, `w * h` samples), and
    /// the contrast of both sides, which the normalisation computes anyway.
    pub fn ncc_patch(&self, patch: &[f32]) -> (f32, Contrast) {
        assert_eq!(patch.len(), self.data.len(), "patch size");
        let n = patch.len() as f64;
        let mean = patch.iter().map(|&v| v as f64).sum::<f64>() / n;
        let var: f64 = patch.iter().map(|&v| (v as f64 - mean).powi(2)).sum();
        let contrast = Contrast {
            template: self.energy / n,
            window: var / n,
        };
        if var <= MIN_VARIANCE * n {
            return (0.0, contrast);
        }
        // The template is zero-mean, so the patch mean cancels from Σ T·I.
        let cross = dot(&self.data, patch) as f64;
        let ncc = (cross / (self.energy * var).sqrt()).clamp(-1.0, 1.0) as f32;
        (ncc, contrast)
    }
}

/// The part of a template placement that lies inside the image.
#[derive(Debug, Clone, Copy)]
struct Overlap {
    /// First template column inside the image.
    tx0: usize,
    /// First template row inside the image.
    ty0: usize,
    /// End (exclusive) of the template columns inside the image.
    tx1: usize,
    /// End (exclusive) of the template rows inside the image.
    ty1: usize,
    /// Image column of template column `tx0`.
    ix0: usize,
    /// Image row of template row `ty0`.
    iy0: usize,
    /// Pixels in the overlap.
    n: usize,
}

/// Contrast of the two sides of one placement: variances per pixel of the
/// template and of the image window, over the same pixels.
///
/// NCC divides both out, so it scores a faint template against a contrasty
/// window (or the reverse) as if they had the same contrast. The gain puts
/// that factor back for the tracker's trust rules.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Contrast {
    /// Variance per pixel of the template.
    pub template: f64,
    /// Variance per pixel of the image window.
    pub window: f64,
}

impl Contrast {
    /// Contrast gain `sqrt(window / template)`: how many times stronger the
    /// window's deviations from its mean are than the template's. `0` for a
    /// flat window and infinite for a flat template; NCC scores either `0`.
    pub fn gain(&self) -> f64 {
        if self.window <= MIN_VARIANCE {
            0.0
        } else if self.template <= MIN_VARIANCE {
            f64::INFINITY
        } else {
            (self.window / self.template).sqrt()
        }
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
mod tests;
