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
//!
//! An untruncated box is always measured on its own proportions, so Full is
//! always the whole box, crown to feet. A truncated box is sized by the
//! first of these that applies:
//!
//! 1. a head the shot remembers from an earlier untruncated box (below);
//! 2. for a box cut by the bottom edge, an assumed head, at every size but
//!    Full (below);
//! 3. the fill rule: boxes cut by the top or too squat, and the Full of a
//!    box cut by the bottom.
//!
//! With a remembered head the crown is still the box top and the scale
//! comes from
//!
//! ```text
//! head = max(remembered, b.h / 7.5)
//! ```
//!
//! The visible part of a body is never taller than the whole body, so
//! `b.h / 7.5` ([`visible_head`]) is a lower bound on the head. The memory
//! keeps a lectern hiding the legs from changing the zoom; the bound keeps a
//! person who walked toward the camera until their feet left the picture
//! from being sized by the smaller head they had further away.
//!
//! # Boxes cut by the bottom
//!
//! A person framed from the knees or the waist up never shows an
//! untruncated box, so there is no head to remember, and the fill rule
//! frames the whole visible body at every tight size: on a box 0.35 of the
//! frame high, Close is 0.38 high and Medium 0.58, with the crown 8% and 40%
//! down the frame. Instead, a box cut by the bottom with its crown in the
//! picture is read as showing `V` head heights from the crown down
//! ([`assumed_visible_heads`]), and cut from the crown on that head:
//!
//! ```text
//! pixel_aspect = b.h / (b.w * source_aspect)
//! V    = clamp(pixel_aspect * SHOULDER_HEADS, 1.6, 3.0)
//! head = b.h / V
//! h    = min(cut_heads(size) * head, b.h) / (1 - headroom)
//! ```
//!
//! For every box tall enough in pixels to be a standing body (pixel aspect
//! at or above [`MIN_STANDING_ASPECT`]) the cap of 3.0 binds, so the rule is
//! one fixed assumption: a person cut by the bottom shows at least crown to
//! waist, so the head is at most `b.h / 3` ([`MAX_ASSUMED_HEADS`], the
//! Medium cut line). That is the loosest head the assumption allows; when
//! more of the body shows (a knee-up shot) the true head is smaller and the
//! shots come out looser than their size, never tighter. The width term is
//! not a width measurement. [`SHOULDER_HEADS`] is `3.0 / MIN_STANDING_ASPECT`
//! (1.875) only so that `V` meets the cap exactly at the squat threshold:
//! squatter boxes read as fewer heads, continuously, down to head and
//! shoulders ([`MIN_ASSUMED_HEADS`], the Close cut line), and a box widened
//! by arms held out or an instrument frames looser, the safe side. The
//! floor keeps the Close cut inside the box. The `min(.., b.h)` keeps every
//! frame from asking for picture below the source edge, so the crown keeps
//! the preset's headroom (with `V` below 3 the Medium cut would fall below
//! the box and the frame would be pushed up). Full keeps the fill rule: its
//! cut, the feet, is outside the box.
//!
//! For a box `(0.45, 0.65, 0.10, 0.35)` on 16:9 in and out (a speaker cut
//! off at about the knees) this gives a Close that needs 5.2x, drawn at the
//! 4x ceiling and flagged limited, a Medium close 0.285 high and a Medium
//! 0.38 high, crown at 3%, 6% and 8%, where the fill rule gave 0.38, 0.45
//! and 0.58 with the crown at 8%, 22% and 40%. The constants are inferred,
//! not measured; ADR 0003 lists them for approval.
//!
//! # Head memory
//!
//! [`HeadMemory`] is where a remembered head height comes from. It holds one
//! value, seeded by the first untruncated box and then smoothed over every
//! later untruncated box with a long half-life
//! ([`FramerConfig::head_half_life`](crate::FramerConfig::head_half_life)).
//! While boxes look truncated it never falls (the box says nothing about how
//! tall the hidden body is) but rises toward the visible lower bound with
//! the same half-life, so it does not stay stuck below what the picture
//! shows. The memory is per shot: each shot's
//! [`ShotFramer`](crate::ShotFramer) owns one and is its only writer, and it
//! lives and dies with the shot. Shots of one subject made together see the
//! same boxes and remember the same value; a shot made later starts from the
//! boxes it sees. The long half-life keeps a raised hand or a box spike just
//! before the legs disappear from setting the scale.
//!
//! It holds a second value in a separate slot: the assumed visible head
//! count `V` ([`HeadMemory::assumed_heads`]), seeded by the first box that is
//! cut by the bottom, shows the crown and is clear of both side edges, then
//! smoothed with the same half-life over every later such box. Without the
//! smoothing, a speaker holding the arms out for a second widens the box,
//! lowers `V` and pumps the zoom, flipping the "(limited)" label each time.
//! `V` is a guess, so it never feeds the measured head: it never seeds or
//! lowers it and never takes part in `max(remembered, b.h / 7.5)`.
//!
//! Once a shot holds `V` it reads the bottom edge with hysteresis, as the
//! framer reads the zoom ceiling:
//!
//! * every box cut by the bottom is framed on the stored `V`, also one that
//!   touches a side edge (its width is cut, so it cannot update `V`) and one
//!   whose top touches the frame top (the crown is taken as the box top; the
//!   frame clamps). Without a stored `V` such boxes keep the fill rule, so
//!   the rule never switches back and forth while a speaker paces to the
//!   edge of the picture;
//! * a box whose bottom is within [`RELEASE_MARGIN`] (0.03, about 22 px at
//!   720p, inferred) of the source bottom still reads as cut by the bottom
//!   ([`measured_head`]): it is not measured on the canon and does not seed
//!   or update the measured head. One box that ends at 0.985 therefore
//!   cannot seed `b.h / 7.5`, the scale of a whole body, and collapse Medium
//!   close and Medium onto the Close.
//!
//! # Limits
//!
//! The crown is the box top, so raised hands or a guitar headstock above the
//! head move it up and enlarge the measured head. The horizontal centre is
//! the box centre, not the torso. The memory lags a person who walks toward
//! the camera and is then hidden at once (a speaker arriving at a lectern):
//! the shot is sized from the head the memory has reached, tighter than
//! true.
//!
//! The assumed head is bounded, not right. A chest-up box seen in profile is
//! narrow enough to read as non-squat, so its Close is cut near the neck. A
//! whole body whose feet sit on the bottom edge reads as crown to waist, so
//! its Close is cut near the hip. A person cut by the bottom at mid-distance
//! (a box about 0.25 high) needs more than the 4x ceiling for both Close and
//! Medium close, so both are drawn at the ceiling and marked as the same
//! picture. The release band needs a stored `V`, so a shot whose very first
//! box ends just clear of the edge band (bottom between 0.97 and 0.99) is
//! measured on the canon before any `V` exists, and that memory then wins.
//! All of these wait for a head source (a head detector, masks or pose).

