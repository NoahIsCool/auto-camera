//! Headless runs: the engine for a fixed time, optionally recording, with a
//! JSON summary.
//!
//! A run goes through these phases, each with its own time limit so a broken
//! setup fails with a message instead of hanging:
//!
//! 1. **Start** the engine on the wall clock with monitors and multiview
//!    tiles off (nobody is watching).
//! 2. **Signal**: wait for a wide shot to become available. A headless run
//!    has no operator, so unless the director is in Auto mode (which
//!    establishes a shot by itself) the first wide is cut to program.
//! 3. **Output**: with `--record`, start the program output and wait until
//!    ffmpeg runs. The output also streams when the config sets an RTMP URL.
//! 4. **Run** for the requested time, sampling snapshots to log program
//!    changes and the largest subject count.
//! 5. **Stop** the output and wait for the recording to be finalized, then
//!    shut the engine down, which joins every thread and reaps ffmpeg.
//!
//! The summary is always produced; `ok` is false and `error` says why when a
//! phase failed. Program changes are logged from samples taken every
//! [`SAMPLE`], so two changes closer together than that show as one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::ShotId;
use ac_director::{CandidateKind, ContentProfile, DirectorMode};
use ac_engine::{
    Engine, EngineCommand, EngineHandle, EngineOptions, EngineSnapshot, MonitorSettings,
    OutputState, ShotStatus, StudioConfig,
};
use serde::Serialize;

/// Interval between snapshot samples.
const SAMPLE: Duration = Duration::from_millis(20);
/// Longest wait for a source to deliver its first frames.
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for ffmpeg to start.
const OUTPUT_START_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for the recording to be finalized.
const OUTPUT_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// What a headless run does.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadlessOptions {
    /// Length of the run, counted from when recording started (or from when
    /// the first shot was available, without a recording).
    pub seconds: f64,
    /// Program recording path; must not exist.
    pub record: Option<PathBuf>,
}

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

/// Runs `config` headless on the wall clock; see the module docs.
pub fn run(config: StudioConfig, path: &Path, options: &HeadlessOptions) -> Summary {
    run_with(config, path, options, EngineOptions::default())
}

/// [`run`] with explicit engine options (tests use a faster clock).
pub fn run_with(
    mut config: StudioConfig,
    path: &Path,
    options: &HeadlessOptions,
    engine_options: EngineOptions,
) -> Summary {
    let started = Instant::now();
    config.monitors = MonitorSettings::disabled();
    let engine = match Engine::start_with(config, engine_options) {
        Ok(e) => e,
        Err(e) => return failed_to_start(path, options, started, e.to_string()),
    };
    let mut sampler = Sampler::default();
    let error = drive(&engine, options, &mut sampler).err();
    let (ticks, threads_joined, final_snapshot, error) = match engine.shutdown() {
        Ok(r) => (r.ticks, r.threads_joined, Some(r.final_snapshot), error),
        Err(e) => (0, 0, None, error.or(Some(e.to_string()))),
    };
    let last = sampler.last.clone().or(final_snapshot.clone());
    summarize(
        path,
        options,
        started,
        error,
        Totals {
            ticks,
            threads_joined,
        },
        last.as_deref(),
        final_snapshot.as_deref(),
        &sampler,
    )
}

/// `why`, or why the engine died when it did (a dead engine also makes
/// every wait end early).
fn failure(h: &EngineHandle, why: &str) -> String {
    match &h.snapshot().fatal {
        Some(fatal) => format!("{why}: {fatal}"),
        None => why.to_owned(),
    }
}

