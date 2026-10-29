//! The JSON summary of a headless run: plain serializable data, built by
//! `summarize` in `report` from the run's snapshots.
//!
//! Every value is rounded for reading (milliseconds to one decimal,
//! normalized coordinates to four, scores to two, the program loudness and
//! peaks to 0.1 dB and the seconds they cover to two decimals) and absent
//! values are `null` (for the loudness and peaks: no reading), so a summary
//! diffed between runs shows changes in behaviour, not in float noise.
//!
//! Besides counters it carries what a run without a window needs to judge
//! the pictures: how each query's prompt was read (what the detector was
//! asked for, the framings and any notices) and what the newest detector
//! pass on each source found for it (spec M2), whether each source's
//! detector was ready and when (a run that ended while it loaded says
//! nothing about detection), each shot's crop width and height range over the run (Phase
//! 1 Milestone 1 of `/mnt/project-files/research/model-picker-design.md` §5
//! asks a headless band run for a crop-height variation of at most 10% on
//! static performers), whether each shot is zoom-limited and which shot it
//! then looks the same as, and the subjects at the end with the queries
//! they answer, so duplicate subjects and their roles can be told apart
//! without a contact sheet. An output that streams also reports how the
//! stream's connection stood while the output last ran, and how often it
//! was lost and opened again (spec M14). With audio it also carries the
//! program's loudness and peaks (spec M12), and each audio input's drift
//! correction (spec C.3; drift corrections to 0.1 ppm).

use std::path::PathBuf;

use ac_core::{Basis, ShotSize, TrackState};
use ac_director::{ContentProfile, DirectorMode};
use ac_engine::ShotStatus;
use serde::Serialize;

use super::audio_inputs::AudioInputSummary;
use super::sidecar::SidecarSummary;

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
    /// The queries and how their prompts were read.
    pub queries: Vec<QuerySummary>,
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
    /// The per-tick trace, with `--trace` and an engine that started.
    pub trace: Option<TraceSummary>,
    /// The detection log, with `--record-detections` and an engine that
    /// started.
    pub detections: Option<DetectionsSummary>,
    /// Shots that should have been on program but were drawn black.
    pub render_issues: Vec<String>,
    /// The detector sidecar, when the studio has one: its state, process,
    /// model and restarts (`sidecar` module).
    pub sidecar: Option<SidecarSummary>,
    /// The audio inputs at the end, with their drift correction; empty
    /// without any.
    pub audio_inputs: Vec<AudioInputSummary>,
}

