//! Auto-director: proposes the next shot for preview and, when allowed, takes.
//!
//! The operator stays the preview/program authority. The director watches the
//! shot catalog and, once per tick, proposes the shot that should go on air
//! next. In [`DirectorMode::Suggest`] the engine stages that proposal on
//! preview and the operator decides; in [`DirectorMode::Auto`] (opt-in, for
//! unattended use) the director also says when to take it.
//!
//! # Theory
//!
//! The rules follow professional videographers' practice (Rui et al.) and
//! the GAZED shot-selection energy, reduced to a greedy online decision:
//!
//! * **Score** every candidate with [`score`]: weighted quality minus a
//!   recency penalty that rotates among equal shots.
//! * **Hysteresis**: the proposal changes only when a challenger leads it by a
//!   margin δ for a hold time, so near-equal shots do not flap.
//! * **Dwell**: no ordinary take before a minimum dwell; a take is forced at a
//!   maximum dwell (shorter for wides).
//! * **Jump cuts**: never propose a shot of the same source whose crop
//!   overlaps program's with IoU above 0.4.
//! * **Fallback**: when program quality stays low for 0.5 s, propose a wide
//!   at once, and in Auto mode take it at once.
//!
//! # Invariants
//!
//! * The suggestion is never the program shot, never unavailable, never below
//!   the fallback quality, never a jump cut from program.
//! * There is no take outside Auto mode, and never during a transition.
//! * Off mode outputs nothing and keeps no state.
//! * Everything is deterministic: time enters only as the tick's `now`.
//!
//! See [`director`] for the step-by-step procedure.

pub mod candidate;
pub mod config;
pub mod decision;
pub mod director;

pub use candidate::{
    CandidateKind, ScoreWeights, ShotCandidate, base_score, is_jump_cut, recency, score,
};
pub use config::{ConfigError, ContentProfile, DirectorConfig, DirectorMode, Dwell};
pub use decision::{DirectorDecision, Reason, TakeRequest, TransitionHint};
pub use director::{Director, DirectorInput};
