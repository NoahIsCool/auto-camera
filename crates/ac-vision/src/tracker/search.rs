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
//! # Trust
//!
//! A match moves the box only when it is [trusted](Match::trusted): a peak
//! of at least `accept_score`, with no separate local maximum within
//! `ambiguity_margin` of it, that the contrast check below does not hold
//! back. `step` coasts the box otherwise. An ambiguous match, or a match of
//! at least `accept_score` held back by its contrast, reports at most
//! `ambiguous_confidence`; a match below `accept_score` reports its own score
//! (an occluded subject's confidence sinks with its visible part).
//!
//! # The contrast check, on surprising moves only
//!
//! NCC is blind to contrast: it divides both patches by their own standard
//! deviation. A faint template, such as a shadow on a wall whose box varies
//! by seven grey levels, matches a nearby high-contrast performer of the same
//! shape at 0.70 to 0.76 (or a gentle wall relief at about 0.66) while its
//! own score sinks as the performer enters its window. So each match carries
//! its contrast gain: the window's standard deviation over the template's,
//! on the same visible pixels (O(1) from the integral images).
//!
//! A light cue changes a real performer's contrast just as much, though, and
//! does not always keep their match strong. A wash scales every grey level of
//! the box, which NCC ignores (the match stays at about 0.9, is learned and
//! the template takes on the new contrast). A followspot that lights the
//! performer but not the backdrop around them, inside their box too, also
//! changes the box's structure: the match sinks to about 0.7 at a gain of 5
//! to 6, below `update_score`, so it is never learned and the gain stays out
//! of range for as long as the spot is on. A check on the gain alone would
//! hold that performer back for the whole cue.
//!
//! What tells the two apart is where the match lands. The performer under a
//! cue is where they were heading, near the constant-velocity prediction;
//! the faint template's jumps onto a lookalike or onto the wall land a box
//! width or more away. So a match is *surprising* when its centre lies more
//! than `surprise_fraction` (default 0.25) of the box's shorter side, at the
//! matching level, from the predicted centre, and the check holds back only a
//! surprising match that scores at least `accept_score` but below
//! `update_score` and whose gain is outside `1 / max_contrast_gain ..=
//! max_contrast_gain`. A match near the prediction is judged as before,
//! whatever its gain; a match of at least `update_score` moves the box
//! wherever it lands.
//!
//! The distance widens with the search window, by `dt / nominal_dt` up to
//! [`MAX_WINDOW_GROWTH`] times, because the prediction is less certain over
//! a longer interval. That matters most after a late start: a tracker
//! started on a detection's frame, several frames old, has no velocity yet,
//! so its first update's catch-up move is unpredicted. At the nominal
//! distance, a performer who walked a third of a box width between the
//! detection's frame and the first update, under a spot that came on in
//! between, was held back on that move and on every frame after it, until
//! they were lost and came back as a new subject. Widened, a catch-up of up
//! to three quarters of the shorter side is judged on its score, while the
//! jumps above are still a box width or more.
//!
//! What the rule gives up: a weak gain-mismatched match that creeps towards
//! a distractor in steps shorter than the distance is followed. And a
//! performer under a sustained spot, whose template therefore keeps the old
//! contrast, who lands farther than the distance from the prediction in one
//! frame (a third of a box width sideways, say) is held back from then on,
//! until the light changes or a detection restarts the tracker. A step that
//! small leaves the coasting box overlapping the performer above the store's
//! `reseed_iou`, so a detection agrees with the box (a longer one re-seeds
//! as a drift). So the subject store restarts a tracker that coasted on
//! every frame since the subject's previous detection from the next one, and
//! a subject whose detections come further apart than its `lost_after` is
//! lost first and restarted by the detection that recovers it; either way
//! the new template is taken under the spot. Velocity prediction covers ordinary motion:
//! stops, turns, sway and a dash that builds up over a few frames all stay
//! within it. The parent module's docs name the case no contrast check helps
//! with, a start box that already holds someone else's pixels.
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
//!   for a scale probe. Whether it is surprising is decided once, from the
//!   integer peak's refined centre; a scale probe, within a pixel and a
//!   half of that centre on each axis, inherits it.

use crate::image::GrayImage;
use crate::ncc::{Level, ScoreMap, Template};

use super::TemplateTrackerConfig;

/// Smallest template side, in pixels, used for matching.
const MIN_TEMPLATE_SIDE: usize = 3;
/// Largest widening of the search window, and of the distance beyond which
/// a match is surprising, after skipped frames or a late start.
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
    /// The integer peak lies farther than `surprise_fraction` of the box's
    /// shorter side, widened with the search window, from the predicted
    /// centre.
    pub(super) surprising: bool,
}

impl Match {
    /// True when the contrast check holds the box back although the score
    /// alone would move it: a surprising match of at least `accept_score`
    /// and below `update_score` whose window's contrast is off the
    /// template's by more than `max_contrast_gain` either way.
    pub(super) fn contrast_refused(&self, config: &TemplateTrackerConfig) -> bool {
        let g = config.max_contrast_gain;
        self.surprising
            && (config.accept_score..config.update_score).contains(&self.score)
            && !(1.0 / g..=g).contains(&self.gain)
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
/// size finds the peak and checks ambiguity, distance from the prediction
/// and contrast; then, if the peak
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
    let (cx, cy) = (
        (ox + peak.dx) as f64 + sx + tw as f64 / 2.0,
        (oy + peak.dy) as f64 + sy + th as f64 / 2.0,
    );
    let coarse = Match {
        cx,
        cy,
        scale: 1.0,
        score: peak.score,
        ambiguous,
        gain,
        surprising: (cx - pcx).hypot(cy - pcy) > config.surprise_fraction * bw.min(bh) * growth,
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
                surprising: coarse.surprising,
            };
        }
    }
    Some(best)
}
