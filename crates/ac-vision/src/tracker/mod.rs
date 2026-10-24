//! A pure-Rust, per-frame template tracker.
//!
//! [`TemplateTracker`] is the fast tier of the two-tier vision design: it
//! follows subjects the detector (or operator) already identified, on every
//! frame, at a fraction of a millisecond per subject.
//!
//! # Per frame
//!
//! 1. The frame is converted to luma and downscaled (area filter) to a working
//!    width of about 320 to 480 px, never enlarged. A mean pyramid (2x2
//!    averaging) with integral images is built on top. One pyramid serves
//!    every subject on the frame: the tracker keeps the pyramid of the newest
//!    frame it saw (keyed by source, sequence number and PTS), so the update
//!    and any number of starts on the same frame (a detection batch that
//!    admits or reseeds several subjects) convert it once.
//! 2. Each subject predicts its centre with a constant-velocity model:
//!    `c' = c + v · dt`, where `dt` comes from the frames' presentation
//!    timestamps (frames skipped by a latest-frame mailbox are accounted for).
//! 3. The subject is matched on the pyramid level where its box's longer side
//!    is at most `template_size` pixels. Its appearance template is resampled
//!    to the box size there and scored with normalized cross-correlation at
//!    every integer offset in a window around the prediction. The window radius is `search_fraction` of the box's longer
//!    side (clamped), widened when frames were skipped.
//! 4. The peak is refined to sub-pixel precision. If it is trusted (see
//!    below), the image is resampled around it at scales `1 - step`, `1`
//!    and `1 + step` on a 3x3 grid of centres and compared with the template;
//!    a scale change must beat the unscaled score by `scale_penalty`, so size
//!    does not drift on noise. Near the frame border, where the enlarged box
//!    would not fit, the scale is held.
//!
//! # Trust
//!
//! The confidence is the peak NCC, in `0..=1`. The box moves to the peak only
//! when the peak is at least `accept_score` *and* no separate local maximum
//! comes within `ambiguity_margin` of it *and* it passes the contrast check
//! below. Otherwise the subject is considered occluded or ambiguous: the box
//! coasts on its decaying velocity instead of jumping to a distractor, and an
//! ambiguous or contrast-refused match reports at most `ambiguous_confidence`.
//! The appearance template learns the new look with `learning_rate` only when
//! the confidence reaches `update_score` and the box is fully inside the
//! frame, so occluders and the frame border are not learned. The tracker
//! never drops a subject by itself.
//!
//! NCC is blind to contrast: it divides both patches by their own standard
//! deviation. A faint template, such as a shadow on a wall whose box varies
//! by seven grey levels, matches a nearby high-contrast performer of the same
//! shape at 0.70 to 0.76 (or a gentle wall gradient at about 0.6) while its
//! own score sinks as the performer enters its window. So each match also carries its
//! contrast gain: the window's standard deviation over the template's, on the
//! same visible pixels (O(1) from the integral images). A match whose gain is
//! outside `1 / max_contrast_gain ..= max_contrast_gain` moves the box only if
//! it reaches `update_score`; below that it coasts like an ambiguous match,
//! without scale probes or learning. It is not refused outright, because a
//! light cue changes a real performer's contrast three to five times in one
//! frame: their match stays strong, is accepted and learned, and the template
//! takes on the new contrast at `learning_rate` per frame.
//!
//! The check does not help when the start box already holds someone else's
//! pixels (a neighbour's head in a corner of a shadow's box): the template
//! then follows that head at its own contrast. That case waits on
//! measurements from the band clip. A false subject such as a shadow is for
//! the operator to hide (and, later, for exclusion zones).
//!
//! # Cost model
//!
//! For a W x H source and working size `w x h` (`w = work_width`):
//!
//! * Conversion: one read of the RGBA frame plus `(W/w + 1)` taps per output
//!   sample per axis; about 4 to 8 M multiply-adds for 1080p, split across
//!   rows with rayon.
//! * Pyramid and integral images: about `3 · w · h` operations (`w · h` is
//!   83 k at 384 x 216).
//! * Per subject: `(2r + 1)²` placements times `tw · th` multiply-adds, with
//!   `tw, th ≤ template_size` and `r ≤ max_search_radius`, plus 27 resampled
//!   scale probes of `tw · th` bilinear samples each. With the defaults (32 px
//!   templates, `r ≤ 16` at 30 fps) that is at most about 1.1 M multiply-adds
//!   plus 28 k bilinear samples, typically a quarter of that. Subjects run in
//!   parallel with rayon.
//!
//! The cost per subject does not depend on the source resolution, and only
//! weakly on the subject size, because larger subjects are matched on coarser
//! levels. Measured with `examples/track_bench.rs` (release build, 2.1 GHz
//! Xeon VM): converting a 1080p frame takes about 7 ms on one core and 3.3 ms on
//! four; each worst-case subject (32 px template, radius 16) adds 0.9 ms of
//! one core; 16 subjects on 1080p run at 7.4 ms per frame on four cores. The
//! conversion dominates and grows with the source, so a caller with a
//! hardware-scaled proxy (for example 640 x 360 from the decoder) can pass
//! that instead: results are in normalized coordinates either way.
//!
//! # Invariants
//!
//! * One tracker serves one source; it binds to the source of the first
//!   started subject and unbinds when the last one stops.
//! * A subject is advanced at most once per frame: frames whose sequence
//!   number is not newer than the subject's last frame leave it unchanged
//!   (stale or repeated frames are dropped).
//! * Reported boxes are the visible part of the subject, inside the frame.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ac_core::{Point, Rect, SourceId, SubjectId};
use ac_media::VideoFrame;
use rayon::prelude::*;

