//! Framing quality: where the on-air crop puts the person it frames
//! (spec V9 crop containment and framing error, C.3 framing quality, C.4
//! headroom and cut lines).
//!
//! Each sample is one on-air subject shot of the scoped source on one
//! scored keyframe, with the labelled person it frames (see the scorers'
//! module docs); a shot framing nobody labelled there is counted in the
//! details as `unlabelled` and scored by nothing here. The labels give a
//! person's box and, where labelled, the heights of the crown, chin, tops
//! of the shoulders and waist ([`BodyLines`](crate::labels::BodyLines));
//! all heights below are normalized source `y`, growing downward.
//!
//! # Metrics
//!
//! * `framing.head_torso_inside`: share of samples whose crop keeps the
//!   head and the torso ([`keeps_head_and_torso`]). The head is the band
//!   from crown to chin, which must lie between the crop's top and bottom
//!   edges, and the box's centre line, which must lie between its sides:
//!   labels give no head width, so a head cut by a side edge is caught only
//!   once its centre leaves. The torso is the shoulder line for every size
//!   wider than a close-up (whose cut is the shoulders), and the waist too
//!   in a full shot (whose cut is below it). Samples missing a line they
//!   need are not counted.
//! * `framing.headroom.<size>`: share of samples of that size whose
//!   headroom, crown to top edge over the crop height ([`headroom`]), lies
//!   in the size's band ([`FramingLimits`]), ends included. A crown above
//!   the crop is negative headroom and always out of band.
//! * `framing.lower_cut_error`: the largest distance between the crop's
//!   bottom edge and its size's cut line, in head heights (chin minus
//!   crown), from the labelled line a size cuts at
//!   ([`lower_cut_error`]): the shoulders for a close-up, the waist for a
//!   medium shot, the feet (the box's bottom, when not truncated there) for
//!   a full shot. A medium close-up cuts at the bottom of the sternum,
//!   which is not labelled, so it is not measured.
//! * `framing.subject_containment`: share of samples whose crop holds the
//!   person's box down to its own bottom edge ([`cut_at_frame_bottom`],
//!   then [`crate::containment`]): what the shot shows of the person is not
//!   clipped at the top or the sides. The box below the bottom edge is cut
//!   by the shot's design.
//! * `framing.joint_clearance`: how far the bottom edge stays from a joint
//!   (C.4: no cuts at joints). The labels have no joint keypoints (hips,
//!   knees, elbows; the shoulder and waist lines are cut lines, not
//!   joints), so it is never measurable yet.
//!
//! # Invariants
//!
//! * A share is over at least one sample and lies in `0..=1`, and its
//!   `samples` is its denominator; over no sample it is not measurable,
//!   never 0 or NaN.
//! * The metrics come in a fixed order: head and torso, the four headroom
//!   bands widest first, the lower cut, containment, joint clearance.

use ac_core::{Rect, ShotSize};

use super::join::Join;
use crate::labels::{PersonBox, validate::SLACK};
use crate::report::{Limit, Measure, Metric};
use crate::thresholds::{Band, FramingLimits};

/// The subject shot sizes with a headroom band, tightest last, with their
/// metric names.
const BANDED: [(ShotSize, &str); 4] = [
    (ShotSize::Full, "framing.headroom.full"),
    (ShotSize::Medium, "framing.headroom.medium"),
    (ShotSize::MediumClose, "framing.headroom.medium_close"),
    (ShotSize::Close, "framing.headroom.close"),
];

/// One on-air subject shot on one scored keyframe, with the person it
/// frames.
#[derive(Debug, Clone, Copy)]
pub struct Sample<'a> {
    /// The shot's size, when known.
    pub size: Option<ShotSize>,
    /// The crop on air.
    pub crop: Rect,
    /// The labelled person it frames.
    pub person: &'a PersonBox,
}