/// Phases 2 to 5; returns why the run failed.
fn drive(engine: &Engine, options: &HeadlessOptions, sampler: &mut Sampler) -> Result<(), String> {
    let h = engine.handle();
    let snap = h
        .wait_until(SIGNAL_TIMEOUT, |s| first_wide(s).is_some())
        .ok_or_else(|| failure(&h, "no source delivered a picture in time"))?;
    sampler.record(&snap);
    if snap.director.mode != DirectorMode::Auto && snap.switcher.program.is_none() {
        let wide = first_wide(&snap).expect("checked above");
        h.send(EngineCommand::SetPreview { shot: wide })
            .and_then(|_| h.send(EngineCommand::Cut))
            .map_err(|e| format!("cannot put a wide on program: {e}"))?;
    }
    if let Some(record) = &options.record {
        h.send(EngineCommand::StartOutput {
            rtmp_url: None,
            record_path: Some(record.clone()),
        })
        .map_err(|e| format!("cannot start the output: {e}"))?;
        // Snapshots published before the command was applied still say
        // Idle, so wait for a state the start leads to.
        let snap = h
            .wait_until(OUTPUT_START_TIMEOUT, |s| {
                !matches!(s.output.state, OutputState::Idle | OutputState::Starting)
            })
            .ok_or_else(|| failure(&h, "the output did not start in time"))?;
        if snap.output.state != OutputState::Running {
            return Err(format!("the output did not start: {:?}", snap.output.state));
        }
    }
    let end = Instant::now() + Duration::from_secs_f64(options.seconds);
    while Instant::now() < end {
        std::thread::sleep(SAMPLE.min(end.saturating_duration_since(Instant::now())));
        let snap = h.snapshot();
        sampler.record(&snap);
        if !snap.running {
            return Err(failure(&h, "the engine stopped early"));
        }
        if matches!(snap.output.state, OutputState::Failed(_)) {
            return Err(format!("the output failed: {:?}", snap.output.state));
        }
    }
    if options.record.is_some() {
        h.send(EngineCommand::StopOutput)
            .map_err(|e| format!("cannot stop the output: {e}"))?;
        let snap = h
            .wait_until(OUTPUT_STOP_TIMEOUT, |s| {
                !matches!(s.output.state, OutputState::Running | OutputState::Stopping)
            })
            .ok_or_else(|| failure(&h, "the recording was not finalized in time"))?;
        sampler.record(&snap);
        if snap.output.state != OutputState::Finished {
            return Err(format!("the output failed: {:?}", snap.output.state));
        }
    }
    Ok(())
}

/// The first available wide shot.
fn first_wide(snap: &EngineSnapshot) -> Option<ShotId> {
    snap.shots
        .iter()
        .find(|s| s.kind == CandidateKind::Wide && s.available)
        .map(|s| s.id.clone())
}

/// What the run loop learns from the snapshots it samples.
#[derive(Debug, Default)]
struct Sampler {
    /// Newest running snapshot.
    last: Option<Arc<EngineSnapshot>>,
    /// Largest subject count.
    max_subjects: usize,
    /// Program changes seen.
    log: Vec<(f64, String)>,
}

impl Sampler {
    /// Takes in one snapshot.
    fn record(&mut self, snap: &Arc<EngineSnapshot>) {
        if !snap.running {
            return;
        }
        self.max_subjects = self.max_subjects.max(snap.subjects.len());
        if let Some(p) = &snap.switcher.program
            && self.log.last().is_none_or(|(_, last)| *last != p.0)
        {
            self.log.push((snap.media_time.as_secs_f64(), p.0.clone()));
        }
        self.last = Some(snap.clone());
    }
}

/// Counters from the engine report.
struct Totals {
    /// Ticks run.
    ticks: u64,
    /// Threads joined.
    threads_joined: usize,
}

/// The summary of a run whose engine never started.
fn failed_to_start(
    path: &Path,
    options: &HeadlessOptions,
    started: Instant,
    error: String,
) -> Summary {
    summarize(
        path,
        options,
        started,
        Some(format!("the engine did not start: {error}")),
        Totals {
            ticks: 0,
            threads_joined: 0,
        },
        None,
        None,
        &Sampler::default(),
    )
}