use crate::api::{TrackObservation, Tracker};
use crate::boxes::clamp_box;
use crate::error::VisionError;
use crate::image::GrayImage;
use crate::ncc::{Level, Pyramid, ScoreMap, Template};

mod config;
#[cfg(test)]
mod tests;

pub use config::TemplateTrackerConfig;

/// Shorter side, in pixels, below which no further pyramid level is built.
const MIN_LEVEL_SIDE: usize = 8;
/// Most pyramid levels built (level 0 included).
const MAX_LEVELS: usize = 6;
/// Smallest template side, in pixels, used for matching.
const MIN_TEMPLATE_SIDE: usize = 3;
/// Largest widening of the search window after skipped frames.
const MAX_WINDOW_GROWTH: f64 = 3.0;
/// Radius, in pixels, of the grid of centres probed at each scale.
const SCALE_PROBE_RADIUS: usize = 1;

/// Per-subject tracking state.
#[derive(Debug, Clone)]
struct Track {
    /// Box centre, normalized; may sit on the frame border.
    center: Point,
    /// Box width and height, normalized; the box may extend past the border.
    size: (f64, f64),
    /// Centre velocity in normalized units per second.
    velocity: (f64, f64),
    /// Appearance at the canonical template resolution, values in `0..=1`.
    appearance: GrayImage,
    /// Confidence of the latest estimate.
    confidence: f32,
    /// Sequence number of the last frame applied.
    seq: u64,
    /// Presentation time of the last frame applied.
    pts: Duration,
}

impl Track {
    /// The visible part of the box, inside the frame.
    fn visible_box(&self) -> Rect {
        let full = Rect::from_center(self.center, self.size.0, self.size.1);
        clamp_box(full).unwrap_or(Rect::new(
            self.center.x.min(1.0 - 1e-6),
            self.center.y.min(1.0 - 1e-6),
            1e-6,
            1e-6,
        ))
    }
}

/// Result of matching one subject on one frame.
#[derive(Debug, Clone, Copy)]
struct Match {
    /// Matched centre in pixels of the matching level.
    cx: f64,
    /// Matched centre in pixels of the matching level.
    cy: f64,
    /// Scale relative to the previous box size.
    scale: f64,
    /// Peak NCC.
    score: f32,
    /// A separate peak came within the ambiguity margin.
    ambiguous: bool,
    /// Contrast gain of the image window at the match over the template.
    gain: f64,
}

impl Match {
    /// True when the contrast check holds the box back: the window's
    /// contrast is off the template's by more than `max_contrast_gain`
    /// either way and the score is below `update_score`.
    fn contrast_refused(&self, config: &TemplateTrackerConfig) -> bool {
        let g = config.max_contrast_gain;
        self.score < config.update_score && !(1.0 / g..=g).contains(&self.gain)
    }

    /// True when the match moves the box: a peak of at least `accept_score`,
    /// unambiguous and not refused by the contrast check.
    fn trusted(&self, config: &TemplateTrackerConfig) -> bool {
        self.score >= config.accept_score && !self.ambiguous && !self.contrast_refused(config)
    }
}

/// The pyramid of one frame, kept for reuse on that frame.
#[derive(Debug, Clone)]
struct CachedPyramid {
    /// The frame it was built from: source, sequence number and PTS.
    key: (SourceId, u64, Duration),
    /// The pyramid.
    pyramid: Arc<Pyramid>,
}

/// Per-frame NCC template tracker; see the [module docs](self).
#[derive(Debug, Clone, Default)]
pub struct TemplateTracker {
    /// Tuning.
    config: TemplateTrackerConfig,
    /// Source the tracker is bound to while it has subjects.
    source: Option<SourceId>,
    /// Subjects by id.
    tracks: BTreeMap<SubjectId, Track>,
    /// Pyramid of the newest frame converted.
    cache: Option<CachedPyramid>,
    /// Pyramids built so far (one per distinct frame worked on).
    pyramids_built: u64,
}

