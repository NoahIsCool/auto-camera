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
//! 3. **Detector**: when a source has discovery, wait until no source's
//!    detector is preparing, for at most [`DETECTOR_TIMEOUT`] (a cold
//!    CoreML compile of OWLv2 takes two to three minutes, and every new
//!    build pays CoreML's own part of it again). Timing the run from the
//!    first wide would spend a short run on the load, and ending the
//!    process mid-compile would throw the compile away, since CoreML keeps
//!    only a compile that finished; so the run waits, and fails with
//!    "still loading" rather than reporting a detector that never ran.
//! 4. **Output**: with `--record`, start the program output and wait until
//!    ffmpeg runs. The output also streams when the config sets an RTMP URL.
//! 5. **Run** for the requested time, sampling snapshots to log program
//!    changes, the largest subject count and each shot's crop size range.
//! 6. **Stop** the output and wait for the recording to be finalized, then
//!    shut the engine down, which joins every thread and reaps ffmpeg.
//!
//! The summary ([`summary`]) is always produced; `ok` is false and `error`
//! says why when a phase failed. Snapshots are sampled every [`SAMPLE`]
//! from the detector phase on, so two program changes closer together than
//! that show as one, and a crop range can miss a single tick's extreme.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::{Rect, ShotId};
use ac_director::{CandidateKind, ContentProfile, DirectorMode};
use ac_engine::{
    Engine, EngineCommand, EngineHandle, EngineOptions, EngineSnapshot, MonitorSettings,
    OutputState, StudioConfig,
};

mod summary;

pub use summary::{
    DirectorSummary, OutputSummary, ProgramSummary, ShotSummary, SourceSummary, SubjectSummary,
    Summary, TimingSummary,
};

/// Interval between snapshot samples.
const SAMPLE: Duration = Duration::from_millis(20);
/// Longest wait for a source to deliver its first frames.
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for the detectors to load: a cold CoreML compile of OWLv2
/// took 140 to 176 s on an M3 Max, so this leaves room for a slower Mac.
const DETECTOR_TIMEOUT: Duration = Duration::from_secs(300);
/// Longest wait for ffmpeg to start.
const OUTPUT_START_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for the recording to be finalized.
const OUTPUT_STOP_TIMEOUT: Duration = Duration::from_secs(30);

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
    failure_in(&h.snapshot(), why)
}

/// `why`, or why the engine of `snap` died when it did.
fn failure_in(snap: &EngineSnapshot, why: &str) -> String {
    match &snap.fatal {
        Some(fatal) => format!("{why}: {fatal}"),
        None => why.to_owned(),
    }
}

/// Phases 2 to 6; returns why the run failed.
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
    await_detectors(|| h.snapshot(), sampler, DETECTOR_TIMEOUT)?;
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

/// Phase 3: samples `snapshot` until no source's detector is preparing,
/// for at most `limit` (real time); see the module docs.
fn await_detectors(
    mut snapshot: impl FnMut() -> Arc<EngineSnapshot>,
    sampler: &mut Sampler,
    limit: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let snap = snapshot();
        sampler.record(&snap);
        if !snap.running {
            return Err(failure_in(&snap, "the engine stopped early"));
        }
        if !snap.sources.iter().any(|s| s.discovery.preparing) {
            return Ok(());
        }
        if started.elapsed() >= limit {
            return Err(format!(
                "the detector was still loading after {} s",
                limit.as_secs_f64()
            ));
        }
        std::thread::sleep(SAMPLE);
    }
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
    /// Crop size range of every shot seen, by shot id.
    crops: HashMap<String, CropRange>,
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
        for shot in &snap.shots {
            self.crops
                .entry(shot.id.0.clone())
                .and_modify(|r| r.widen(shot.crop))
                .or_insert_with(|| CropRange::new(shot.crop));
        }
        self.last = Some(snap.clone());
    }
}

/// The smallest and largest crop width and height a shot was seen with.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CropRange {
    /// Narrowest and widest, as fractions of source width.
    width: [f64; 2],
    /// Shortest and tallest, as fractions of source height.
    height: [f64; 2],
}

impl CropRange {
    /// The range of a shot seen once, with `crop`.
    fn new(crop: Rect) -> Self {
        Self {
            width: [crop.w, crop.w],
            height: [crop.h, crop.h],
        }
    }

    /// Takes in another sighting, with `crop`.
    fn widen(&mut self, crop: Rect) {
        self.width = [self.width[0].min(crop.w), self.width[1].max(crop.w)];
        self.height = [self.height[0].min(crop.h), self.height[1].max(crop.h)];
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
/// director), the final one (output and counters, including whether a
/// detector was still preparing when the run ended) and the samples (crop
/// ranges).
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
                    discovery_preparing: v.discovery.preparing,
                    detector_ready_s: v.discovery.ready_after.map(|d| round(d.as_secs_f64(), 2)),
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
                .map(|v| {
                    // A shot only in the final snapshot was never sampled.
                    let crop = sampler
                        .crops
                        .get(&v.id.0)
                        .copied()
                        .unwrap_or_else(|| CropRange::new(v.crop));
                    ShotSummary {
                        id: v.id.0.clone(),
                        name: v.name.clone(),
                        status: v.status,
                        quality: (v.quality * 100.0).round() / 100.0,
                        crop_width_min: round(crop.width[0], 4),
                        crop_width_max: round(crop.width[1], 4),
                        crop_height_min: round(crop.height[0], 4),
                        crop_height_max: round(crop.height[1], 4),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let subjects = last
        .map(|s| {
            s.subjects
                .iter()
                .map(|v| SubjectSummary {
                    id: v.id.0,
                    source: v.source.0.clone(),
                    query: v.query.0.clone(),
                    aliases: v.aliases.iter().map(|q| q.0.clone()).collect(),
                    state: v.state,
                    bbox: [v.bbox.x, v.bbox.y, v.bbox.w, v.bbox.h].map(|c| round(c, 4)),
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
        subjects,
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
