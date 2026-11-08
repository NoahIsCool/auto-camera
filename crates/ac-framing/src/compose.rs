//! Ideal crop composition.
//!
//! A crop is described by its centre and its width as a fraction of the
//! source width. Its height follows from the output aspect ratio:
//!
//! ```text
//! h = w * source_aspect / output_aspect
//! ```
//!
//! How tall the frame should be depends on what the subject's box covers
//! ([`Extent`]); [`compose_subject`] picks the basis. The extent passed in
//! is the one the subject is framed as: the engine marks a box a whole
//! person only when the detector did and the query that asked for the shot
//! is about a person (`ac_core::Basis::framing_extent`), so "guitar
//! close-up" answered with the player holding the guitar stays on the
//! object basis.
//!
//! * **Object basis** ([`compose`], the fill rule). For a subject box `b` and
//!   a preset with fill `f` ([`ShotSize::fill`]): frame height `h = b.h / f`,
//!   so the box fills `f` of the frame. Used for objects, for parts such as an
//!   upper body, and for person boxes too truncated to measure, remember or
//!   guess: with nothing remembered, boxes cut by the top or too squat,
//!   boxes cut by the bottom that touch a side or the top while no assumed
//!   head count is in force, and the Full of any box cut by the bottom or
//!   read as cut by it.
//! * **Person basis** ([`crate::person`]). A whole-person box is cut at a body
//!   line measured in head heights from the crown, so "Close" on a person
//!   half the frame high asks for a head-and-shoulders shot, not a
//!   full-length one (and is drawn at the zoom ceiling, flagged, when it
//!   needs more zoom than the preset allows). A box that is not measured on
//!   its own proportions (truncated, or read as cut by the bottom) uses the
//!   shot's remembered head; a box cut by the bottom of the picture with
//!   nothing remembered is cut on a bounded assumed head instead, framed
//!   down from the crown and never below the box
//!   ([`person::compose_assumed`]).
//!
//! Both then place the frame the same way:
//!
//! * the frame's top edge sits `r * h` above the subject's top edge (the
//!   crown, for a person), where `r` is the preset's headroom;
//! * the frame is centred horizontally on the subject;
//! * the width is limited by the preset's zoom ceiling (`w >= 1 / max_zoom`)
//!   and by the source (`w <= 1`, `h <= 1`), and the crop is moved, never
//!   resized, to stay inside the source picture.
//!
//! # Zoom honesty
//!
//! When the zoom ceiling stops a shot reaching its size, the crop is drawn at
//! the ceiling, looser than asked, and [`Composition::limited_by_zoom`] says
//! so, so the operator sees "Close (limited)" instead of a close-up that is
//! not one. [`Composition::zoom_fit`] says by how much (the wanted width over
//! the narrowest the ceiling allows), so the framer can keep the verdict
//! steady near the ceiling. Hitting the source bounds (a shot wider than the
//! picture) is not zoom limiting and is not flagged.
//!
//! # Invariants
//!
//! For every input, finite or not, the result is finite and never wider
//! than the source allows:
//!
//! * a non-finite box frames the whole source;
//! * a degenerate (empty) box frames its position at the zoom ceiling;
//! * headroom is clamped into `0..=0.5`, the config's range;
//! * a NaN zoom ceiling allows no zoom (1x);
//! * invalid aspect ratios are treated as equal.
//!
//! For valid inputs on the object basis the crop is exactly the historical
//! fill-rule crop.

use ac_core::{Extent, Point, Preset, Rect, ShotSize};

use crate::person;

/// Largest headroom applied, as a fraction of the frame height; the config
/// accepts `0..=0.5`.
const MAX_HEADROOM: f64 = 0.5;

/// Relative slack before a shot counts as limited by zoom, so a size that
/// lands exactly on the ceiling (up to rounding) is not flagged.
const LIMIT_TOLERANCE: f64 = 1e-9;

