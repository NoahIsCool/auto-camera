//! Ground-truth labels for one clip of the evaluation set.
//!
//! Releases are gated on measured quality on annotated footage (spec V9).
//! A [`LabelFile`] is the ground truth for one video clip: which file it
//! is, where the people are on the labelled frames, which roles they hold
//! and which parts of the picture are not labelled. It is plain data, read
//! and written as JSON ([`LabelFile::from_json`], [`LabelFile::to_json`])
//! and checked by [`LabelFile::validate`]. Reading the file, hashing the
//! video and running the engine over it belong to the runner; this module
//! never touches the file system.
//!
//! # One file, one clip
//!
//! A label file is self-contained: a scorer needs only it and the video.
//! The [`ClipManifest`] names the video by the SHA-256 of its bytes, so a
//! re-encoded or trimmed file can never be scored against labels drawn on
//! another one, and records the facts every label depends on: picture size,
//! frame rate, frame count and the camera that shot it.
//!
//! # Time
//!
//! Frames are counted from 0 in presentation order. A [`Keyframe`] carries
//! both its frame index (what labelling tools count) and its presentation
//! time in nanoseconds from the clip's first frame (what the engine tags
//! every frame and observation with), so labels join engine output by PTS.
//! Clips have a constant frame rate (variable-rate footage is transcoded
//! before labelling), and the two must agree: the frame nearest a
//! keyframe's PTS at the clip's rate is its frame ([`FrameRate`]). The rule
//! catches the common slips, frames counted from 1 and times in the wrong
//! unit. Role intervals are spans of frame indexes, half open.
//!
//! # People
//!
//! Every person is declared once as a [`PersonTrack`] with a stable
//! [`TrackId`]. Keyframes are labelled exhaustively: a keyframe lists every
//! person visible in it outside the exclusion zones, each as a
//! [`PersonBox`] naming its track. Nobody listed means nobody there, which
//! is what lets a scorer count false detections. Boxes use the normalized
//! coordinates of [`ac_core::Rect`] and cover the visible part of the whole
//! person, as an [`Extent::Person`](ac_core::Extent::Person) detection
//! does. Each box says:
//!
//! * its *visibility*, the fraction of the whole person that can be seen
//!   (`0.0..=1.0`, the MOT Challenge convention), lowered by occlusion and
//!   by the picture's edges alike;
//! * its [`Truncation`], the sides at which the body continues beyond the
//!   box, hidden by the frame edge or by something in front (a lectern, a
//!   drum kit). It is the ground truth for `ac_framing`'s "looks truncated"
//!   guess, and a truncated person is never fully visible;
//! * its [`BodyLines`]: the heights of the crown, chin, tops of the
//!   shoulders and waist. `ac_framing` infers these from the box by the
//!   7.5-head canon to cut person shots; labelled lines measure that guess.
//!
//! # Roles
//!
//! A [`RoleTag`] is a word for what someone is or does ("pastor",
//! "speaker", "worship leader"), in the style of the prompt lexicon's role
//! words. A track's `tags` hold for the whole clip; a [`RoleInterval`] gives
//! a track a role for part of it (who is speaking when). A track holds a
//! role at a frame when the role is one of its tags or an interval covers
//! the frame, so the same role is never given both ways, and one track's
//! intervals for one role never overlap.
//!
//! # Exclusions
//!
//! An [`ExclusionZone`] is a polygon of the camera's picture whose people
//! are not labelled, such as the audience or a screen repeating the stage.
//! The cameras of the evaluation set are locked off, so zones belong to the
//! camera and hold for the whole clip. A detection inside a zone that
//! matches no labelled person is neither a hit nor a false alarm; a real
//! person walking in front of a zone is still labelled. A picture of a
//! person (a photo on the backdrop) is not a person: it is left unlabelled
//! and outside any zone, so detecting it counts as a false alarm.
//!
//! # Versioning
//!
//! Every file states its `schema_version`. A reader accepts only the
//! version it was built for ([`SCHEMA_VERSION`]) and checks it before
//! anything else, so a file from another version is reported as such and
//! not as a pile of field errors. Unknown fields are rejected, so a
//! misspelt field is an error rather than a default. Any change to the
//! fields or to what they mean bumps the version.
//!
//! # Invariants
//!
//! A file that passes [`LabelFile::validate`] keeps every rule listed in
//! [`mod@validate`]; in short: ids are well formed and unique, every
//! reference names a declared track, every coordinate lies in `0..=1`,
//! keyframes are in frame order inside the clip with PTS that name their
//! frames, and role intervals are non-empty, inside the clip and never
//! overlap for one track and role.