impl TemplateTracker {
    /// A tracker with `config`, which is validated.
    pub fn new(config: TemplateTrackerConfig) -> Result<Self, VisionError> {
        config.validate()?;
        Ok(Self {
            config,
            ..Self::default()
        })
    }

    /// The tracker's tuning.
    pub fn config(&self) -> &TemplateTrackerConfig {
        &self.config
    }

    /// The source the tracker is bound to, if it has subjects.
    pub fn source(&self) -> Option<&SourceId> {
        self.source.as_ref()
    }

    /// Current estimates without advancing to a new frame, ordered by id.
    pub fn observations(&self) -> Vec<TrackObservation> {
        self.tracks
            .iter()
            .map(|(&id, t)| TrackObservation {
                id,
                bbox: t.visible_box(),
                confidence: t.confidence,
            })
            .collect()
    }

    /// Pyramids built so far: one per distinct frame the tracker started
    /// or updated subjects on, however many subjects that involved.
    pub fn pyramids_built(&self) -> u64 {
        self.pyramids_built
    }

    /// The working-resolution pyramid of `frame`, built once per frame.
    fn pyramid(&mut self, frame: &VideoFrame) -> Arc<Pyramid> {
        let key = (frame.source.clone(), frame.seq, frame.pts);
        if let Some(c) = &self.cache
            && c.key == key
        {
            return c.pyramid.clone();
        }
        let w = frame.width.min(self.config.work_width) as usize;
        let h = ((frame.height as f64 * w as f64 / frame.width as f64).round() as usize).max(1);
        let pyramid = Arc::new(Pyramid::new(
            GrayImage::from_frame(frame, w, h),
            MIN_LEVEL_SIDE,
            MAX_LEVELS,
        ));
        self.pyramids_built += 1;
        self.cache = Some(CachedPyramid {
            key,
            pyramid: pyramid.clone(),
        });
        pyramid
    }

    /// Fails when `frame` belongs to another source than the bound one.
    fn check_source(&self, frame: &VideoFrame) -> Result<(), VisionError> {
        match &self.source {
            Some(expected) if *expected != frame.source => Err(VisionError::SourceMismatch {
                expected: expected.clone(),
                got: frame.source.clone(),
            }),
            _ => Ok(()),
        }
    }
}

/// Index of the finest level on which a box of normalized `size` has a longer
/// side of at most `limit` pixels, or the coarsest level.
fn level_for(pyramid: &Pyramid, size: (f64, f64), limit: f64) -> usize {
    let fits = |l: &Level| (size.0 * l.img.width as f64).max(size.1 * l.img.height as f64) <= limit;
    pyramid
        .levels
        .iter()
        .position(fits)
        .unwrap_or(pyramid.levels.len() - 1)
}

/// Canonical template size for a box of `bw x bh` working pixels.
fn canonical_size(bw: f64, bh: f64, side: usize) -> (usize, usize) {
    let short = |a: f64, b: f64| ((side as f64 * a / b).round() as usize).clamp(4, side);
    if bw >= bh {
        (side, short(bh, bw))
    } else {
        (short(bw, bh), side)
    }
}