/// An ideal crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Composition {
    /// Centre of the crop.
    pub center: Point,
    /// Width as a fraction of the source width.
    pub width: f64,
    /// The preset's zoom ceiling stopped the shot reaching its size: the
    /// crop is drawn at the ceiling and is looser than the size asks for.
    pub limited_by_zoom: bool,
    /// The width the shot's size asks for over the narrowest width the zoom
    /// ceiling allows: below 1 the ceiling holds the shot back by that
    /// factor, at or above 1 the size is reachable. Infinite when nothing
    /// is asked of the zoom (wides, unusable boxes); never NaN.
    pub zoom_fit: f64,
}

impl Composition {
    /// The crop as a rectangle, given the source and output aspect ratios.
    pub fn rect(&self, source_aspect: f64, output_aspect: f64) -> Rect {
        let h = match aspects(source_aspect, output_aspect) {
            Some((src, out)) => self.width * src / out,
            None => self.width,
        };
        Rect::from_center(self.center, self.width, h)
    }
}

/// What composition needs to know about a subject on one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubjectGeometry {
    /// The subject's box in normalized source coordinates.
    pub bbox: Rect,
    /// What the box covers.
    pub extent: Extent,
    /// The shot's remembered head height for this person, as a fraction of
    /// the source height ([`crate::HeadMemory`]). Used only when `bbox` is
    /// not measured on the canon ([`person::measured_head`]: it looks
    /// truncated, or reads as cut by the bottom), and then never below what
    /// the box itself shows ([`person::visible_head`]); a measured box is
    /// framed on its own proportions. Ignored for objects.
    pub remembered_head: Option<f64>,
    /// The assumed visible head count in force for this person's shot
    /// ([`crate::HeadMemory::assumed_heads_in_force`]): its smoothed count
    /// while it reads the person as cut by the bottom, else `None`. When it
    /// is a positive finite number, every box within
    /// [`person::RELEASE_MARGIN`] of the source bottom reads as cut by the
    /// bottom: it is not measured on the canon ([`person::measured_head`])
    /// and, with nothing remembered, is cut on this count. Without one, only
    /// a box with its own [`person::assumed_visible_heads`] is guessed.
    /// Ignored for objects.
    pub assumed_heads: Option<f64>,
}

impl SubjectGeometry {
    /// A subject known only by its box and extent.
    pub fn new(bbox: Rect, extent: Extent) -> Self {
        Self {
            bbox,
            extent,
            remembered_head: None,
            assumed_heads: None,
        }
    }
}

/// The aspect ratios when both are positive finite numbers.
fn aspects(source_aspect: f64, output_aspect: f64) -> Option<(f64, f64)> {
    let valid = |a: f64| a.is_finite() && a > 0.0;
    (valid(source_aspect) && valid(output_aspect)).then_some((source_aspect, output_aspect))
}

/// Crop height per unit of crop width for the given aspect ratios; `1` when
/// either ratio is not a positive finite number.
pub(crate) fn height_per_width(source_aspect: f64, output_aspect: f64) -> f64 {
    aspects(source_aspect, output_aspect).map_or(1.0, |(src, out)| src / out)
}

/// Largest crop width that fits the source for the given aspect ratios
/// (`1` when either is not a positive finite number).
pub fn max_width(source_aspect: f64, output_aspect: f64) -> f64 {
    aspects(source_aspect, output_aspect).map_or(1.0, |(src, out)| (out / src).min(1.0))
}

/// The whole source picture, as much as the output aspect allows, centred.
fn wide(source_aspect: f64, output_aspect: f64) -> Composition {
    Composition {
        center: Point { x: 0.5, y: 0.5 },
        width: max_width(source_aspect, output_aspect),
        limited_by_zoom: false,
        zoom_fit: f64::INFINITY,
    }
}

/// True when every coordinate of `r` is finite.
pub(crate) fn is_finite(r: &Rect) -> bool {
    [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite())
}

/// The preset's headroom, clamped into `0..=MAX_HEADROOM` (`0` when NaN).
pub(crate) fn headroom(preset: &Preset) -> f64 {
    if preset.headroom.is_nan() {
        0.0
    } else {
        preset.headroom.clamp(0.0, MAX_HEADROOM)
    }
}