use std::fmt;

use ac_core::{Point, Rect};
use serde::{Deserialize, Serialize};

mod error;
mod json;
pub mod validate;

pub use error::{FieldError, FieldErrors};

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// Most frames a clip may have: ten million, 46 hours at 60 fps. Bounds
/// what an importer allocates for one clip.
pub const MAX_FRAMES: u64 = 10_000_000;

/// Highest frame rate a clip may declare, in frames per second (the bound
/// the file source accepts).
pub const MAX_FPS: u32 = 1000;

/// Nanoseconds per second.
const NANOS: u128 = 1_000_000_000;

/// Ground-truth labels for one clip; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelFile {
    /// The schema version the file follows: [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The video the labels were drawn on.
    pub clip: ClipManifest,
    /// Areas of the camera's picture whose people are not labelled.
    pub exclusions: Vec<ExclusionZone>,
    /// Every person labelled anywhere in the clip.
    pub tracks: Vec<PersonTrack>,
    /// Roles tracks hold for part of the clip.
    pub roles: Vec<RoleInterval>,
    /// The labelled frames, in frame order.
    pub keyframes: Vec<Keyframe>,
}

/// Identifies a clip: letters, digits, `-` and `_`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClipId(pub String);

/// Identifies a physical camera position: letters, digits, `-` and `_`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CameraId(pub String);

/// The video a label file describes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipManifest {
    /// Stable name of the clip.
    pub id: ClipId,
    /// SHA-256 of the video file's bytes, as 64 lowercase hex digits.
    pub sha256: String,
    /// The camera that shot the clip; its exclusion zones are this file's.
    pub camera: CameraId,
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
    /// The clip's constant frame rate.
    pub fps: FrameRate,
    /// Number of frames in the clip, `1..=`[`MAX_FRAMES`].
    pub frame_count: u64,
}

/// A frame rate as a fraction of frames per second (`30000/1001`, not
/// `29.97`), so frame times are exact however long the clip.
///
/// Frame `n` is presented at `n * den / num` seconds, rounded down to the
/// nanosecond as the file source computes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameRate {
    /// Numerator: frames.
    pub num: u32,
    /// Denominator: seconds.
    pub den: u32,
}

impl FrameRate {
    /// `num / den` frames per second, unchecked.
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    /// Whether both parts are non-zero and the rate is at most [`MAX_FPS`].
    pub fn is_valid(&self) -> bool {
        self.num > 0
            && self.den > 0
            && u64::from(self.num) <= u64::from(MAX_FPS) * u64::from(self.den)
    }

    /// Presentation time of `frame` in nanoseconds from the first frame,
    /// saturating at `u64::MAX`; `None` when a part of the rate is zero.
    pub fn pts_ns(&self, frame: u64) -> Option<u64> {
        if self.num == 0 || self.den == 0 {
            return None;
        }
        let ns = u128::from(frame) * u128::from(self.den) * NANOS / u128::from(self.num);
        Some(u64::try_from(ns).unwrap_or(u64::MAX))
    }

    /// The frame presented nearest to `pts_ns` (ties go to the later
    /// frame); `None` when a part of the rate is zero.
    pub fn nearest_frame(&self, pts_ns: u64) -> Option<u64> {
        if self.num == 0 || self.den == 0 {
            return None;
        }
        let period = u128::from(self.den) * NANOS;
        let frame = (u128::from(pts_ns) * u128::from(self.num) + period / 2) / period;
        Some(u64::try_from(frame).unwrap_or(u64::MAX))
    }
}

impl fmt::Display for FrameRate {
    /// `30` for a whole rate, `30000/1001` otherwise.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

/// An area of the camera's picture whose people are not labelled; see the
/// module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExclusionZone {
    /// Name of the zone, such as `audience`: letters, digits, `-` and `_`,
    /// unique in the file.
    pub id: String,
    /// Vertices in normalized picture coordinates, in drawing order; at
    /// least three, enclosing a non-zero area.
    pub polygon: Vec<Point>,
}

/// Identifies a person within one clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TrackId(pub u32);

