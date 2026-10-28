//! Shot candidates, the scoring function and the jump-cut rule.
//!
//! Each tick the engine describes every shot it could put on air as a
//! [`ShotCandidate`]: its source, whether it is a wide or a subject shot, its
//! crop in normalized source coordinates, a `quality` in `0.0..=1.0`
//! (tracking confidence multiplied by framing settledness, computed by the
//! engine) and a `priority`, how many levels the operator ranked the prompt
//! of the shot's subject above the lowest-ranked one among the subjects on
//! offer. The director turns that into one number per shot with [`score`]:
//!
//! ```text
//! score(c) = k(c) · q(c) + u · p(c) − v · r(c)
//!
//!   q(c) = quality clamped to 0..=1 (NaN counts as 0)
//!   k(c) = ScoreWeights::wide_weight for a wide, 1 for a subject shot
//!   p(c) = ShotCandidate::priority: levels above the lowest priority on
//!          offer, 0 for a wide and for every shot when all share one
//!   r(c) = clamp(1 − (now − t_used) / W, 0, 1) when the shot was last on
//!          program at t_used, else 0                       (recency, 0..=1)
//!   u    = ScoreWeights::priority_weight
//!   v    = ScoreWeights::variety_penalty, W = ScoreWeights::variety_window
//! ```
//!
//! *Why a wide weight.* A wide has no tracking to lose, so the engine reports
//! it at quality ≈ 1 and it would win every comparison. Weighting it below 1
//! keeps it as the safety shot (fallback, establishing, rhythm breaks forced by
//! maximum dwell) without letting it crowd out well-tracked singles.
//!
//! *Why a recency penalty.* Professional rules ask for shots to change often
//! but not predictably. Penalising a shot that left program recently, with a
//! penalty that decays linearly to zero over `W`, makes the director rotate
//! among otherwise equal shots instead of bouncing between the top two.
//!
//! *Why a priority term.* Quality says how well a shot is framed, not whether
//! it shows the right person: a well-tracked drummer and a well-tracked
//! pastor look the same to it. The operator ranks prompts ("role priority"),
//! and each level adds a fixed `u` to the shot, so with equal quality the
//! higher-priority prompt's shot wins. The term counts levels above the
//! lowest priority in use rather than the configured number, so that only
//! differences between prompts matter: with one priority everywhere, every
//! `p` is 0 and every score is exactly what it is without the term, and a
//! wide, which shows no prompt in particular, stands with the lowest. At the
//! default `u` = 0.05, two levels equal the director's default margin δ
//! (0.1), enough to displace an equal-quality incumbent, while the widest
//! spread the engine allows (four levels, 0.2) stays below the variety
//! penalty (0.3): priority tilts the rotation towards the important
//! subjects without ending it. A shot one level up is worth the same as one
//! tracked 0.05 better (an exact tie goes to the earlier-listed candidate,
//! and f32 rounding may tip it either way), and loses to one tracked more
//! than 0.05 better.
//!
//! Scores therefore lie in `−v..=max(1, k) + u · p_max`. The program shot is
//! compared with [`base_score`], which leaves out the recency term: being on
//! air is not "recent use" that should count against the incumbent. It keeps
//! the priority term, so an important shot on program is as hard to replace
//! as it was to choose.
//!
//! The jump-cut rule ([`is_jump_cut`]) forbids cutting between two shots of
//! the same source whose crops overlap with IoU above a threshold (0.4 by
//! default): the viewer would see the picture lurch rather than a new shot.

use std::time::Duration;

use ac_core::{Rect, ShotId, ShotTarget, SourceId};
use serde::{Deserialize, Serialize};

/// Whether a candidate shows a whole source or frames a subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// The whole picture of a source: the safety and establishing shot.
    Wide,
    /// A tracked subject framed by a preset.
    Subject,
}