/// Places a frame of height `desired_h` (a fraction of the source height)
/// with the preset's headroom above `top` and centred on `center_x`; see the
/// module docs. `top` and `center_x` must be finite.
pub(crate) fn place(
    desired_h: f64,
    top: f64,
    center_x: f64,
    preset: &Preset,
    source_aspect: f64,
    output_aspect: f64,
) -> Composition {
    let ratio = height_per_width(source_aspect, output_aspect);
    let widest = max_width(source_aspect, output_aspect);
    // A NaN ceiling allows no zoom; an infinite one still leaves a width.
    let zoom = if preset.max_zoom.is_nan() {
        1.0
    } else {
        preset.max_zoom.clamp(1.0, f64::MAX)
    };
    let narrowest = (1.0 / zoom).min(widest);
    let desired_w = if desired_h.is_nan() {
        widest
    } else {
        desired_h / ratio
    };
    let width = desired_w.clamp(narrowest, widest);
    let limited_by_zoom = desired_w < narrowest * (1.0 - LIMIT_TOLERANCE);
    let zoom_fit = desired_w / narrowest;
    let h = width * ratio;
    let frame_top = top - headroom(preset) * h;
    let center = Point {
        x: center_x,
        y: frame_top + h / 2.0,
    };
    let bounded = Rect::from_center(center, width, h).clamped_within(&Rect::FULL);
    Composition {
        center: bounded.center(),
        width,
        limited_by_zoom,
        zoom_fit,
    }
}

/// Computes the ideal crop for `subject` under `preset` on the object
/// basis (the fill rule); see the module docs.
pub fn compose(
    subject: &Rect,
    preset: &Preset,
    source_aspect: f64,
    output_aspect: f64,
) -> Composition {
    if preset.size == ShotSize::Wide || !is_finite(subject) {
        return wide(source_aspect, output_aspect);
    }
    let desired_h = subject.h / preset.size.fill();
    place(
        desired_h,
        subject.y,
        subject.center().x,
        preset,
        source_aspect,
        output_aspect,
    )
}

