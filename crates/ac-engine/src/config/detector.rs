//! The `[detector]` table: the open-vocabulary detector used for discovery
//! on every source.
//!
//! The table is tagged by `kind`: `"none"`, `"synthetic"`, `"owl"` or
//! `"sidecar"` (whose keys are [`SidecarSettings`]). Besides holding the
//! settings, [`DetectorSettings`] answers what the rest of the engine needs
//! to know about the detector, read from its kind and never from a model
//! name: the lowest score its detections carry
//! ([`DetectorSettings::score_floor`]), whether one process serves every
//! source ([`DetectorSettings::is_shared`]) and whether it sees actions
//! ([`DetectorSettings::sees_actions`]).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::SidecarSettings;

/// Default score of the synthetic detector.
fn default_score() -> f32 {
    0.9
}

/// The open-vocabulary detector used for discovery on every source.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DetectorSettings {
    /// No detector: only wide shots and operator reacquires.
    #[default]
    None,
    /// Ground truth of synthetic sources, answering every query with every
    /// moving box. For demos and tests without a model.
    Synthetic {
        /// Score given to every box.
        #[serde(default = "default_score")]
        score: f32,
    },
    /// OWLv2 / OWL-ViT through ONNX Runtime (needs the `onnx` feature).
    Owl {
        /// The combined graph, or the image detector when `text_encoder` is set.
        model: PathBuf,
        /// Text encoder of a split export.
        #[serde(default)]
        text_encoder: Option<PathBuf>,
        /// `tokenizer.json` of the model.
        tokenizer: PathBuf,
        /// Model input side (960 for OWLv2 base).
        #[serde(default)]
        input_size: Option<u32>,
        /// Score threshold for every query.
        #[serde(default)]
        score_threshold: Option<f32>,
        /// Try CoreML on macOS.
        #[serde(default)]
        use_coreml: Option<bool>,
        /// Return whole people chosen by each prompt (the default) instead
        /// of the prompt's own boxes; see `ac_vision::matching`.
        #[serde(default)]
        people_first: Option<bool>,
        /// Lowest "a person" score kept when matching people first.
        #[serde(default)]
        person_threshold: Option<f32>,
    },
    /// A detector in its own process (SAM 3.1 on MLX first), started once
    /// and shared by every source; see [`SidecarSettings`].
    Sidecar(SidecarSettings),
}

impl DetectorSettings {
    /// The lowest score this detector's detections can carry, for the
    /// subject store's floor to follow when it is below the store's
    /// default, which would otherwise silently drop them:
    ///
    /// * OWL matching people first scores everyone and described-person
    ///   prompts on the person scale (default 0.15; see
    ///   `ac_vision::matching`);
    /// * the sidecar returns boxes down to its `score_threshold`.
    pub fn score_floor(&self) -> Option<f32> {
        match self {
            DetectorSettings::Owl {
                people_first,
                person_threshold,
                ..
            } if people_first.unwrap_or(true) => Some(
                person_threshold
                    .unwrap_or(ac_vision::matching::MatchParams::default().person_threshold),
            ),
            DetectorSettings::Sidecar(s) => Some(s.score_threshold),
            _ => None,
        }
    }

    /// True when one detector process serves every source, one pass at a
    /// time (the sidecar), so the engine keeps one request in flight for
    /// the whole studio instead of one per source (see `crate::core`).
    pub fn is_shared(&self) -> bool {
        matches!(self, DetectorSettings::Sidecar(_))
    }

    /// True when the detector sees what a person is doing, not only people
    /// and objects, so a role whose phrase names an action
    /// ([`ac_core::RoleNeeds::Action`]) needs no notice
    /// ([`crate::queries`]). This is the one capability of the design's
    /// contract (Phase 2, "Capability contract"; §4.2: each role "carries a
    /// `needs` tag ..., and the UI warns when the chosen model lacks the
    /// capability") that the engine reads so far, and it reads it from the
    /// detector's kind, never from a model name:
    ///
    /// * the sidecar runs SAM 3.1, which found the speaker for "a person
    ///   speaking" with no false subjects (`sam31-vs-owlv2.md`); until a
    ///   sidecar's hello declares capabilities, every sidecar is taken to
    ///   see actions;
    /// * OWLv2 boxes people and objects: "a person speaking" picked every
    ///   person (benchmark B0);
    /// * the synthetic detector gives every query every box, and no
    ///   detector sees nothing.
    pub fn sees_actions(&self) -> bool {
        matches!(self, DetectorSettings::Sidecar(_))
    }

    /// Checks scores and sizes.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            DetectorSettings::None => Ok(()),
            DetectorSettings::Synthetic { score } => {
                if *score > 0.0 && *score <= 1.0 {
                    Ok(())
                } else {
                    Err(format!("detector.score {score} must be in (0, 1]"))
                }
            }
            DetectorSettings::Owl {
                input_size,
                score_threshold,
                person_threshold,
                ..
            } => {
                if input_size.is_some_and(|s| !(32..=4096).contains(&s)) {
                    return Err("detector.input_size must be in 32..=4096".into());
                }
                if score_threshold.is_some_and(|t| !(0.0..=1.0).contains(&t)) {
                    return Err("detector.score_threshold must be in 0..=1".into());
                }
                if person_threshold.is_some_and(|t| !(0.0..=1.0).contains(&t)) {
                    return Err("detector.person_threshold must be in 0..=1".into());
                }
                Ok(())
            }
            DetectorSettings::Sidecar(s) => s.validate(),
        }
    }
}