use ac_core::{Preset, Rect, ShotSize};

use crate::compose::{Composition, compose, headroom, is_finite, place};

/// Height of a standing adult in head heights (the artistic canon).
pub const HEADS_PER_BODY: f64 = 7.5;

/// Lowest pixel height-to-width ratio of a box that can be a whole standing
/// body; squatter boxes look truncated (inferred threshold).
pub const MIN_STANDING_ASPECT: f64 = 1.6;

/// Distance from an edge of the source, as a fraction of its height (top
/// and bottom) or width (sides), within which a box counts as touching that
/// edge.
pub const EDGE_MARGIN: f64 = 0.01;

/// The Close cut line, the tops of the shoulders, in head heights below the
/// crown.
const CLOSE_CUT: f64 = 1.6;

/// The Medium close cut line, the bottom of the sternum, in head heights
/// below the crown.
const MEDIUM_CLOSE_CUT: f64 = 2.3;

/// The Medium cut line, the waist, in head heights below the crown.
const MEDIUM_CUT: f64 = 3.0;

/// Fewest head heights a box cut by the bottom is assumed to show: head and
/// shoulders, the Close cut line, so the Close never asks for picture below
/// the box; see the module docs.
pub const MIN_ASSUMED_HEADS: f64 = CLOSE_CUT;

/// Most head heights a box cut by the bottom is assumed to show: crown to
/// waist, the Medium cut line, so the head is never read smaller than
/// `b.h / 3`; see the module docs (inferred).
pub const MAX_ASSUMED_HEADS: f64 = MEDIUM_CUT;

