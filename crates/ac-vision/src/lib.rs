//! Vision for Auto Camera: open-vocabulary detection and per-frame tracking.
//!
//! The crate defines the model-runtime-independent contracts ([`Detector`],
//! [`Tracker`] and their result types) and ships:
//!
//! * [`TemplateTracker`], a pure-Rust tracker that follows every subject on
//!   every frame with normalized cross-correlation on a grayscale pyramid,
//!   constant-velocity prediction and a small scale search. It is the fast
//!   tier that keeps shots framed between detector updates.
//! * [`ScriptedDetector`], a detector driven by a closure or keyframe table,
//!   for tests, demos and offline evaluation.
//! * Behind the `onnx` feature, `OnnxOwlDetector`: OWLv2 / OWL-ViT through
//!   ONNX Runtime (CoreML on macOS when available, CPU otherwise). Its pure
//!   pre- and post-processing lives in [`owl`] and is always compiled, so it is
//!   tested without model files.
//! * Box utilities in [`boxes`]: clipping, centre-size decoding, NMS.
//!
//! All geometry is in normalized source coordinates (see [`ac_core::Rect`]).
//! Nothing here reads clocks; time enters through frame timestamps.

pub mod api;
pub mod boxes;
pub mod error;
mod image;
pub mod matching;
mod ncc;
#[cfg(feature = "onnx")]
pub mod onnx_owl;
pub mod owl;
pub mod scripted;
pub mod tracker;

pub use api::{Detection, Detector, PromptQuery, TrackObservation, Tracker, sanitize_detections};
pub use boxes::{clamp_box, cxcywh_to_rect, nms, nms_indices};
pub use error::VisionError;
#[cfg(feature = "onnx")]
pub use onnx_owl::{OnnxOwlDetector, OwlConfig, OwlModel, OwlTensorNames};
pub use scripted::ScriptedDetector;
pub use tracker::{TemplateTracker, TemplateTrackerConfig};
