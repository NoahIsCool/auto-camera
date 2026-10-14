//! Shared domain model for Auto Camera.
//!
//! The vocabulary follows the rebuild specification:
//!
//! * A [`Source`] is a video input (camera, capture card, network feed or file).
//! * A [`Query`] is a named text prompt, such as "person's upper body", with the
//!   framing [`Preset`]s that apply to every subject it matches.
//! * A [`Subject`] is one tracked entity that a query matched on one source.
//! * A [`Shot`] is a virtual camera: a subject framed by a preset, or a source wide.
//!
//! All geometry is in normalized source coordinates (`0.0..=1.0` on both axes,
//! origin at the top-left), so it does not depend on capture resolution.

pub mod geometry;
pub mod model;

pub use geometry::{Point, Rect};
pub use model::{
    Preset, Query, QueryId, Shot, ShotId, ShotSize, ShotTarget, Source, SourceId, Subject,
    SubjectId, TrackState,
};