/// Computes the ideal crop for `subject` under `preset`, on the basis its
/// extent calls for; see the module docs and [`crate::person`].
///
/// A person box is sized by the first of:
///
/// 1. its own proportions, when it is measured on the canon
///    ([`person::measured_head`]: not [`person::looks_truncated`] and,
///    with an assumed count in force, clear of [`person::RELEASE_MARGIN`]);
/// 2. the larger of the remembered head and [`person::visible_head`];
/// 3. an assumed head ([`person::compose_assumed`], every size but Full),
///    for a box cut by the bottom: the count in force for any box within
///    [`person::RELEASE_MARGIN`] of the bottom, or, with none in force, the
///    box's own [`person::assumed_visible_heads`];
/// 4. the object basis.
pub fn compose_subject(
    subject: &SubjectGeometry,
    preset: &Preset,
    source_aspect: f64,
    output_aspect: f64,
) -> Composition {
    let bbox = &subject.bbox;
    if subject.extent == Extent::Object || preset.size == ShotSize::Wide || !is_finite(bbox) {
        return compose(bbox, preset, source_aspect, output_aspect);
    }
    let in_force = subject.assumed_heads.filter(|v| person::usable_heads(*v));
    let remembered = subject
        .remembered_head
        .filter(|h| h.is_finite() && *h > 0.0)
        .map(|h| person::visible_head(bbox).map_or(h, |bound| h.max(bound)));
    if let Some(head) = person::measured_head(bbox, source_aspect, in_force).or(remembered) {
        return person::compose_person(bbox, head, preset, source_aspect, output_aspect);
    }
    let assumed = match in_force {
        Some(heads) => person::in_release_band(bbox).then_some(heads),
        None => person::assumed_visible_heads(bbox, source_aspect),
    };
    match assumed {
        Some(heads) => person::compose_assumed(bbox, heads, preset, source_aspect, output_aspect),
        None => compose(bbox, preset, source_aspect, output_aspect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: f64 = 16.0 / 9.0;
    const OUT: f64 = 16.0 / 9.0;

    fn person() -> Rect {
        Rect::new(0.45, 0.3, 0.1, 0.3)
    }

    #[test]
    fn wide_preset_frames_the_whole_source() {
        let c = compose(
            &person(),
            &Preset::new("w", "Wide", ShotSize::Wide),
            SRC,
            OUT,
        );
        assert_eq!(c.rect(SRC, OUT), Rect::FULL);
        assert!(!c.limited_by_zoom);
    }

    #[test]
    fn subject_fills_the_requested_fraction_and_headroom() {
        let p = Preset::new("m", "Medium", ShotSize::Medium);
        let c = compose(&person(), &p, SRC, OUT);
        let r = c.rect(SRC, OUT);
        assert!((person().h / r.h - ShotSize::Medium.fill()).abs() < 1e-9);
        assert!(((person().y - r.y) / r.h - p.headroom).abs() < 1e-9);
        assert!((r.center().x - person().center().x).abs() < 1e-9);
        assert!(!c.limited_by_zoom);
    }

    #[test]
    fn zoom_ceiling_limits_tiny_subjects() {
        let tiny = Rect::new(0.5, 0.5, 0.01, 0.02);
        let p = Preset::new("c", "Close", ShotSize::Close);
        let c = compose(&tiny, &p, SRC, OUT);
        assert!((c.width - 1.0 / p.max_zoom).abs() < 1e-12);
        assert!(c.limited_by_zoom, "drawn at the ceiling, and said so");
    }

    #[test]
    fn a_size_exactly_at_the_ceiling_is_not_limited() {
        let p = Preset::new("c", "Close", ShotSize::Close);
        // h / fill == 1 / max_zoom exactly.
        let b = Rect::new(0.4, 0.4, 0.1, 0.25 * ShotSize::Close.fill());
        let c = compose(&b, &p, SRC, OUT);
        assert!((c.width - 0.25).abs() < 1e-12);
        assert!(!c.limited_by_zoom);
    }

    #[test]
    fn hitting_the_source_bounds_is_not_zoom_limiting() {
        let tall = Rect::new(0.4, 0.05, 0.2, 0.9);
        let c = compose(&tall, &Preset::new("f", "Full", ShotSize::Full), SRC, OUT);
        assert_eq!(c.width, 1.0);
        assert!(!c.limited_by_zoom);
    }

    #[test]
    fn crop_stays_inside_source_near_edges() {
        let edge = Rect::new(0.95, 0.02, 0.05, 0.2);
        let c = compose(
            &edge,
            &Preset::new("m", "Medium", ShotSize::Medium),
            SRC,
            OUT,
        );
        assert!(Rect::FULL.contains_rect(&c.rect(SRC, OUT)));
    }

    #[test]
    fn portrait_output_from_landscape_source_fits() {
        let c = compose(
            &person(),
            &Preset::new("w", "Wide", ShotSize::Wide),
            SRC,
            9.0 / 16.0,
        );
        let r = c.rect(SRC, 9.0 / 16.0);
        assert!((r.h - 1.0).abs() < 1e-12);
        assert!(Rect::FULL.contains_rect(&r));
    }

    #[test]
    fn object_extent_keeps_the_fill_rule() {
        for size in [ShotSize::Full, ShotSize::Medium, ShotSize::Close] {
            let p = Preset::new("p", "P", size);
            let geometry = SubjectGeometry {
                remembered_head: Some(0.05),
                ..SubjectGeometry::new(person(), Extent::Object)
            };
            assert_eq!(
                compose_subject(&geometry, &p, SRC, OUT),
                compose(&person(), &p, SRC, OUT),
                "{size:?}: an object never uses a head height"
            );
        }
    }

    #[test]
    fn invalid_aspects_are_treated_as_equal() {
        for (src, out) in [
            (f64::NAN, OUT),
            (SRC, 0.0),
            (-1.0, OUT),
            (SRC, f64::INFINITY),
        ] {
            assert_eq!(height_per_width(src, out), 1.0);
            assert_eq!(max_width(src, out), 1.0);
        }
        assert!((height_per_width(SRC, 9.0 / 16.0) - SRC * 16.0 / 9.0).abs() < 1e-12);
    }

    #[test]
    fn headroom_is_clamped_into_the_config_range() {
        let mut p = Preset::new("m", "Medium", ShotSize::Medium);
        for (given, used) in [(f64::NAN, 0.0), (-0.2, 0.0), (0.08, 0.08), (3.0, 0.5)] {
            p.headroom = given;
            assert_eq!(headroom(&p), used, "{given}");
        }
    }
}
