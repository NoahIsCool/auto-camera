//! Person-basis composition: shots cut at body lines measured in head
//! heights from the crown.
//!
//! # Why
//!
//! The fill rule ([`ShotSize::fill`]) was tuned on upper-body boxes. The
//! people-first OWL detector returns whole people
//! ([`Extent::Person`](ac_core::Extent::Person)), and on a whole person the
//! fill rule frames far too wide. For a person box half the frame high, 16:9
//! in and out:
//!
//! | Size | Fill rule frame height | Person basis frame height | Zoom needed |
//! |---|---|---|---|
//! | `Full` | 1.25, so the wide shot | 0.556 | 1.8x |
//! | `Medium` | 0.83, a long shot | 0.217 | 4.6x |
//! | `MediumClose` | 0.64 | 0.163 | 6.1x |
//! | `Close` | 0.54, a full-length shot | 0.110 | 9.1x |
//!
//! # Body proportions from the box
//!
//! There is no head detector yet, so the head is inferred from the box with
//! the artistic canon of a standing adult about [`HEADS_PER_BODY`] (7.5)
//! head heights tall (a convention the framing research marks unverified).
//! For an untruncated whole-person box `b`:
//!
//! ```text
//! crown = b.y
//! head  = b.h / 7.5
//! cut   = crown + cut_heads(size) * head
//! h     = (cut - crown) / (1 - headroom)
//! top   = crown - headroom * h
//! ```
//!
//! so the crown sits `headroom * h` below the frame top and the frame's
//! bottom edge falls on the cut line. The cut lines ([`cut_heads`]) are
//! broadcast cut points converted to head heights (estimates):
//!
//! | Size | Cut point | Head heights below the crown |
//! |---|---|---|
//! | `Close` | tops of the shoulders | 1.6 (1.5 to 1.75) |
//! | `MediumClose` | bottom of the sternum | 2.3 |
//! | `Medium` | the waist, clear of the hip joint (about 4) | 3.0 |
//! | `Full` | the feet: the whole box | 7.5 |
//!
//! The headroom is the preset's, which `Preset::new` already sets inside the
//! broadcast bands (Close 3%, MCU 6%, MS 8%, Full 10%), now measured from
//! the crown. The frame is centred horizontally on the box. Sizing and zoom
//! limits then follow the shared rules in [`mod@crate::compose`]: a shot the
//! zoom ceiling cannot reach is drawn at the ceiling, crown still at its
//! headroom, and flagged [`Composition::limited_by_zoom`].
//!
//! # Truncated boxes
//!
//! The canon holds only for a whole standing body. A box [looks
//! truncated](looks_truncated) when it touches the bottom edge of the source
//! (the legs leave the picture) or the top edge (the crown may have), or
//! when it is too squat in pixels for a standing body: pixel height over
//! pixel width, `b.h / (b.w * source_aspect)`, below [`MIN_STANDING_ASPECT`]
//! (1.6, an inferred threshold: seated, behind a lectern or a drum kit).
//! Such a box falls back to the fill rule, unless a head height is already
//! known: then the crown is still the box top and the known head sets the
//! scale, so a lectern hiding the legs does not change the zoom.
//!
//! # Head memory
//!
//! [`HeadMemory`] is where a known head height comes from. It holds one
//! value, seeded by the first untruncated box and then smoothed over every
//! later untruncated box with a long half-life
//! ([`FramerConfig::head_half_life`](crate::FramerConfig::head_half_life)),
//! and frozen while boxes look truncated. The memory is per shot: each
//! shot's [`ShotFramer`](crate::ShotFramer) owns one and is its only writer,
//! and it lives and dies with the shot. Shots of one subject made together
//! see the same boxes and remember the same value; a shot made later starts
//! from the boxes it sees. The smoothing also steadies the scale against box
//! jitter, while a speaker walking toward the camera is still followed,
//! slowly.
//!
//! # Limits
//!
//! The crown is the box top, so raised hands or a guitar headstock above the
//! head move it up and enlarge the measured head; the long half-life damps
//! the scale, not the crown. The horizontal centre is the box centre, not the
//! torso. Both wait for a head source (a head detector, masks or pose).

use ac_core::{Preset, Rect, ShotSize};

use crate::compose::{Composition, compose, headroom, is_finite, place};

/// Height of a standing adult in head heights (the artistic canon).
pub const HEADS_PER_BODY: f64 = 7.5;