/// One source in the summary.
#[derive(Debug, Clone, Serialize)]
pub struct SourceSummary {
    /// Source id.
    pub id: String,
    /// Health at the run's last tick: `starting`, `running`, `offline` or
    /// `reconnecting` (its input lost or not there yet, and retried), or
    /// `stopped`.
    pub state: String,
    /// What kept the source from delivering at the run's last tick (a file
    /// that is not there, a lost input); `null` while it delivered.
    pub error: Option<String>,
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
    /// Master-clock seconds at which the detector's load ended (`null`
    /// while it loaded, and without a detector). The load may have failed:
    /// then `discovery_errors` and `discovery_last_error` say why.
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

/// One query in the summary: its prompt as read when it was created.
#[derive(Debug, Clone, Serialize)]
pub struct QuerySummary {
    /// Query id.
    pub id: String,
    /// Operator-facing name.
    pub name: String,
    /// The prompt as the operator typed it.
    pub prompt: String,
    /// What the detector is asked for.
    pub detector_phrase: String,
    /// The prompt was sent as typed, without reading shot or role words.
    pub literal: bool,
    /// Shot sizes the prompt's shot words name, in typed order.
    pub shot_words: Vec<ShotSize>,
    /// Ids of the presets its subjects are framed with.
    pub presets: Vec<String>,
    /// Person or object.
    pub basis: Basis,
    /// The operator window's parse line.
    pub parse: String,
    /// The parse's notices, one sentence each.
    pub notes: Vec<String>,
    /// What the newest applied detector pass of each source found for it,
    /// in source order; sources without such a pass are left out.
    pub matches: Vec<MatchSummary>,
}

/// Prompt feedback in the summary: one source's newest applied pass.
#[derive(Debug, Clone, Serialize)]
pub struct MatchSummary {
    /// Source id.
    pub source: String,
    /// Detections returned for the query.
    pub count: usize,
    /// Those at or above the subject store's floor; only these can become
    /// subjects.
    pub kept: usize,
    /// The floor, to two decimals.
    pub floor: f64,
    /// Their best score, to two decimals; `null` when there were none.
    pub best: Option<f64>,
    /// Sequence number of the frame the pass looked at.
    pub seq: u64,
    /// That frame's PTS, in seconds.
    pub pts_seconds: f64,
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
    /// The preset's zoom ceiling keeps the shot looser than its size.
    pub limited_by_zoom: bool,
    /// Id of another shot of the same subject that this one looks the same
    /// as (both drawn at the zoom ceiling), when the engine marks one.
    pub same_picture_as: Option<String>,
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

/// The program audio's loudness and peaks in the summary (spec M12), as
/// the meter read them at the end of the session: integrated over the whole
/// session, momentary and short-term over its last 400 ms and 3 s, peaks
/// the session's largest. Values are to one decimal (Tech 3341's
/// resolution) and `null` where the meter had no reading: a window not yet
/// filled, digital silence, nothing above the gates.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoudnessSummary {
    /// Gated integrated loudness, in LUFS.
    pub integrated_lufs: Option<f64>,
    /// Momentary loudness, in LUFS.
    pub momentary_lufs: Option<f64>,
    /// Short-term loudness, in LUFS.
    pub short_term_lufs: Option<f64>,
    /// Largest sample, in dBFS.
    pub sample_peak_dbfs: Option<f64>,
    /// Largest 4× oversampled value, in dBTP.
    pub true_peak_dbtp: Option<f64>,
    /// Program audio measured, in seconds (inserted silence included).
    pub seconds: f64,
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
    /// Program loudness and peaks, when the output carried audio.
    pub loudness: Option<LoudnessSummary>,
    /// ffmpeg's encoder.
    pub encoder: Option<String>,
    /// ffmpeg's exit code.
    pub exit_code: Option<i32>,
    /// Newest ffmpeg error line.
    pub last_error: Option<String>,
    /// The stream, when the session streamed.
    pub stream: Option<StreamSummary>,
}

/// The stream in the summary: its connection as the output last saw it
/// while running, and the session's counts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamSummary {
    /// `connecting`, `connected`, `reconnecting` or `down`.
    pub state: String,
    /// The reconnection attempt under way, while reconnecting.
    pub attempt: Option<u32>,
    /// The ingest had stopped taking packets and they were being dropped.
    pub stalled: bool,
    /// Times the connection was lost over the session.
    pub outages: u32,
    /// Times it was opened again after a loss.
    pub reconnects: u32,
    /// What the ingest took over the newest two seconds of media, in
    /// kbit/s to one decimal; `null` when this ffmpeg cannot tell.
    pub bitrate_kbps: Option<f64>,
}

/// The per-tick trace in the summary.
#[derive(Debug, Clone, Serialize)]
pub struct TraceSummary {
    /// The trace file.
    pub path: PathBuf,
    /// Lines written, one per tick.
    pub records: u64,
    /// Ticks without a line: dropped because the writer fell behind, or
    /// never written after a write error. `records + missing` is `ticks`.
    pub missing: u64,
}

/// The detection log in the summary.
#[derive(Debug, Clone, Serialize)]
pub struct DetectionsSummary {
    /// The log file.
    pub path: PathBuf,
    /// Lines written, one per applied detector pass.
    pub records: u64,
    /// Applied passes without a line: dropped because the writer fell
    /// behind, or never written after a write error. `records + missing`
    /// is the sum of the sources' applied passes.
    pub missing: u64,
}
