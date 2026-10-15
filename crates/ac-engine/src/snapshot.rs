//! The engine's published state: one immutable [`EngineSnapshot`] per tick.
//!
//! The engine thread owns all mutable state. Once per tick it assembles an
//! `EngineSnapshot` and publishes it as an `Arc` into a latest-value
//! mailbox ([`crate::mailbox::Latest`]); readers (the operator UI, the
//! headless runner, tests) take the newest one whenever they like, never
//! block the engine and never see a half-updated state. Pictures in a
//! snapshot (monitors and multiview tiles) are `Arc`-backed frames, so a
//! snapshot is cheap to build and to clone; tiles that were not re-rendered
//! this tick are shared with the previous snapshot.

use std::path::PathBuf;
use std::time::Duration;

use ac_core::{Preset, QueryId, Rect, ShotId, SourceId, SubjectId, TrackState};
use ac_director::{CandidateKind, ContentProfile, DirectorMode, Reason};
use ac_media::{SourceState, VideoFrame};
use ac_output::EncoderStats;
use ac_switcher::Transition;

use crate::audio::AudioPacerStats;
use crate::catalog::ShotStatus;
use crate::schedule::TickTimer;
use crate::vision::VisionStats;

/// Tally lamps of a shot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Visible on program (red), including both sides of a running mix.
    pub program: bool,
    /// Staged on preview and not visible on program (green).
    pub preview: bool,
}

/// One shot as the operator sees it.
#[derive(Debug, Clone)]
pub struct ShotView {
    /// Stable id.
    pub id: ShotId,
    /// Operator-facing name.
    pub name: String,
    /// Source the shot crops from.
    pub source: SourceId,
    /// Wide or subject shot.
    pub kind: CandidateKind,
    /// The framed subject, for subject shots.
    pub subject: Option<SubjectId>,
    /// The preset id, for subject shots.
    pub preset: Option<String>,
    /// Current crop in normalized source coordinates.
    pub crop: Rect,
    /// Director quality in `0..=1`.
    pub quality: f32,
    /// Framing settledness in `0..=1`.
    pub settled: f64,
    /// Health.
    pub status: ShotStatus,
    /// The shot can be put on air.
    pub available: bool,
    /// Tally lamps.
    pub tally: Tally,
    /// Multiview tile, refreshed at the thumbnail rate.
    pub thumbnail: Option<VideoFrame>,
}

/// One subject, including hidden ones (so they can be shown again).
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectView {
    /// Source of the subject.
    pub source: SourceId,
    /// Id, unique within the source; also its display number.
    pub id: SubjectId,
    /// Primary query.
    pub query: QueryId,
    /// Other queries it answers.
    pub aliases: Vec<QueryId>,
    /// Current box.
    pub bbox: Rect,
    /// Tracking or lost.
    pub state: TrackState,
    /// Latest tracker confidence.
    pub confidence: f32,
    /// Never retired automatically.
    pub pinned: bool,
    /// Shots hidden from the multiview.
    pub hidden: bool,
    /// On program or preview.
    pub protected: bool,
}

/// One prompt query.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryView {
    /// Stable id.
    pub id: QueryId,
    /// Operator-facing name.
    pub name: String,
    /// Detector prompt.
    pub prompt: String,
    /// Framings.
    pub presets: Vec<Preset>,
}

/// Switcher state.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitcherView {
    /// The shot on air.
    pub program: Option<ShotId>,
    /// The staged shot.
    pub preview: Option<ShotId>,
    /// Selected transition.
    pub transition: Transition,
    /// A transition is running.
    pub in_transition: bool,
    /// Overlay shot and its opacity while mixing.
    pub overlay: Option<(ShotId, f64)>,
    /// Opacity of black over the program.
    pub dip: f64,
}

/// Auto-director state.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectorView {
    /// Autonomy mode.
    pub mode: DirectorMode,
    /// Content profile.
    pub profile: ContentProfile,
    /// Current suggestion.
    pub suggestion: Option<ShotId>,
    /// Why, as data.
    pub reason: Reason,
    /// Why, as text for the operator.
    pub reason_text: String,
    /// Takes the director requested so far.
    pub takes: u64,
}

/// Discovery counters of one source.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveryStats {
    /// Requests sent to the worker.
    pub requests: u64,
    /// Results applied.
    pub applied: u64,
    /// Results dropped as stale.
    pub stale: u64,
    /// Detector errors.
    pub errors: u64,
    /// Requests abandoned after the timeout.
    pub timeouts: u64,
    /// Latency of the newest result.
    pub last_latency: Option<Duration>,
    /// Newest detector error.
    pub last_error: Option<String>,
}

