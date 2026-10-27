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
//!    build pays CoreML's own part of it again), or, on a sidecar, for at
//!    least its `startup_timeout_s` ([`detector_limit`]), since the window
//!    would keep preparing that long too. Timing the run from the first
//!    wide would spend a short run on the load, and ending the process
//!    mid-compile would throw the compile away, since CoreML keeps only a
//!    compile that finished; so the run waits, and fails with "still
//!    loading" rather than reporting a detector that never ran.
//! 4. **Output**: with `--record`, start the program output and wait until
//!    ffmpeg runs. The output also streams when the config sets an RTMP URL.
//! 5. **Run** for the requested time, sampling snapshots to log program
//!    changes, the largest subject count and each shot's crop size range.
//! 6. **Stop** the output and wait for the recording to be finalized, then
//!    shut the engine down, which joins every thread and reaps ffmpeg.
//!
//! A termination signal (Ctrl-C, `SIGTERM`, `SIGHUP`) ends whichever phase
//! is running; the engine is then shut down as after a normal run (which
//! also finishes a running recording and ends the detector sidecar), and
//! the summary says [`INTERRUPTED`].
//!
//! The summary ([`summary`]) is always produced; `ok` is false and `error`
//! says why when a phase failed. It reports how each query's prompt was
//! read (what the detector was asked for, the shot words' sizes, the basis
//! and any notices) and what the newest detector pass on each source found
//! for it (the match count and best score, spec M2), so a prompt that does
//! not mean what the operator thought, or that the detector does not
//! understand, shows up in a headless run too. Snapshots are sampled every
//! [`SAMPLE`] from the detector phase on, so two program changes closer
//! together than that show as one, and a crop range can miss a single
//! tick's extreme.
//!
//! This module runs the phases; the run's flags ([`HeadlessOptions`]) are
//! in `flags`, and the summary's types in [`summary`] and its building
//! from the run's snapshots in `report`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::signals::QuitSignal;
use ac_core::{Rect, ShotId};
use ac_director::{CandidateKind, DirectorMode};
use ac_engine::{
    DetectorSettings, Engine, EngineCommand, EngineHandle, EngineOptions, EngineSnapshot,
    MonitorSettings, OutputState, StudioConfig,
};

mod flags;
mod report;
mod summary;

pub use flags::HeadlessOptions;
pub use summary::{
    DirectorSummary, MatchSummary, OutputSummary, ProgramSummary, QuerySummary, ShotSummary,
    SourceSummary, SubjectSummary, Summary, TimingSummary,
};

use report::{Totals, failed_to_start, summarize};

/// Interval between snapshot samples.
const SAMPLE: Duration = Duration::from_millis(20);
/// Longest wait for a source to deliver its first frames.
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for the detectors to load, unless a sidecar's own startup
/// deadline is longer ([`detector_limit`]): a cold CoreML compile of OWLv2
/// took 140 to 176 s on an M3 Max, so this leaves room for a slower Mac.
const DETECTOR_TIMEOUT: Duration = Duration::from_secs(300);
/// Longest wait for ffmpeg to start.
const OUTPUT_START_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest wait for the recording to be finalized.
const OUTPUT_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs `config` headless on the wall clock; see the module docs.
pub fn run(
    config: StudioConfig,
    path: &Path,
    options: &HeadlessOptions,
    quit: &QuitSignal,
) -> Summary {
    run_with(config, path, options, EngineOptions::default(), quit)
}

/// [`run`] with explicit engine options (tests use a faster clock).
pub fn run_with(
    mut config: StudioConfig,
    path: &Path,
    options: &HeadlessOptions,
    engine_options: EngineOptions,
    quit: &QuitSignal,
) -> Summary {
    let started = Instant::now();
    config.monitors = MonitorSettings::disabled();
    let limit = detector_limit(&config.detector);
    let engine = match Engine::start_with(config, engine_options) {
        Ok(e) => e,
        Err(e) => return failed_to_start(path, options, started, e.to_string()),
    };
    let mut sampler = Sampler::default();
    let error = drive(&engine, options, limit, &mut sampler, quit).err();
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

/// Why a run that a termination signal cut short failed.
pub const INTERRUPTED: &str = "interrupted by a termination signal";

/// How long phase 3 may wait for the detectors of `detector`: the fixed
/// [`DETECTOR_TIMEOUT`], or for a sidecar its `startup_timeout_s` plus
/// [`SIGNAL_TIMEOUT`] when that is longer. The sidecar's deadline counts
/// from its spawn at engine start, before phase 2, so this covers it with
/// room for the loaders to hand over to the core.
fn detector_limit(detector: &DetectorSettings) -> Duration {
    match detector {
        DetectorSettings::Sidecar(s) => {
            DETECTOR_TIMEOUT.max(Duration::from_secs_f64(s.startup_timeout_s) + SIGNAL_TIMEOUT)
        }
        _ => DETECTOR_TIMEOUT,
    }
}

/// Phases 2 to 6; returns why the run failed. Phase 3 waits for the
/// detectors for at most `detector_limit`. A termination signal (`quit`)
/// ends any phase early, and the caller shuts the engine down as after a
/// normal run.
fn drive(
    engine: &Engine,
    options: &HeadlessOptions,
    detector_limit: Duration,
    sampler: &mut Sampler,
    quit: &QuitSignal,
) -> Result<(), String> {
    let h = engine.handle();
    let snap = h
        .wait_until(SIGNAL_TIMEOUT, |s| {
            quit.requested() || first_wide(s).is_some()
        })
        .ok_or_else(|| failure(&h, "no source delivered a picture in time"))?;
    if quit.requested() {
        return Err(INTERRUPTED.into());
    }
    sampler.record(&snap);
    if snap.director.mode != DirectorMode::Auto && snap.switcher.program.is_none() {
        let wide = first_wide(&snap).expect("checked above");
        h.send(EngineCommand::SetPreview { shot: wide })
            .and_then(|_| h.send(EngineCommand::Cut))
            .map_err(|e| format!("cannot put a wide on program: {e}"))?;
    }
    await_detectors(|| h.snapshot(), sampler, detector_limit, quit)?;
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
                quit.requested()
                    || !matches!(s.output.state, OutputState::Idle | OutputState::Starting)
            })
            .ok_or_else(|| failure(&h, "the output did not start in time"))?;
        if quit.requested() {
            return Err(INTERRUPTED.into());
        }
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
        if quit.requested() {
            return Err(INTERRUPTED.into());
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
/// for at most `limit` (real time), or until `quit`; see the module docs.
fn await_detectors(
    mut snapshot: impl FnMut() -> Arc<EngineSnapshot>,
    sampler: &mut Sampler,
    limit: Duration,
    quit: &QuitSignal,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let snap = snapshot();
        sampler.record(&snap);
        if !snap.running {
            return Err(failure_in(&snap, "the engine stopped early"));
        }
        if quit.requested() {
            return Err(INTERRUPTED.into());
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

#[cfg(test)]
pub(crate) mod tests;