/// Headroom of a crown at height `crown` in `crop`: the gap from the top
/// edge down to the crown over the crop's height; `None` for a crop with
/// no height.
pub fn headroom(crop: &Rect, crown: f64) -> Option<f64> {
    (crop.h > 0.0 && crop.h.is_finite()).then(|| (crown - crop.y) / crop.h)
}

/// The headroom band of `size` in `limits`; `None` for a wide.
pub fn band(limits: &FramingLimits, size: ShotSize) -> Option<Band> {
    match size {
        ShotSize::Wide => None,
        ShotSize::Full => Some(limits.headroom_full),
        ShotSize::Medium => Some(limits.headroom_medium),
        ShotSize::MediumClose => Some(limits.headroom_medium_close),
        ShotSize::Close => Some(limits.headroom_close),
    }
}

/// Whether `value` lies in `[low, high]`, allowing [`SLACK`] for rounding.
pub fn in_band(value: f64, [low, high]: Band) -> bool {
    value >= low - SLACK && value <= high + SLACK
}

/// Whether height `y` lies between `crop`'s top and bottom edges.
fn inside_vertically(crop: &Rect, y: f64) -> bool {
    y >= crop.y - SLACK && y <= crop.bottom() + SLACK
}

/// Whether `crop` keeps `person`'s head and torso (see the module docs);
/// `None` when a line the check needs is not labelled. An unknown size is
/// checked for the head alone.
pub fn keeps_head_and_torso(
    crop: &Rect,
    person: &PersonBox,
    size: Option<ShotSize>,
) -> Option<bool> {
    let lines = &person.lines;
    let mut needed = vec![lines.crown?, lines.chin?];
    match size {
        Some(ShotSize::MediumClose | ShotSize::Medium) => needed.push(lines.shoulder?),
        Some(ShotSize::Full | ShotSize::Wide) => {
            needed.push(lines.shoulder?);
            needed.push(lines.waist?);
        }
        Some(ShotSize::Close) | None => {}
    }
    let centre = person.bbox.center().x;
    let sides = centre >= crop.x - SLACK && centre <= crop.right() + SLACK;
    Some(sides && needed.iter().all(|&y| inside_vertically(crop, y)))
}

/// How far `crop`'s bottom edge lies below the line a shot of `size` cuts
/// `person` at, in head heights (negative above it); `None` when the line
/// or the head is not labelled, the head has no height, or the size has no
/// labelled cut line (a medium close-up, a wide).
pub fn lower_cut_error(crop: &Rect, person: &PersonBox, size: ShotSize) -> Option<f64> {
    let lines = &person.lines;
    let head = lines.chin? - lines.crown?;
    if head <= 0.0 {
        return None;
    }
    let cut = match size {
        ShotSize::Close => lines.shoulder?,
        ShotSize::Medium => lines.waist?,
        ShotSize::Full if !person.truncation.bottom => person.bbox.bottom(),
        ShotSize::Full | ShotSize::MediumClose | ShotSize::Wide => return None,
    };
    Some((crop.bottom() - cut) / head)
}

/// `person`'s box cut at `crop`'s bottom edge: the part a shot means to
/// show. A box wholly below the edge keeps no height.
pub fn cut_at_frame_bottom(person: &Rect, crop: &Rect) -> Rect {
    let bottom = person.bottom().min(crop.bottom());
    Rect::new(person.x, person.y, person.w, (bottom - person.y).max(0.0))
}

/// Every sample of the run, in keyframe order, and the number of on-air
/// subject shots that framed nobody labelled.
fn samples<'a>(join: &Join<'a>) -> (Vec<Sample<'a>>, u64) {
    let mut samples = Vec::new();
    let mut unlabelled = 0;
    for k in &join.keyframes {
        for (shot, person) in join.on_air_subject_shots(k) {
            match person {
                Some(person) => samples.push(Sample {
                    size: join.scope.shot_size(shot),
                    crop: shot.crop,
                    person,
                }),
                None => unlabelled += 1,
            }
        }
    }
    (samples, unlabelled)
}

