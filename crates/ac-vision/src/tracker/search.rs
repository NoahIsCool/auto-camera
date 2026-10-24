//! Locating one subject on one pyramid level, and the rules that decide
//! whether the result is trusted.
//!
//! [`match_on_level`] is steps 3 and 4 of the tracker's per-frame work (see
//! the module docs of the parent `tracker` module): an integer-offset NCC
//! search around the predicted placement, sub-pixel refinement, the
//! ambiguity and contrast checks on the peak, and for a trusted peak a small
//! scale search. [`Match`] carries the result and the trust rules that
//! `step` applies to it.
//!
//! # Invariants
//!
//! * Pure and deterministic: a match depends only on the level, the
//!   appearance template, the prediction, the box size, `dt` and the tuning.
//! * Every placement is scored on pixels of the matching level; positions
//!   and sizes are in pixels of that level.
//! * The scale search runs only for a trusted integer peak, so a match that
//!   is not trusted never changes the box size.
//! * A match's contrast gain is taken at the placement whose score it
//!   reports: the visible overlap for the integer peak, the resampled patch
//!   for a scale probe.

use crate::image::GrayImage;
use crate::ncc::{Level, ScoreMap, Template};

use super::TemplateTrackerConfig;

/// Smallest template side, in pixels, used for matching.
const MIN_TEMPLATE_SIDE: usize = 3;
/// Largest widening of the search window after skipped frames.
const MAX_WINDOW_GROWTH: f64 = 3.0;
/// Radius, in pixels, of the grid of centres probed at each scale.
const SCALE_PROBE_RADIUS: usize = 1;

/// Result of matching one subject on one frame.
#[derive(Debug, Clone, Copy)]
pub(super) struct Match {
    /// Matched centre in pixels of the matching level.
    pub(super) cx: f64,
    /// Matched centre in pixels of the matching level.
    pub(super) cy: f64,
    /// Scale relative to the previous box size.
    pub(super) scale: f64,
    /// Peak NCC.
    pub(super) score: f32,
    /// A separate peak came within the ambiguity margin.
    pub(super) ambiguous: bool,
    /// Contrast gain of the image window at the match over the template.
    pub(super) gain: f64,
}

impl Match {
    /// True when the contrast check holds the box back: the window's
    /// contrast is off the template's by more than `max_contrast_gain`
    /// either way and the score is below `update_score`.
    pub(super) fn contrast_refused(&self, config: &TemplateTrackerConfig) -> bool {
        let g = config.max_contrast_gain;
        self.score < config.update_score && !(1.0 / g..=g).contains(&self.gain)
    }

    /// True when the match moves the box: a peak of at least `accept_score`,
    /// unambiguous and not refused by the contrast check.
    pub(super) fn trusted(&self, config: &TemplateTrackerConfig) -> bool {
        self.score >= config.accept_score && !self.ambiguous && !self.contrast_refused(config)
    }
}

/// Matches `appearance` for a box of `bw x bh` level pixels predicted at
/// `(pcx, pcy)` on `level`.
///
/// Two stages: an integer-offset search with the template at the current box
/// size finds the peak and checks ambiguity and contrast; then, if the peak
/// is trusted and the enlarged box fits inside the image, the image itself is
/// resampled at scales `1 - step`, `1` and `1 + step` on a 3x3 grid of
/// centres around the peak. Resampling the image (rather than resizing the
/// template to whole pixels) makes the scale steps exact even for small
/// boxes. The returned match carries the contrast gain at its own placement:
/// the visible overlap for the integer peak, the resampled patch for a scale
/// probe.
pub(super) fn match_on_level(
    config: &TemplateTrackerConfig,
    level: &Level,
    appearance: &GrayImage,
    (pcx, pcy): (f64, f64),
    (bw, bh): (f64, f64),
    dt: f64,
) -> Option<Match> {
    let (cw, ch) = (appearance.width as f64, appearance.height as f64);
    let tw = (bw.round() as usize).max(MIN_TEMPLATE_SIDE);
    let th = (bh.round() as usize).max(MIN_TEMPLATE_SIDE);
    let template = Template::new(
        tw,
        th,
        &appearance.sample_patch(cw / 2.0, ch / 2.0, cw, ch, tw, th),
    )?;
    let ox = (pcx - tw as f64 / 2.0).round() as isize;
    let oy = (pcy - th as f64 / 2.0).round() as isize;
    let growth = (dt / config.nominal_dt).clamp(1.0, MAX_WINDOW_GROWTH);
    let radius =
        (config.search_fraction * bw.max(bh)).max(config.min_search_radius as f64) * growth;
    let radius = radius.min(config.max_search_radius as f64).ceil() as usize;
    let map = ScoreMap::compute(level, &template, ox, oy, radius, config.min_visible);
    let peak = map.best()?;
    let exclusion = (tw.min(th) / 2).max(2);
    let ambiguous = map.second_peak(peak, exclusion) > peak.score - config.ambiguity_margin;
    let (sx, sy) = map.subpixel(peak);
    let gain = template
        .contrast_at(level, ox + peak.dx, oy + peak.dy, config.min_visible)
        .map_or(0.0, |c| c.gain());
    let coarse = Match {
        cx: (ox + peak.dx) as f64 + sx + tw as f64 / 2.0,
        cy: (oy + peak.dy) as f64 + sy + th as f64 / 2.0,
        scale: 1.0,
        score: peak.score,
        ambiguous,
        gain,
    };
    let grown = (1.0 + config.scale_step) * bw.max(bh) / 2.0 + SCALE_PROBE_RADIUS as f64 + 1.0;
    let (lw, lh) = (level.img.width as f64, level.img.height as f64);
    let inside = coarse.cx - grown >= 0.0
        && coarse.cy - grown >= 0.0
        && coarse.cx + grown <= lw
        && coarse.cy + grown <= lh;
    if !coarse.trusted(config) || !inside {
        return Some(coarse);
    }
    let mut best = coarse;
    let mut best_adjusted = f32::NEG_INFINITY;
    let scales = [1.0, 1.0 - config.scale_step, 1.0 + config.scale_step];
    let count = if config.scale_step > 0.0 { 3 } else { 1 };
    for &s in &scales[..count] {
        let r = SCALE_PROBE_RADIUS as isize;
        let probes: Vec<((isize, isize), f32, f64)> = (-r..=r)
            .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
            .map(|(dx, dy)| {
                let (x, y) = (coarse.cx + dx as f64, coarse.cy + dy as f64);
                let patch = level.img.sample_patch(x, y, s * bw, s * bh, tw, th);
                let (score, contrast) = template.ncc_patch(&patch);
                ((dx, dy), score, contrast.gain())
            })
            .collect();
        let probe = ScoreMap::from_scores(SCALE_PROBE_RADIUS, probes.iter().map(|q| q.1).collect());
        let Some(p) = probe.best() else { continue };
        let adjusted = if s == 1.0 {
            p.score
        } else {
            p.score - config.scale_penalty
        };
        if adjusted > best_adjusted {
            let (px, py) = probe.subpixel(p);
            best_adjusted = adjusted;
            best = Match {
                cx: coarse.cx + p.dx as f64 + px,
                cy: coarse.cy + p.dy as f64 + py,
                scale: s,
                score: p.score,
                ambiguous: false,
                gain: probes
                    .iter()
                    .find(|q| q.0 == (p.dx, p.dy))
                    .map_or(0.0, |q| q.2),
            };
        }
    }
    Some(best)
}
