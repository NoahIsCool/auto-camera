//! The flags of a headless run: what `--seconds` and `--record` ask for,
//! once [`crate::cli`] has parsed them.
//!
//! The run length is counted from when the run is ready, not from when the
//! process started: a first shot, loaded detectors and, with `--record`, a
//! running recording come first (phases 2 to 4 in [`super`]), each wait
//! with its own limit, so a detector that takes minutes to load does not
//! eat into the run.

use std::path::PathBuf;

/// What a headless run does.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadlessOptions {
    /// Length of the run, counted from when recording started (or, without
    /// a recording, from when the first shot was available and every
    /// detector had loaded); the wait for a loading detector is not part
    /// of it.
    pub seconds: f64,
    /// Program recording path; must not exist.
    pub record: Option<PathBuf>,
}