/// The share of `outcomes` that are true, or why there are none.
fn share(outcomes: &[bool], none: &str) -> (Measure, u64) {
    let n = outcomes.len() as u64;
    if n == 0 {
        return (Measure::NotMeasurable(none.to_owned()), 0);
    }
    let yes = outcomes.iter().filter(|&&o| o).count();
    (Measure::Value(yes as f64 / n as f64), n)
}

/// Scores framing over `join` into `out`; see the module docs.
pub(crate) fn score(join: &Join, limits: &FramingLimits, out: &mut Vec<Metric>) {
    let (samples, unlabelled) = samples(join);
    let kept: Vec<bool> = samples
        .iter()
        .filter_map(|s| keeps_head_and_torso(&s.crop, s.person, s.size))
        .collect();
    let (measure, n) = share(
        &kept,
        "no on-air subject shot framed a person with the crown, chin and torso lines it needs labelled",
    );
    out.push(
        Metric::new(
            "framing.head_torso_inside",
            "fraction of frames",
            measure,
            n,
            Limit::min(limits.min_head_torso_inside),
        )
        .detail("unlabelled", unlabelled as f64),
    );

    for (size, name) in BANDED {
        let band = band(limits, size).expect("a subject size has a band");
        let headrooms: Vec<f64> = samples
            .iter()
            .filter(|s| s.size == Some(size))
            .filter_map(|s| headroom(&s.crop, s.person.lines.crown?))
            .collect();
        let outcomes: Vec<bool> = headrooms.iter().map(|&h| in_band(h, band)).collect();
        let (measure, n) = share(
            &outcomes,
            "no on-air shot of this size framed a person with the crown labelled",
        );
        let lowest = headrooms.iter().copied().fold(f64::INFINITY, f64::min);
        let highest = headrooms.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        out.push(
            Metric::new(
                name,
                "fraction of frames",
                measure,
                n,
                Limit::min(limits.min_headroom_in_band),
            )
            .detail("band_low", band[0])
            .detail("band_high", band[1])
            .detail("lowest", lowest)
            .detail("highest", highest),
        );
    }

    let errors: Vec<f64> = samples
        .iter()
        .filter_map(|s| lower_cut_error(&s.crop, s.person, s.size?))
        .collect();
    let measure = if errors.is_empty() {
        Measure::NotMeasurable(
            "no on-air close-up, medium or full shot framed a person with the crown, chin and \
             cut line labelled (a medium close-up's cut, the bottom of the sternum, is not \
             labelled)"
                .into(),
        )
    } else {
        Measure::Value(errors.iter().map(|e| e.abs()).fold(0.0, f64::max))
    };
    // NaN with no errors, which the detail leaves out.
    let mean = errors.iter().fold(0.0, |a, e| a + e) / errors.len() as f64;
    out.push(
        Metric::new(
            "framing.lower_cut_error",
            "head heights",
            measure,
            errors.len() as u64,
            Limit::max(limits.max_lower_cut_error),
        )
        .detail("mean_signed", mean),
    );

    let crops: Vec<Rect> = samples.iter().map(|s| s.crop).collect();
    let cut: Vec<Rect> = samples
        .iter()
        .map(|s| cut_at_frame_bottom(&s.person.bbox, &s.crop))
        .collect();
    let measure = match crate::containment(&crops, &cut) {
        Some(share) => Measure::Value(share),
        None => Measure::NotMeasurable("no on-air subject shot framed a labelled person".into()),
    };
    out.push(Metric::new(
        "framing.subject_containment",
        "fraction of frames",
        measure,
        samples.len() as u64,
        Limit::min(limits.min_subject_containment),
    ));

    out.push(Metric::new(
        "framing.joint_clearance",
        "fraction of frame height",
        Measure::NotMeasurable(
            "the labels carry no joint keypoints (hips, knees, elbows); the shoulder and waist \
             lines are cut lines, not joints"
                .into(),
        ),
        0,
        Limit::min(limits.min_joint_clearance),
    ));
}

#[cfg(test)]
mod tests;
