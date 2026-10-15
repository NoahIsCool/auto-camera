//! Shared domain model for Auto Camera.
//!
//! The vocabulary follows the rebuild specification:
//!
//! * A [`Source`] is a video input (camera, capture card, network feed or file).
//! * A [`Query`] is a named text prompt, such as "speaker upper body", with the
//!   framing [`Preset`]s that apply to every subject it matches and the
//!   prompt's [`ParsedPrompt`], read once when the query is created.
//! * A [`Subject`] is one tracked entity that a query matched on one source;
//!   its [`Extent`] says whether its box is a whole person or the object
//!   itself, which with its query's [`Basis`] decides how its shots are
//!   composed ([`Query::framing_extent`]).
//! * A [`Shot`] is a virtual camera: a subject framed by a preset, or a source wide.
//!
//! An operator's prompt is not the detector's prompt: [`parse_prompt`] splits
//! "speaker upper body" into what to look for ("a person speaking", through
//! the [`Lexicon`]'s role words) and the shot sizes to frame it at (Medium and
//! Medium close), and says whether the subject is framed as a person or an
//! object ([`Basis`]).
//!
//! All geometry is in normalized source coordinates (`0.0..=1.0` on both axes,
//! origin at the top-left), so it does not depend on capture resolution.

pub mod geometry;
pub mod lexicon;
pub mod model;
pub mod prompt;

pub use geometry::{Extent, Point, Rect};
pub use lexicon::{Lexicon, LexiconError, Placement, RoleWord, ShotWord, WordClass};
pub use model::{
    Preset, Query, QueryId, Shot, ShotId, ShotSize, ShotTarget, Source, SourceId, Subject,
    SubjectId, TrackState,
};
pub use prompt::{Basis, ParsedPrompt, PromptWarning, literal_prompt, parse_prompt};
