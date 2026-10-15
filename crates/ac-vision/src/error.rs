//! Errors raised by detectors and trackers.
//!
//! The enum does not mention any model-runtime type, so code that only uses the
//! traits compiles the same with or without the `onnx` feature. Runtime and
//! tokenizer failures are carried as their display text.

use std::path::PathBuf;

use ac_core::{Rect, SourceId};
use thiserror::Error;

/// Errors from vision components.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VisionError {
    /// A bounding box is non-finite, empty, outside the frame or too small.
    #[error("invalid box {bbox:?}: {reason}")]
    InvalidBox {
        /// The offending box, in normalized source coordinates.
        bbox: Rect,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// The image region has too little texture to be tracked.
    #[error("untrackable region: {0}")]
    Untrackable(String),
    /// A frame from another source was given to a component bound to one source.
    #[error("frame from source {got:?} given to a tracker bound to {expected:?}")]
    SourceMismatch {
        /// Source the component is bound to.
        expected: SourceId,
        /// Source of the frame that was passed.
        got: SourceId,
    },
    /// A configuration value is out of range.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    /// A model file does not exist.
    #[error("model file not found: {0}")]
    ModelNotFound(PathBuf),
    /// The model runtime failed to load or run a model.
    #[error("model runtime: {0}")]
    Model(String),
    /// The tokenizer failed to load or encode a prompt.
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    /// A model output does not have the expected shape.
    #[error("unexpected tensor shape: {0}")]
    Shape(String),
}