impl From<&ShotTarget> for CandidateKind {
    fn from(target: &ShotTarget) -> Self {
        match target {
            ShotTarget::Wide { .. } => CandidateKind::Wide,
            ShotTarget::Subject { .. } => CandidateKind::Subject,
        }
    }
}

/// One shot the director may suggest, as seen on one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotCandidate {
    /// Shot identity, shared with the switcher.
    pub id: ShotId,
    /// Source the shot crops from.
    pub source: SourceId,
    /// Wide or subject shot.
    pub kind: CandidateKind,
    /// Current crop in normalized source coordinates.
    pub crop: Rect,
    /// Tracking confidence × framing settledness in `0.0..=1.0`, from the
    /// engine. Out-of-range values are clamped and NaN counts as `0.0`.
    pub quality: f32,
    /// False when the shot cannot be shown (source down, subject retired).
    pub available: bool,
    /// When the shot was last on program (media clock), if ever.
    pub recently_used_at: Option<Duration>,
    /// Priority levels the prompt of the shot's subject stands above the
    /// lowest priority among the subjects on offer (`p` in the [module
    /// docs](self)): 0 for a wide, and for every shot when all share one.
    pub priority: u8,
}

impl ShotCandidate {
    /// Quality clamped to `0.0..=1.0`, with NaN mapped to `0.0`.
    pub fn quality(&self) -> f32 {
        if self.quality.is_nan() {
            0.0
        } else {
            self.quality.clamp(0.0, 1.0)
        }
    }

    /// True when the crop is finite and has a positive size.
    ///
    /// A candidate with an invalid crop cannot be rendered, so the director
    /// never suggests it and never uses it for the jump-cut test.
    pub fn has_valid_crop(&self) -> bool {
        let r = &self.crop;
        [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite()) && r.w > 0.0 && r.h > 0.0
    }
}

/// Weights of the scoring function (see the [module docs](self)).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoreWeights {
    /// Multiplier `k` applied to a wide's quality; subject shots use 1.
    pub wide_weight: f32,
    /// Bonus `u` per priority level of a shot's prompt.
    pub priority_weight: f32,
    /// Penalty `v` for a shot that has just left program.
    pub variety_penalty: f32,
    /// Window `W` over which the recency penalty decays to zero. A zero
    /// window disables the penalty.
    pub variety_window: Duration,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            wide_weight: 0.75,
            priority_weight: 0.05,
            variety_penalty: 0.3,
            variety_window: Duration::from_secs(60),
        }
    }
}

/// Recency `r` in `0.0..=1.0`: 1 for a shot that has just left program,
/// decaying linearly to 0 after `window`. A shot never used scores 0.
///
/// A `used_at` later than `now` (a clock glitch) counts as just used.
pub fn recency(used_at: Option<Duration>, now: Duration, window: Duration) -> f32 {
    let Some(used_at) = used_at else {
        return 0.0;
    };
    if window.is_zero() {
        return 0.0;
    }
    let age = now.saturating_sub(used_at);
    (1.0 - age.as_secs_f32() / window.as_secs_f32()).clamp(0.0, 1.0)
}

/// `k(c) · q(c) + u · p(c)`: the score without the recency term, used for
/// program.
///
/// With `p(c)` = 0 the priority term adds exactly zero, so the result is
/// `k(c) · q(c)` to the bit.
pub fn base_score(candidate: &ShotCandidate, weights: &ScoreWeights) -> f32 {
    let k = match candidate.kind {
        CandidateKind::Wide => weights.wide_weight,
        CandidateKind::Subject => 1.0,
    };
    k * candidate.quality() + weights.priority_weight * f32::from(candidate.priority)
}

/// `k(c) · q(c) + u · p(c) − v · r(c)`: how much the director wants
/// `candidate` next.
pub fn score(candidate: &ShotCandidate, now: Duration, weights: &ScoreWeights) -> f32 {
    base_score(candidate, weights)
        - weights.variety_penalty * recency(candidate.recently_used_at, now, weights.variety_window)
}