/// Builds the summary from the last running snapshot (shots, subjects and
/// director) and the final one (output and counters).
#[allow(clippy::too_many_arguments)]
fn summarize(
    path: &Path,
    options: &HeadlessOptions,
    started: Instant,
    error: Option<String>,
    totals: Totals,
    last: Option<&EngineSnapshot>,
    fin: Option<&EngineSnapshot>,
    sampler: &Sampler,
) -> Summary {
    let end = fin.or(last);
    let sources = end
        .map(|s| {
            s.sources
                .iter()
                .map(|v| SourceSummary {
                    id: v.id.0.clone(),
                    size: [v.width, v.height],
                    fps: round(v.fps, 2),
                    frames: v.frames,
                    missed: v.missed,
                    discovery_requests: v.discovery.requests,
                    discovery_applied: v.discovery.applied,
                    discovery_stale: v.discovery.stale,
                    discovery_errors: v.discovery.errors,
                    discovery_timeouts: v.discovery.timeouts,
                    discovery_last_ms: v
                        .discovery
                        .last_latency
                        .map(|d| round(d.as_secs_f64() * 1e3, 1)),
                    discovery_last_error: v.discovery.last_error.clone(),
                    subjects_created: v.vision.created,
                    subjects_retired: v.vision.retired,
                })
                .collect()
        })
        .unwrap_or_default();
    let shots = last
        .map(|s| {
            s.shots
                .iter()
                .map(|v| ShotSummary {
                    id: v.id.0.clone(),
                    name: v.name.clone(),
                    status: v.status,
                    quality: (v.quality * 100.0).round() / 100.0,
                })
                .collect()
        })
        .unwrap_or_default();
    let director = last.map_or(
        DirectorSummary {
            mode: DirectorMode::Off,
            profile: ContentProfile::Talk,
            takes: 0,
            reason: String::new(),
        },
        |s| DirectorSummary {
            mode: s.director.mode,
            profile: s.director.profile,
            takes: s.director.takes,
            reason: s.director.reason_text.clone(),
        },
    );
    let timing = end.map(|s| s.timing).unwrap_or_default();
    let output = options.record.as_ref().and(end).map(|s| {
        let o = &s.output;
        OutputSummary {
            state: format!("{:?}", o.state).to_lowercase(),
            record_path: o.record_path.clone(),
            rtmp_url: o.rtmp_url.clone(),
            frames_pushed: o.frames_pushed,
            frames_written: o.stats.as_ref().map_or(0, |st| st.frames_written),
            frames_dropped: o.stats.as_ref().map_or(0, |st| st.frames_dropped),
            encoder: o
                .stats
                .as_ref()
                .map(|st| st.encoder.ffmpeg_name().to_owned()),
            exit_code: o.stats.as_ref().and_then(|st| st.exit_code),
            last_error: o.stats.as_ref().and_then(|st| st.last_error.clone()),
        }
    });
    Summary {
        ok: error.is_none(),
        error,
        config: path.to_owned(),
        seconds: options.seconds,
        wall_seconds: round(started.elapsed().as_secs_f64(), 2),
        ticks: totals.ticks,
        media_seconds: end.map_or(0.0, |s| round(s.media_time.as_secs_f64(), 3)),
        threads_joined: totals.threads_joined,
        sources,
        max_subjects: sampler.max_subjects,
        shots,
        program: ProgramSummary {
            last: last.and_then(|s| s.switcher.program.as_ref().map(|p| p.0.clone())),
            changes: end.map_or(0, |s| s.program_changes),
            log: sampler
                .log
                .iter()
                .map(|(t, s)| (round(*t, 2), s.clone()))
                .collect(),
        },
        director,
        timing: TimingSummary {
            budget_ms: round(timing.budget_ms, 2),
            avg_ms: round(timing.tick.avg_ms, 2),
            max_ms: round(timing.tick.max_ms, 2),
            late_ticks: timing.late_ticks,
            skipped_ticks: timing.skipped_ticks,
        },
        output,
        render_issues: end.map(|s| s.render_issues.clone()).unwrap_or_default(),
    }
}

/// Rounds `v` to `digits` decimals for a readable summary.
fn round(v: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    (v * scale).round() / scale
}

#[cfg(test)]
mod tests;