/// Time step between two presentation timestamps, in seconds.
fn interval(config: &TemplateTrackerConfig, from: Duration, to: Duration) -> f64 {
    if to > from {
        (to - from).as_secs_f64().min(config.max_dt)
    } else {
        config.nominal_dt
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
fn match_on_level(
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

/// Advances `track` to the frame described by `pyramid`, `seq` and `pts`.
fn step(
    config: &TemplateTrackerConfig,
    track: &mut Track,
    pyramid: &Pyramid,
    seq: u64,
    pts: Duration,
) {
    let dt = interval(config, track.pts, pts);
    track.seq = seq;
    track.pts = pts;
    let predicted = Point {
        x: (track.center.x + track.velocity.0 * dt).clamp(0.0, 1.0),
        y: (track.center.y + track.velocity.1 * dt).clamp(0.0, 1.0),
    };
    let level = &pyramid.levels[level_for(pyramid, track.size, config.template_size as f64)];
    let (lw, lh) = (level.img.width as f64, level.img.height as f64);
    let (bw, bh) = (track.size.0 * lw, track.size.1 * lh);
    let found = match_on_level(
        config,
        level,
        &track.appearance,
        (predicted.x * lw, predicted.y * lh),
        (bw, bh),
        dt,
    );
    match found {
        Some(m) if m.trusted(config) => {
            let center = Point {
                x: (m.cx / lw).clamp(0.0, 1.0),
                y: (m.cy / lh).clamp(0.0, 1.0),
            };
            let a = config.velocity_smoothing;
            track.velocity = (
                (1.0 - a) * track.velocity.0 + a * (center.x - track.center.x) / dt,
                (1.0 - a) * track.velocity.1 + a * (center.y - track.center.y) / dt,
            );
            track.center = center;
            let base = &pyramid.levels[0].img;
            let (min_w, min_h) = (
                config.min_box_px / base.width as f64,
                config.min_box_px / base.height as f64,
            );
            track.size = (
                (track.size.0 * m.scale).clamp(min_w.min(1.0), 1.0),
                (track.size.1 * m.scale).clamp(min_h.min(1.0), 1.0),
            );
            track.confidence = m.score.clamp(0.0, 1.0);
            let full = Rect::from_center(center, track.size.0, track.size.1);
            if m.score >= config.update_score && Rect::FULL.contains_rect(&full) {
                learn(config, track, level);
            }
        }
        other => {
            track.center = predicted;
            track.velocity = (
                track.velocity.0 * config.velocity_decay,
                track.velocity.1 * config.velocity_decay,
            );
            track.confidence = match other {
                // Held back by its ambiguity or its contrast, not its score.
                Some(m) if m.ambiguous || m.score >= config.accept_score => {
                    m.score.min(config.ambiguous_confidence)
                }
                Some(m) => m.score,
                None => 0.0,
            }
            .clamp(0.0, 1.0);
        }
    }
}

/// Blends the current appearance of `track`'s box on `level` into its template.
fn learn(config: &TemplateTrackerConfig, track: &mut Track, level: &Level) {
    let (lw, lh) = (level.img.width as f64, level.img.height as f64);
    let (cw, ch) = (track.appearance.width, track.appearance.height);
    let fresh = level.img.sample_patch(
        track.center.x * lw,
        track.center.y * lh,
        track.size.0 * lw,
        track.size.1 * lh,
        cw,
        ch,
    );
    let r = config.learning_rate;
    track
        .appearance
        .data
        .iter_mut()
        .zip(fresh)
        .for_each(|(a, f)| *a = (1.0 - r) * *a + r * f);
}

impl Tracker for TemplateTracker {
    fn start(&mut self, frame: &VideoFrame, id: SubjectId, bbox: Rect) -> Result<(), VisionError> {
        self.check_source(frame)?;
        let clipped = clamp_box(bbox).ok_or(VisionError::InvalidBox {
            bbox,
            reason: "empty, non-finite or outside the frame",
        })?;
        let pyramid = self.pyramid(frame);
        let base = &pyramid.levels[0].img;
        let (bw, bh) = (
            clipped.w * base.width as f64,
            clipped.h * base.height as f64,
        );
        if bw.min(bh) < self.config.min_box_px {
            return Err(VisionError::InvalidBox {
                bbox,
                reason: "smaller than min_box_px at the working resolution",
            });
        }
        let (cw, ch) = canonical_size(bw, bh, self.config.template_size as usize);
        let size = (clipped.w, clipped.h);
        let level = &pyramid.levels[level_for(&pyramid, size, self.config.template_size as f64)];
        let (lw, lh) = (level.img.width as f64, level.img.height as f64);
        let center = clipped.center();
        let data = level.img.sample_patch(
            center.x * lw,
            center.y * lh,
            size.0 * lw,
            size.1 * lh,
            cw,
            ch,
        );
        if Template::new(cw, ch, &data).is_none() {
            return Err(VisionError::Untrackable(
                "the box has no texture to match".into(),
            ));
        }
        self.tracks.insert(
            id,
            Track {
                center,
                size,
                velocity: (0.0, 0.0),
                appearance: GrayImage::new(cw, ch, data),
                confidence: 1.0,
                seq: frame.seq,
                pts: frame.pts,
            },
        );
        self.source = Some(frame.source.clone());
        Ok(())
    }

    fn update(&mut self, frame: &VideoFrame) -> Vec<TrackObservation> {
        if self.source.as_ref() != Some(&frame.source) {
            return Vec::new();
        }
        if self.tracks.values().any(|t| frame.seq > t.seq) {
            let pyramid = self.pyramid(frame);
            let config = &self.config;
            self.tracks
                .par_iter_mut()
                .filter(|(_, t)| frame.seq > t.seq)
                .for_each(|(_, t)| step(config, t, &pyramid, frame.seq, frame.pts));
        }
        self.observations()
    }

    fn stop(&mut self, id: SubjectId) {
        self.tracks.remove(&id);
        if self.tracks.is_empty() {
            self.source = None;
            self.cache = None;
        }
    }

    fn tracked(&self) -> Vec<SubjectId> {
        self.tracks.keys().copied().collect()
    }
}