/// True when cutting from `from` to `to` would be a jump cut: both shots crop
/// the same source and their crops overlap with IoU above `max_iou`.
///
/// Shots with an invalid crop are never judged jump cuts; they are filtered
/// out elsewhere.
pub fn is_jump_cut(from: &ShotCandidate, to: &ShotCandidate, max_iou: f64) -> bool {
    from.source == to.source
        && from.has_valid_crop()
        && to.has_valid_crop()
        && from.crop.iou(&to.crop) > max_iou
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::SubjectId;

    fn cand(kind: CandidateKind, source: &str, crop: Rect, quality: f32) -> ShotCandidate {
        ShotCandidate {
            id: ShotId("s".into()),
            source: SourceId(source.into()),
            kind,
            crop,
            quality,
            available: true,
            recently_used_at: None,
            priority: 0,
        }
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn score_follows_documented_formula() {
        let w = ScoreWeights::default();
        let mut c = cand(CandidateKind::Subject, "cam", Rect::FULL, 0.8);
        assert!((score(&c, secs(100.0), &w) - 0.8).abs() < 1e-6);
        // Left program 15 s ago with a 60 s window: r = 0.75, penalty 0.225.
        c.recently_used_at = Some(secs(85.0));
        assert!((score(&c, secs(100.0), &w) - (0.8 - 0.3 * 0.75)).abs() < 1e-6);
        let wide = cand(CandidateKind::Wide, "cam", Rect::FULL, 1.0);
        assert!((score(&wide, secs(0.0), &w) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn base_score_ignores_recency() {
        let w = ScoreWeights::default();
        let mut c = cand(CandidateKind::Subject, "cam", Rect::FULL, 0.6);
        c.recently_used_at = Some(secs(10.0));
        assert!((base_score(&c, &w) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn each_priority_level_adds_the_priority_weight() {
        let w = ScoreWeights::default();
        let mut c = cand(CandidateKind::Subject, "cam", Rect::FULL, 0.8);
        c.priority = 2;
        assert!((score(&c, secs(100.0), &w) - (0.8 + 2.0 * 0.05)).abs() < 1e-6);
        // The program comparison keeps the priority, only recency goes.
        c.recently_used_at = Some(secs(85.0));
        assert!((base_score(&c, &w) - 0.9).abs() < 1e-6);
        assert!((score(&c, secs(100.0), &w) - (0.9 - 0.3 * 0.75)).abs() < 1e-6);
        // A level is worth the same on a wide, after its weight.
        let mut wide = cand(CandidateKind::Wide, "cam", Rect::FULL, 1.0);
        wide.priority = 1;
        assert!((score(&wide, secs(0.0), &w) - 0.8).abs() < 1e-6);
        // The largest level stays finite, and a zero weight ignores levels.
        c.priority = u8::MAX;
        assert!(score(&c, secs(100.0), &w).is_finite());
        let off = ScoreWeights {
            priority_weight: 0.0,
            ..w
        };
        assert!((base_score(&c, &off) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn one_level_is_worth_0_05_of_quality() {
        let w = ScoreWeights::default();
        for quality in [0.5, 0.6, 0.7, 0.8, 0.85] {
            let mut up = cand(CandidateKind::Subject, "cam", Rect::FULL, quality);
            up.priority = 1;
            let better = |by: f32| cand(CandidateKind::Subject, "cam", Rect::FULL, quality + by);
            let now = secs(0.0);
            // Tracked 0.05 better: the same score, up to f32 rounding.
            assert!((score(&up, now, &w) - score(&better(0.05), now, &w)).abs() < 1e-6);
            // Tracked more than 0.05 better: the level loses.
            assert!(score(&up, now, &w) < score(&better(0.06), now, &w));
        }
    }

    #[test]
    fn zero_priority_scores_exactly_as_without_the_term() {
        let w = ScoreWeights::default();
        for kind in [CandidateKind::Wide, CandidateKind::Subject] {
            let k = if kind == CandidateKind::Wide {
                w.wide_weight
            } else {
                1.0
            };
            for quality in [0.0, 0.3, 0.55, 0.8, 0.95, 1.0, 1.4, -0.1, f32::NAN] {
                for used in [None, Some(secs(0.0)), Some(secs(42.5)), Some(secs(99.0))] {
                    let mut c = cand(kind, "cam", Rect::FULL, quality);
                    c.recently_used_at = used;
                    let now = secs(100.0);
                    // The formula before priorities: k · q − v · r.
                    let base = k * c.quality();
                    let old = base - w.variety_penalty * recency(used, now, w.variety_window);
                    assert_eq!(base_score(&c, &w).to_bits(), base.to_bits());
                    assert_eq!(score(&c, now, &w).to_bits(), old.to_bits());
                }
            }
        }
    }

    #[test]
    fn recency_decays_linearly_and_handles_edges() {
        let window = secs(60.0);
        assert_eq!(recency(None, secs(5.0), window), 0.0);
        assert!((recency(Some(secs(0.0)), secs(0.0), window) - 1.0).abs() < 1e-6);
        assert!((recency(Some(secs(0.0)), secs(30.0), window) - 0.5).abs() < 1e-6);
        assert_eq!(recency(Some(secs(0.0)), secs(120.0), window), 0.0);
        // A timestamp from the future counts as just used.
        assert!((recency(Some(secs(10.0)), secs(5.0), window) - 1.0).abs() < 1e-6);
        // A zero window disables the penalty.
        assert_eq!(recency(Some(secs(1.0)), secs(1.0), Duration::ZERO), 0.0);
    }

    #[test]
    fn quality_is_sanitised() {
        let mut c = cand(CandidateKind::Subject, "cam", Rect::FULL, f32::NAN);
        assert_eq!(c.quality(), 0.0);
        c.quality = 1.7;
        assert_eq!(c.quality(), 1.0);
        c.quality = -0.2;
        assert_eq!(c.quality(), 0.0);
    }

    #[test]
    fn crop_validity() {
        let mut c = cand(
            CandidateKind::Subject,
            "cam",
            Rect::new(0.1, 0.1, 0.2, 0.2),
            1.0,
        );
        assert!(c.has_valid_crop());
        c.crop.w = 0.0;
        assert!(!c.has_valid_crop());
        c.crop = Rect::new(f64::NAN, 0.0, 0.2, 0.2);
        assert!(!c.has_valid_crop());
    }

    #[test]
    fn jump_cut_needs_same_source_and_high_overlap() {
        let medium = cand(
            CandidateKind::Subject,
            "cam",
            Rect::new(0.3, 0.2, 0.4, 0.4),
            1.0,
        );
        // Tighter crop inside the medium: IoU = 0.09 / 0.16 ≈ 0.56.
        let close = cand(
            CandidateKind::Subject,
            "cam",
            Rect::new(0.35, 0.25, 0.3, 0.3),
            1.0,
        );
        assert!(is_jump_cut(&medium, &close, 0.4));
        assert!(!is_jump_cut(&medium, &close, 0.6));
        let other_source = ShotCandidate {
            source: SourceId("cam2".into()),
            ..close.clone()
        };
        assert!(!is_jump_cut(&medium, &other_source, 0.4));
        // The wide against a medium crop overlaps by 0.16 only.
        let wide = cand(CandidateKind::Wide, "cam", Rect::FULL, 1.0);
        assert!(!is_jump_cut(&medium, &wide, 0.4));
    }

    #[test]
    fn kind_follows_shot_target() {
        let wide = ShotTarget::Wide {
            source: SourceId("cam".into()),
        };
        let subject = ShotTarget::Subject {
            source: SourceId("cam".into()),
            subject: SubjectId(1),
            preset: "medium".into(),
        };
        assert_eq!(CandidateKind::from(&wide), CandidateKind::Wide);
        assert_eq!(CandidateKind::from(&subject), CandidateKind::Subject);
    }
}