/// One source's health.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceView {
    /// Stable id.
    pub id: SourceId,
    /// Operator-facing name.
    pub name: String,
    /// Health reported by the source.
    pub state: SourceState,
    /// Picture width.
    pub width: u32,
    /// Picture height.
    pub height: u32,
    /// Nominal frame rate.
    pub nominal_fps: f64,
    /// New frames per second seen by the engine over the last second.
    pub fps: f64,
    /// Master-clock time since the newest new frame.
    pub frame_age: Option<Duration>,
    /// New frames seen.
    pub frames: u64,
    /// Frames produced but never seen, because a newer one replaced them in
    /// the mailbox before the engine looked (the engine fell behind).
    pub missed: u64,
    /// The source has fresh frames and its shots can go on air.
    pub available: bool,
    /// Live subjects.
    pub subjects: usize,
    /// The source has a detector.
    pub discovery_enabled: bool,
    /// Discovery counters.
    pub discovery: DiscoveryStats,
    /// Tracking and identity counters.
    pub vision: VisionStats,
}

/// Lifecycle of the program output.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OutputState {
    /// Not started.
    #[default]
    Idle,
    /// ffmpeg is being started.
    Starting,
    /// Encoding.
    Running,
    /// Finishing (flushing the recording).
    Stopping,
    /// Finished cleanly; the stats are final.
    Finished,
    /// Failed to start or exited with an error.
    Failed(String),
}

/// Program output state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputView {
    /// Lifecycle.
    pub state: OutputState,
    /// RTMP URL with the stream key redacted.
    pub rtmp_url: Option<String>,
    /// Recording path.
    pub record_path: Option<PathBuf>,
    /// Encoder statistics (final once finished).
    pub stats: Option<EncoderStats>,
    /// Program audio pacing, when the output carries audio.
    pub audio: Option<AudioPacerStats>,
    /// Program frames handed to the encoder this session.
    pub frames_pushed: u64,
}

/// Master-clock timing.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TimingView {
    /// Processing time per tick.
    pub tick: TickTimer,
    /// Tick interval in milliseconds.
    pub budget_ms: f64,
    /// Ticks that started more than half a period late.
    pub late_ticks: u64,
    /// Ticks skipped after falling far behind.
    pub skipped_ticks: u64,
}

/// Everything the engine publishes once per tick; see the module docs.
#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    /// Ticks run so far.
    pub tick: u64,
    /// Master-clock media time of the tick.
    pub media_time: Duration,
    /// False in the final snapshot published after shutdown, or after the
    /// engine thread died.
    pub running: bool,
    /// Why the engine stopped, when it did not stop on request: the engine
    /// thread panicked (the message says where). Sources were stopped and
    /// the encoder finished on the way out, but nothing is on air.
    pub fatal: Option<String>,
    /// Shots in display order.
    pub shots: Vec<ShotView>,
    /// Subjects of every source.
    pub subjects: Vec<SubjectView>,
    /// Prompt queries.
    pub queries: Vec<QueryView>,
    /// Switcher.
    pub switcher: SwitcherView,
    /// Auto-director.
    pub director: DirectorView,
    /// Sources.
    pub sources: Vec<SourceView>,
    /// Program output.
    pub output: OutputView,
    /// Master-clock timing.
    pub timing: TimingView,
    /// Program picture at monitor size.
    pub program_monitor: Option<VideoFrame>,
    /// Preview picture at monitor size.
    pub preview_monitor: Option<VideoFrame>,
    /// Shots that should have been visible on program but were drawn black.
    pub render_issues: Vec<String>,
    /// Times the program shot changed.
    pub program_changes: u64,
}

impl EngineSnapshot {
    /// The shot with `id`.
    pub fn shot(&self, id: &ShotId) -> Option<&ShotView> {
        self.shots.iter().find(|s| &s.id == id)
    }

    /// The source with `id`.
    pub fn source(&self, id: &SourceId) -> Option<&SourceView> {
        self.sources.iter().find(|s| &s.id == id)
    }

    /// Shots framing a subject (not wides).
    pub fn subject_shots(&self) -> impl Iterator<Item = &ShotView> {
        self.shots
            .iter()
            .filter(|s| s.kind == CandidateKind::Subject)
    }
}
