//! The flags of a headless run: what `--seconds`, `--record` and `--trace`
//! ask for, once [`crate::cli`] has parsed them.
//!
//! The run length is counted from when the run is ready, not from when the
//! process started: a first shot, loaded detectors and, with `--record`, a
//! running recording come first (phases 2 to 4 in [`super`]), each wait
//! with its own limit, so a detector that takes minutes to load does not
//! eat into the run. A run that ends well saves the show last, before the
//! engine stops, when `--save-show` names a file.

use std::path::PathBuf;

use crate::show::ShowTarget;

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
    /// Per-tick trace path (JSON Lines, [`crate::trace`]); must not exist.
    /// The trace covers the whole run, from the engine's first tick.
    pub trace: Option<PathBuf>,
    /// Where the show is saved when the run ends well (`--save-show`,
    /// [`crate::show`]); `None` saves nothing.
    pub save_show: Option<ShowTarget>,
}
