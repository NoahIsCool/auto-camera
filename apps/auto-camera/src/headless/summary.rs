//! The JSON summary of a headless run: plain serializable data, built by
//! `summarize` in the parent module from the run's snapshots.
//!
//! Every value is rounded for reading (milliseconds to one decimal,
//! normalized coordinates to four, scores to two) and absent values are
//! `null`, so a summary diffed between runs shows changes in behaviour, not
//! in float noise.
//!
//! Besides counters it carries what a run without a window needs to judge
//! the pictures: whether each source's detector was ready and when (a run
//! that ended while it loaded says nothing about detection), each shot's
//! crop width and height range over the run (Phase 1 Milestone 1 of
//! `/mnt/project-files/research/model-picker-design.md` §5 asks a headless
//! band run for a crop-height variation of at most 10% on static
//! performers), and the subjects at the end with the queries they answer,
//! so duplicate subjects and their roles can be told apart without a
//! contact sheet.

use std::path::PathBuf;

use ac_core::TrackState;
use ac_director::{ContentProfile, DirectorMode};
use ac_engine::ShotStatus;
use serde::Serialize;

/// The JSON summary of a run.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// Every phase succeeded.
    pub ok: bool,
    /// Why the run failed.
    pub error: Option<String>,
    /// Config file used.
    pub config: PathBuf,
    /// Requested run length.
    pub seconds: f64,
    /// Wall-clock length of the whole run, setup and shutdown included.
    pub wall_seconds: f64,
    /// Ticks run.
    pub ticks: u64,
    /// Master-clock time at the end.
    pub media_seconds: f64,
    /// Threads joined at shutdown (discovery workers, output helpers).
    pub threads_joined: usize,
    /// Per-source counters.
    pub sources: Vec<SourceSummary>,
    /// Largest number of live subjects seen.
    pub max_subjects: usize,
    /// Shots at the end of the run.
    pub shots: Vec<ShotSummary>,
    /// Subjects at the end of the run, in source order.
    pub subjects: Vec<SubjectSummary>,
    /// Program shot changes.
    pub program: ProgramSummary,
    /// Auto-director state at the end.
    pub director: DirectorSummary,
    /// Master-clock timing.
    pub timing: TimingSummary,
    /// The output session, when one was started.
    pub output: Option<OutputSummary>,
    /// Shots that should have been on program but were drawn black.
    pub render_issues: Vec<String>,
}

/// One source in the summary.
#[derive(Debug, Clone, Serialize)]
pub struct SourceSummary {
    /// Source id.
    pub id: String,
    /// Picture size.
    pub size: [u32; 2],
    /// Frame rate measured over the last second.
    pub fps: f64,
    /// New frames seen.
    pub frames: u64,
    /// Frames replaced in the mailbox before the engine saw them.
    pub missed: u64,
    /// The run ended while the source's detector was still loading, so the
    /// discovery counters say nothing about the detector.
    pub discovery_preparing: bool,
    /// Master-clock seconds at which the detector was ready (`null` while
    /// it loaded, and without a detector).
    pub detector_ready_s: Option<f64>,
    /// Discovery requests sent.
    pub discovery_requests: u64,
    /// Discovery results applied.
    pub discovery_applied: u64,
    /// Discovery results dropped as stale.
    pub discovery_stale: u64,
    /// Detector errors.
    pub discovery_errors: u64,
    /// Requests abandoned after the timeout.
    pub discovery_timeouts: u64,
    /// Latency of the newest discovery result, in milliseconds.
    pub discovery_last_ms: Option<f64>,
    /// Newest detector error.
    pub discovery_last_error: Option<String>,
    /// Subjects admitted over the run.
    pub subjects_created: u64,
    /// Subjects retired over the run.
    pub subjects_retired: u64,
}

/// One shot in the summary.
#[derive(Debug, Clone, Serialize)]
pub struct ShotSummary {
    /// Shot id.
    pub id: String,
    /// Operator-facing name.
    pub name: String,
    /// Health.
    pub status: ShotStatus,
    /// Director quality in `0..=1`.
    pub quality: f32,
    /// Narrowest crop seen over the run, as a fraction of source width.
    pub crop_width_min: f64,
    /// Widest crop seen over the run, as a fraction of source width.
    pub crop_width_max: f64,
    /// Shortest crop seen over the run, as a fraction of source height.
    pub crop_height_min: f64,
    /// Tallest crop seen over the run, as a fraction of source height.
    pub crop_height_max: f64,
}

/// One subject in the summary, as it was at the end of the run.
#[derive(Debug, Clone, Serialize)]
pub struct SubjectSummary {
    /// Id, unique within the source.
    pub id: u32,
    /// Source id.
    pub source: String,
    /// Primary query id.
    pub query: String,
    /// Ids of the other queries it answers.
    pub aliases: Vec<String>,
    /// Tracking or lost.
    pub state: TrackState,
    /// Box `[x, y, w, h]` in normalized source coordinates.
    pub bbox: [f64; 4],
}

/// Program changes in the summary.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProgramSummary {
    /// The shot on program at the end.
    pub last: Option<String>,
    /// Program changes counted by the engine.
    pub changes: u64,
    /// Sampled changes: master-clock seconds and the new shot.
    pub log: Vec<(f64, String)>,
}

/// Director state in the summary.
#[derive(Debug, Clone, Serialize)]
pub struct DirectorSummary {
    /// Autonomy mode.
    pub mode: DirectorMode,
    /// Content profile.
    pub profile: ContentProfile,
    /// Takes requested.
    pub takes: u64,
    /// The latest reason, as text.
    pub reason: String,
}

/// Timing in the summary.
#[derive(Debug, Clone, Serialize)]
pub struct TimingSummary {
    /// Tick interval.
    pub budget_ms: f64,
    /// Moving average of the tick processing time.
    pub avg_ms: f64,
    /// Longest tick.
    pub max_ms: f64,
    /// Ticks that started late.
    pub late_ticks: u64,
    /// Ticks skipped after falling behind.
    pub skipped_ticks: u64,
}

/// The output session in the summary.
#[derive(Debug, Clone, Serialize)]
pub struct OutputSummary {
    /// Final lifecycle state.
    pub state: String,
    /// Recording path.
    pub record_path: Option<PathBuf>,
    /// RTMP URL, stream key redacted.
    pub rtmp_url: Option<String>,
    /// Program frames handed to the encoder.
    pub frames_pushed: u64,
    /// Frames written to ffmpeg.
    pub frames_written: u64,
    /// Frames dropped because the encoder queue was full.
    pub frames_dropped: u64,
    /// ffmpeg's encoder.
    pub encoder: Option<String>,
    /// ffmpeg's exit code.
    pub exit_code: Option<i32>,
    /// Newest ffmpeg error line.
    pub last_error: Option<String>,
}