/// Lowest pixel height-to-width ratio of a box that can be a whole standing
/// body; squatter boxes look truncated (inferred threshold).
pub const MIN_STANDING_ASPECT: f64 = 1.6;

/// Distance from the top or bottom edge of the source, as a fraction of its
/// height, within which a box counts as touching that edge.
pub const EDGE_MARGIN: f64 = 0.01;

/// The lower cut line of `size`, in head heights below the crown; see the
/// module docs. `None` for [`ShotSize::Wide`], which frames the source.
pub fn cut_heads(size: ShotSize) -> Option<f64> {
    match size {
        ShotSize::Wide => None,
        ShotSize::Full => Some(HEADS_PER_BODY),
        ShotSize::Medium => Some(3.0),
        ShotSize::MediumClose => Some(2.3),
        ShotSize::Close => Some(1.6),
    }
}

/// True when `bbox` cannot be a whole, standing person in a source of
/// `source_aspect` (width over height; `1` when invalid): it touches the top
/// or bottom edge, is too squat in pixels, or is empty or not finite. See
/// the module docs.
pub fn looks_truncated(bbox: &Rect, source_aspect: f64) -> bool {
    let aspect = if source_aspect.is_finite() && source_aspect > 0.0 {
        source_aspect
    } else {
        1.0
    };
    !is_finite(bbox)
        || bbox.w <= 0.0
        || bbox.h <= 0.0
        || bbox.y <= EDGE_MARGIN
        || bbox.bottom() >= 1.0 - EDGE_MARGIN
        || bbox.h < MIN_STANDING_ASPECT * bbox.w * aspect
}

/// The head height of the person in `bbox`, as a fraction of the source
/// height, from the body canon; `None` when the box looks truncated.
pub fn head_height(bbox: &Rect, source_aspect: f64) -> Option<f64> {
    (!looks_truncated(bbox, source_aspect)).then(|| bbox.h / HEADS_PER_BODY)
}

/// The ideal crop for the person in `bbox`, whose head is `head_height`
/// high (a fraction of the source height), under `preset`; see the module
/// docs. Falls back to the fill rule when the head height or the box is not
/// usable, so the result is always finite.
pub fn compose_person(
    bbox: &Rect,
    head_height: f64,
    preset: &Preset,
    source_aspect: f64,
    output_aspect: f64,
) -> Composition {
    let usable = is_finite(bbox) && head_height.is_finite() && head_height > 0.0;
    let Some(heads) = cut_heads(preset.size).filter(|_| usable) else {
        return compose(bbox, preset, source_aspect, output_aspect);
    };
    let desired_h = heads * head_height / (1.0 - headroom(preset));
    place(
        desired_h,
        bbox.y,
        bbox.center().x,
        preset,
        source_aspect,
        output_aspect,
    )
}

/// A shot's memory of its person's head height; see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HeadMemory {
    /// The remembered head height, as a fraction of the source height.
    head: Option<f64>,
}

impl HeadMemory {
    /// An empty memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// The remembered head height, if any untruncated box was seen.
    pub fn get(&self) -> Option<f64> {
        self.head
    }

    /// Folds in the head height measured on `bbox` (a person box in a source
    /// of `source_aspect`) `dt` seconds after the previous call, and returns
    /// the remembered value.
    ///
    /// The first measurable box seeds the memory; later ones move it by
    /// `1 - 2^(-dt / half_life)` of the difference. Truncated boxes leave it
    /// as it is. A `dt` that is not positive moves nothing, and a
    /// `half_life` that is not positive follows each box at once.
    pub fn observe(
        &mut self,
        bbox: &Rect,
        source_aspect: f64,
        half_life: f64,
        dt: f64,
    ) -> Option<f64> {
        if let Some(measured) = head_height(bbox, source_aspect) {
            let alpha = smoothing(dt, half_life);
            self.head = Some(self.head.map_or(measured, |m| m + alpha * (measured - m)));
        }
        self.head
    }
}

/// Fraction of the distance to a new value an exponential filter with
/// `half_life` covers in `dt`; always in `0..=1`.
fn smoothing(dt: f64, half_life: f64) -> f64 {
    if dt.is_nan() || dt <= 0.0 {
        0.0
    } else if half_life.is_nan() || half_life <= 0.0 {
        1.0
    } else {
        1.0 - (-dt / half_life).exp2()
    }
}

#[cfg(test)]
mod tests;