/// A word for what a person is or does, such as `pastor`, `speaker` or
/// `worship leader`: lowercase ASCII letters and digits in words joined by
/// single spaces, `-` or `_`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoleTag(pub String);

/// One person labelled in the clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonTrack {
    /// Unique in the file; keyframes and role intervals refer to it.
    pub id: TrackId,
    /// Roles the person holds for the whole clip, such as `pastor`; each
    /// at most once.
    pub tags: Vec<RoleTag>,
}

/// A role a track holds over part of the clip: frames
/// `start_frame..end_frame`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleInterval {
    /// The track holding the role.
    pub track: TrackId,
    /// The role, such as `speaker`.
    pub role: RoleTag,
    /// First frame of the interval.
    pub start_frame: u64,
    /// Frame after the last one; greater than `start_frame` and at most
    /// the clip's frame count.
    pub end_frame: u64,
}

/// One labelled frame: every person visible in it outside the exclusion
/// zones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    /// Frame index, counted from 0.
    pub frame: u64,
    /// Presentation time in nanoseconds from the clip's first frame.
    pub pts_ns: u64,
    /// The people in the frame, each track at most once.
    pub people: Vec<PersonBox>,
}

/// One person on one keyframe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonBox {
    /// Which person.
    pub track: TrackId,
    /// The visible part of the whole person, in normalized coordinates
    /// inside the picture.
    pub bbox: Rect,
    /// Fraction of the whole person that can be seen, `0.0..=1.0`.
    pub visibility: f64,
    /// Sides at which the body continues beyond the box; omitted when none.
    #[serde(default, skip_serializing_if = "Truncation::is_none")]
    pub truncation: Truncation,
    /// Labelled body lines; omitted when none is labelled.
    #[serde(default, skip_serializing_if = "BodyLines::is_empty")]
    pub lines: BodyLines,
}

/// The sides of a person's box at which the body continues out of sight,
/// cut by the frame edge or hidden by something in front.
///
/// Written with only the cut sides (`{"bottom": true}`), and left out of a
/// [`PersonBox`] when no side is cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Truncation {
    /// The top is cut: the crown may be out of sight.
    #[serde(default, skip_serializing_if = "is_false")]
    pub top: bool,
    /// The bottom is cut: legs or feet are out of sight.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bottom: bool,
    /// The left side is cut.
    #[serde(default, skip_serializing_if = "is_false")]
    pub left: bool,
    /// The right side is cut.
    #[serde(default, skip_serializing_if = "is_false")]
    pub right: bool,
}

impl Truncation {
    /// No side cut.
    pub const NONE: Truncation = Truncation {
        top: false,
        bottom: false,
        left: false,
        right: false,
    };

    /// Whether no side is cut.
    pub fn is_none(&self) -> bool {
        *self == Self::NONE
    }

    /// The names of the cut sides, top first, then bottom, left and right.
    pub fn sides(&self) -> Vec<&'static str> {
        [
            (self.top, "top"),
            (self.bottom, "bottom"),
            (self.left, "left"),
            (self.right, "right"),
        ]
        .into_iter()
        .filter_map(|(cut, side)| cut.then_some(side))
        .collect()
    }
}

/// Serde helper: leave `false` flags out of the written file.
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// Heights of a person's body lines, as normalized `y` in the picture.
///
/// Each line is labelled only where it can be seen or placed with
/// confidence. The lines are those of an upright body, so labelled lines
/// lie inside the person's box, top to bottom in this order; a pose where
/// they do not (lying down, bent double) is left without lines.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyLines {
    /// Top of the head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crown: Option<f64>,
    /// Bottom of the chin; `chin - crown` is the head height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chin: Option<f64>,
    /// Tops of the shoulders, where a close-up is cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shoulder: Option<f64>,
    /// The waist, where a medium shot is cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waist: Option<f64>,
}

impl BodyLines {
    /// Whether no line is labelled.
    pub fn is_empty(&self) -> bool {
        self.named().iter().all(|(_, line)| line.is_none())
    }

    /// Every line with its field name, top to bottom.
    pub fn named(&self) -> [(&'static str, Option<f64>); 4] {
        [
            ("crown", self.crown),
            ("chin", self.chin),
            ("shoulder", self.shoulder),
            ("waist", self.waist),
        ]
    }
}

#[cfg(test)]
mod tests;