/// Width of a box cut by the bottom, in head heights, at which the assumed
/// head count reaches [`MAX_ASSUMED_HEADS`] exactly at
/// [`MIN_STANDING_ASPECT`] (1.875, the narrow end of a frontal adult's
/// shoulders with the arms down; inferred). It keeps the assumption
/// continuous across the squat threshold; it is not a width measurement.
pub const SHOULDER_HEADS: f64 = MAX_ASSUMED_HEADS / MIN_STANDING_ASPECT;

/// Distance from the bottom of the source, as a fraction of its height,
/// within which a box still reads as cut by the bottom once its shot holds
/// an assumed head count (hysteresis on [`EDGE_MARGIN`]; about 22 px at
/// 720p, inferred). See the module docs.
pub const RELEASE_MARGIN: f64 = 0.03;

/// The lower cut line of `size`, in head heights below the crown; see the
/// module docs. `None` for [`ShotSize::Wide`], which frames the source.
pub fn cut_heads(size: ShotSize) -> Option<f64> {
    match size {
        ShotSize::Wide => None,
        ShotSize::Full => Some(HEADS_PER_BODY),
        ShotSize::Medium => Some(MEDIUM_CUT),
        ShotSize::MediumClose => Some(MEDIUM_CLOSE_CUT),
        ShotSize::Close => Some(CLOSE_CUT),
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

/// The head height measured on the canon for `bbox`, as [`head_height`],
/// for a shot holding the assumed head count `assumed`
/// ([`HeadMemory::assumed_heads`]): when it holds one, a box within
/// [`RELEASE_MARGIN`] of the source bottom reads as cut by the bottom and
/// is not measured. See the module docs.
pub fn measured_head(bbox: &Rect, source_aspect: f64, assumed: Option<f64>) -> Option<f64> {
    let released = assumed.is_some_and(usable_heads) && in_release_band(bbox);
    head_height(bbox, source_aspect).filter(|_| !released)
}

/// The smallest head the person in `bbox` can have, whether or not the box
/// is truncated: the visible height on the body canon, `bbox.h / 7.5`
/// (see the module docs). `None` for an empty or non-finite box.
pub fn visible_head(bbox: &Rect) -> Option<f64> {
    (is_finite(bbox) && bbox.h > 0.0).then(|| bbox.h / HEADS_PER_BODY)
}

/// True when `bbox` is finite, not empty, and its bottom is within
/// `margin` of the bottom of the source.
fn reaches_bottom(bbox: &Rect, margin: f64) -> bool {
    is_finite(bbox) && bbox.w > 0.0 && bbox.h > 0.0 && bbox.bottom() >= 1.0 - margin
}

/// True when `bbox` touches the left or right edge of the source.
fn touches_a_side(bbox: &Rect) -> bool {
    bbox.x <= EDGE_MARGIN || bbox.right() >= 1.0 - EDGE_MARGIN
}

/// True when `bbox` (finite and not empty) ends within [`RELEASE_MARGIN`]
/// of the source bottom: cut by the bottom, for a shot that already holds
/// an assumed head count. See the module docs.
pub fn in_release_band(bbox: &Rect) -> bool {
    reaches_bottom(bbox, RELEASE_MARGIN)
}

/// True when `heads` can be used as an assumed head count: finite and
/// positive.
pub(crate) fn usable_heads(heads: f64) -> bool {
    heads.is_finite() && heads > 0.0
}

/// How many head heights from the crown down a person box cut by the
/// bottom of a source of `source_aspect` (width over height) is assumed to
/// show: `clamp(pixel_aspect * SHOULDER_HEADS, 1.6, 3.0)`, so always in
/// [`MIN_ASSUMED_HEADS`]`..=`[`MAX_ASSUMED_HEADS`]; see the module docs.
///
/// For a box tall enough in pixels to be a standing body this is always
/// 3.0: the fixed crown-to-waist assumption. `None` unless the box is
/// finite, not empty, touches the bottom edge, shows its crown (clear of
/// the top edge) and is clear of both side edges (a cut width is not the
/// body's), and the aspect is a positive finite number. Only such boxes
/// seed or update a shot's stored count ([`HeadMemory::assumed_heads`]).
pub fn assumed_visible_heads(bbox: &Rect, source_aspect: f64) -> Option<f64> {
    let applies = source_aspect.is_finite()
        && source_aspect > 0.0
        && reaches_bottom(bbox, EDGE_MARGIN)
        && bbox.y > EDGE_MARGIN
        && !touches_a_side(bbox);
    applies.then(|| {
        let pixel_aspect = bbox.h / (bbox.w * source_aspect);
        (pixel_aspect * SHOULDER_HEADS).clamp(MIN_ASSUMED_HEADS, MAX_ASSUMED_HEADS)
    })
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

/// The ideal crop for the person in `bbox`, a box cut by the bottom of the
/// source assumed to show `visible_heads` head heights from the crown down
/// (clamped into [`MIN_ASSUMED_HEADS`]`..=`[`MAX_ASSUMED_HEADS`]), under
/// `preset`: cut on the head `bbox.h / visible_heads`, never asking for
/// picture below the box; see the module docs. Full, the wide, an unusable
/// box or an unusable count fall back to the fill rule, so the result is
/// always finite.
pub fn compose_assumed(
    bbox: &Rect,
    visible_heads: f64,
    preset: &Preset,
    source_aspect: f64,
    output_aspect: f64,
) -> Composition {
    let usable = is_finite(bbox) && bbox.h > 0.0 && usable_heads(visible_heads);
    let cut = cut_heads(preset.size).filter(|_| usable && preset.size != ShotSize::Full);
    let Some(heads) = cut else {
        return compose(bbox, preset, source_aspect, output_aspect);
    };
    let assumed = visible_heads.clamp(MIN_ASSUMED_HEADS, MAX_ASSUMED_HEADS);
    let desired_h = (heads / assumed).min(1.0) * bbox.h / (1.0 - headroom(preset));
    place(
        desired_h,
        bbox.y,
        bbox.center().x,
        preset,
        source_aspect,
        output_aspect,
    )
}

/// A shot's memory of its person's head height, and, kept apart, of how
/// many head heights its boxes cut by the bottom show; see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HeadMemory {
    /// The remembered head height measured on the canon, as a fraction of
    /// the source height.
    head: Option<f64>,
    /// The smoothed assumed visible head count of boxes cut by the bottom
    /// ([`assumed_visible_heads`]). A guess: it never feeds `head`.
    assumed_heads: Option<f64>,
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

    /// The smoothed number of head heights the shot's boxes cut by the
    /// bottom are assumed to show, once one that shows its crown and is
    /// clear of both side edges was seen; always in
    /// [`MIN_ASSUMED_HEADS`]`..=`[`MAX_ASSUMED_HEADS`].
    pub fn assumed_heads(&self) -> Option<f64> {
        self.assumed_heads
    }

    /// Folds in `bbox` (a person box in a source of `source_aspect`) `dt`
    /// seconds after the previous call, and returns the remembered head.
    ///
    /// The head: the first box measured on the canon ([`measured_head`])
    /// seeds the memory; later ones move it by `1 - 2^(-dt / half_life)` of
    /// the difference. Any other box cannot seed it nor lower it, but moves
    /// it the same way toward the box's [`visible_head`] when that is
    /// larger. Once an assumed count is held, a box within
    /// [`RELEASE_MARGIN`] of the bottom is not measured.
    ///
    /// The assumed count ([`Self::assumed_heads`]): the first box with an
    /// [`assumed_visible_heads`] seeds it and later ones move it the same
    /// way; no other box touches it, and it never touches the head.
    ///
    /// A `dt` that is not positive moves nothing, and a `half_life` that is
    /// not positive follows each box at once.
    pub fn observe(
        &mut self,
        bbox: &Rect,
        source_aspect: f64,
        half_life: f64,
        dt: f64,
    ) -> Option<f64> {
        let alpha = smoothing(dt, half_life);
        if let Some(assumed) = assumed_visible_heads(bbox, source_aspect) {
            self.assumed_heads = Some(match self.assumed_heads {
                None => assumed,
                Some(m) => m + alpha * (assumed - m),
            });
        }
        match (
            measured_head(bbox, source_aspect, self.assumed_heads),
            self.head,
        ) {
            (Some(measured), None) => self.head = Some(measured),
            (Some(measured), Some(m)) => self.head = Some(m + alpha * (measured - m)),
            (None, Some(m)) => {
                if let Some(bound) = visible_head(bbox).filter(|b| *b > m) {
                    self.head = Some(m + alpha * (bound - m));
                }
            }
            (None, None) => {}
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
