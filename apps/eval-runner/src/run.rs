//! The run: [`EngineCore`] stepped tick by tick on a [`VirtualClock`].
//!
//! # The loop
//!
//! A [`TickSchedule`] at the studio's output rate runs on the virtual
//! clock; when it says to wait, the runner sleeps on the clock, which moves
//! simulated time on by exactly that much, so tick `n` runs at media time
//! `n` times the period, never late and never skipped. Each tick:
//!
//! 1. every source's [`Input`] gives its frame for the tick's media time
//!    (`crate::inputs`), fed as a running source whose detector is ready;
//! 2. the discovery results due at this tick are handed in, in the order
//!    their requests were made;
//! 3. the core ticks; the passes it applied go to the detection log, when
//!    there is one;
//! 4. each discovery request the tick made is answered at once by the
//!    source's detector, and the answer is held back for `latency_ticks`
//!    ticks, as if a worker had taken that long, then handed in at step 2
//!    with that latency;
//! 5. the tick's trace record goes to the trace, with the schedule's word
//!    on lateness and skipped ticks (false and 0 on a virtual clock).
//!
//! After the first tick an operator puts the first available wide shot on
//! program, as the headless app does, unless the director is on auto (it
//! takes its own shots then).
//!
//! The run ends after the number of ticks `seconds` asks for, or after the
//! tick that shows the last frame of every source when every source is a
//! clip that ends, whichever comes first. A run with an endless source (a
//! synthetic one, or a looping clip) needs `seconds`.
//!
//! # Why this replays exactly
//!
//! The core is deterministic: equal frames, results and commands at equal
//! media times give equal states and so byte-equal trace records
//! (`ac_engine::core::trace`). Here all of them are functions of the
//! config, the inputs and the detector's answers, and the order of events
//! is fixed (one thread, results in request order). A replay of a run's
//! detection log answers every request of the replay, which asks about the
//! same frames at the same ticks, with exactly what the run's detector
//! answered and the core applied, so it reproduces the run tick for tick.
//! A pass missing from the log is a miss, a failed pass that changes no
//! subject; requests do not depend on what passes found, so every later
//! request is the same as before and still hits.
//!
//! # Invariants
//!
//! * The only clock is the [`VirtualClock`] passed in; the type admits no
//!   other ([`Runner::run`]), and no wall clock or timer is used anywhere
//!   in this crate (`tests/clock.rs`).
//! * One trace record per tick, in order; one log record per applied pass,
//!   in the order applied.
//! * Nothing here creates files: the caller hands in the writers.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Write};
use std::time::Duration;

use ac_core::ShotTarget;
use ac_director::DirectorMode;
use ac_engine::config::{MonitorSettings, StudioConfig};
use ac_engine::discovery::{DiscoveryRequest, DiscoveryResult};
use ac_engine::schedule::{TickDecision, TickSchedule};
use ac_engine::{EngineCommand, EngineCore, SourceFeed};
use ac_sources::{Clock, VirtualClock};
use ac_vision::VisionError;
use serde::Serialize;

use crate::detectors::{self, Detections, SourceDetector};
use crate::inputs::Input;

/// How far behind the schedule may fall before it skips ticks; never
/// reached on a virtual clock.
const MAX_LAG: Duration = Duration::from_secs(1);

/// How a run is bounded and paced.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOptions {
    /// Simulated seconds to run; `None` runs until every clip has ended.
    pub seconds: Option<f64>,
    /// Ticks from a discovery request to its answer, at least 1.
    pub latency_ticks: u32,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            seconds: None,
            latency_ticks: 1,
        }
    }
}

/// Why a run failed.
#[derive(Debug)]
pub enum RunError {
    /// The run could not start: a bad config, input, detector or option.
    Setup(String),
    /// A source could not be read after `ticks` ticks.
    Source {
        /// Ticks run (and traced) before the failure.
        ticks: u64,
        /// What went wrong.
        message: String,
    },
    /// Writing the trace or the detection log failed.
    Write(io::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(m) => write!(f, "{m}"),
            Self::Source { ticks, message } => {
                write!(f, "a source failed after {ticks} ticks: {message}")
            }
            Self::Write(e) => write!(f, "cannot write the output: {e}"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<io::Error> for RunError {
    fn from(e: io::Error) -> Self {
        Self::Write(e)
    }
}

/// What a run did, as the runner prints it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunReport {
    /// `"live"` or `"replay"`.
    pub detections: &'static str,
    /// Ticks run, one trace record each.
    pub ticks: u64,
    /// Media time of the last tick, in seconds.
    pub media_s: f64,
    /// Passes written to the detection log (0 without one).
    pub logged: u64,
    /// Passes of the replayed log for no source of the studio.
    pub foreign_passes: usize,
    /// Per source, in config order.
    pub sources: Vec<SourceReport>,
}

/// What a run did on one source.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceReport {
    /// The source's id.
    pub id: String,
    /// Discovery requests made.
    pub requests: u64,
    /// Passes the core applied.
    pub applied: u64,
    /// Requests a replay had no pass for.
    pub misses: u64,
    /// Other detector errors.
    pub errors: u64,
    /// Passes of this source in the replayed log; `None` for a live run.
    pub recorded: Option<usize>,
}

/// A run set up and ready to go; see the module docs.
pub struct Runner {
    /// The engine core.
    core: EngineCore,
    /// Each source's frames, in config order.
    inputs: Vec<Input>,
    /// Each source's detector, in config order.
    detectors: Vec<SourceDetector>,
    /// Tick period.
    period: Duration,
    /// Ticks to run; `None` until the clips end.
    limit: Option<u64>,
    /// Ticks from a request to its answer.
    latency_ticks: u64,
    /// The director takes its own shots.
    auto: bool,
    /// The report so far.
    report: RunReport,
}

impl fmt::Debug for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runner")
            .field("inputs", &self.inputs)
            .field("period", &self.period)
            .field("limit", &self.limit)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl Runner {
    /// Opens the inputs of `config` and builds its detectors as
    /// `detections` says. The studio's monitors are turned off: the trace
    /// does not show them.
    pub fn new(
        config: &StudioConfig,
        detections: &Detections,
        options: &RunOptions,
    ) -> Result<Self, RunError> {
        let setup = RunError::Setup;
        if options.latency_ticks == 0 {
            return Err(setup("the latency must be at least one tick".into()));
        }
        let mut config = config.clone();
        config.monitors = MonitorSettings::disabled();
        config.validate().map_err(|e| setup(e.to_string()))?;
        let inputs = config
            .sources
            .iter()
            .map(|e| {
                Input::open(e.source_id(), &e.input)
                    .map_err(|err| setup(format!("source {:?}: {err}", e.id)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let period = config.frame_period();
        let limit = tick_limit(options.seconds, config.output.fps, &inputs)?;
        let infos: Vec<_> = inputs.iter().map(|i| i.info().clone()).collect();
        let detectors = detectors::build(&config, &infos, detections).map_err(setup)?;
        let enabled: Vec<bool> = detectors.iter().map(Option::is_some).collect();
        let core = EngineCore::new(&config, infos, &enabled).map_err(setup)?;
        let report = RunReport {
            detections: detections.label(),
            ticks: 0,
            media_s: 0.0,
            logged: 0,
            foreign_passes: foreign_passes(&config, detections),
            sources: config
                .sources
                .iter()
                .map(|e| SourceReport {
                    id: e.id.clone(),
                    requests: 0,
                    applied: 0,
                    misses: 0,
                    errors: 0,
                    recorded: recorded(&e.id, detections),
                })
                .collect(),
        };
        Ok(Self {
            core,
            inputs,
            detectors,
            period,
            limit,
            latency_ticks: u64::from(options.latency_ticks),
            auto: config.director.mode == DirectorMode::Auto,
            report,
        })
    }

    /// Runs to the end on `clock`, writing one trace record per tick to
    /// `trace` and, given `log`, every applied pass to it, both as JSON
    /// Lines; see the module docs.
    ///
    /// Only a [`VirtualClock`] is accepted, so a run is always on
    /// simulated time:
    ///
    /// ```
    /// # use ac_engine::config::StudioConfig;
    /// # use ac_sources::{SourceConfig, VirtualClock};
    /// # use eval_runner::{Detections, RunOptions, Runner};
    /// # let input = SourceConfig::Synthetic { width: 64, height: 36, fps: 10.0, subjects: 1 };
    /// # let config = StudioConfig::with_source("cam", input);
    /// let options = RunOptions { seconds: Some(0.5), ..RunOptions::default() };
    /// let runner = Runner::new(&config, &Detections::Live, &options).unwrap();
    /// let clock = VirtualClock::new();
    /// let report = runner.run(&clock, &mut Vec::new(), None).unwrap();
    /// assert_eq!(report.ticks, 15);
    /// ```
    ///
    /// and the same run on the wall clock does not compile:
    ///
    /// ```compile_fail
    /// # use ac_engine::config::StudioConfig;
    /// # use ac_sources::{SourceConfig, WallClock};
    /// # use eval_runner::{Detections, RunOptions, Runner};
    /// # let input = SourceConfig::Synthetic { width: 64, height: 36, fps: 10.0, subjects: 1 };
    /// # let config = StudioConfig::with_source("cam", input);
    /// let options = RunOptions { seconds: Some(0.5), ..RunOptions::default() };
    /// let runner = Runner::new(&config, &Detections::Live, &options).unwrap();
    /// let clock = WallClock::new();
    /// let report = runner.run(&clock, &mut Vec::new(), None).unwrap();
    /// assert_eq!(report.ticks, 15);
    /// ```
    pub fn run(
        mut self,
        clock: &VirtualClock,
        trace: &mut dyn Write,
        mut log: Option<&mut dyn Write>,
    ) -> Result<RunReport, RunError> {
        let slack = self.period / 2;
        let mut schedule = TickSchedule::new(clock.now(), self.period, MAX_LAG);
        // Answers waiting for their tick, in request order.
        let mut pending: VecDeque<(u64, DiscoveryResult)> = VecDeque::new();
        let ids = self.core.source_ids();
        loop {
            if self.limit.is_some_and(|n| self.report.ticks >= n) {
                break;
            }
            let (index, media_time, late, skipped) = match schedule.poll(clock.now()) {
                TickDecision::Wait(d) => {
                    clock.sleep(d);
                    continue;
                }
                TickDecision::Tick {
                    index,
                    media_time,
                    late,
                    skipped,
                } => (index, media_time, late, skipped),
            };
            let mut feeds = Vec::with_capacity(self.inputs.len());
            for input in &mut self.inputs {
                let frame = input
                    .frame_at(media_time + slack)
                    .map_err(|e| RunError::Source {
                        ticks: self.report.ticks,
                        message: format!("{}: {e}", input.info().id.0),
                    })?;
                feeds.push(SourceFeed::running(frame));
            }
            let due = pending.iter().take_while(|(at, _)| *at <= index).count();
            let results = pending.drain(..due).map(|(_, r)| r).collect();
            let out = self.core.tick(media_time, &feeds, results);
            for pass in &out.applied {
                if let Some(i) = ids.iter().position(|id| *id == pass.source) {
                    self.report.sources[i].applied += 1;
                }
                if let Some(log) = log.as_deref_mut() {
                    log.write_all(pass.to_json_line().as_bytes())?;
                    self.report.logged += 1;
                }
            }
            for (i, request) in out.requests {
                let result = self.answer(i, request);
                pending.push_back((index + self.latency_ticks, result));
            }
            let mut record = self.core.trace_record();
            record.late = late;
            record.skipped = skipped;
            trace.write_all(record.to_json_line().as_bytes())?;
            self.report.ticks += 1;
            self.report.media_s = media_time.as_secs_f64();
            if self.report.ticks == 1 {
                self.take_first_wide();
            }
            if self.inputs.iter().all(Input::has_ended) {
                break;
            }
        }
        trace.flush()?;
        if let Some(log) = log {
            log.flush()?;
        }
        Ok(self.report)
    }

    /// Answers `request` of source `i` with its detector, counting misses
    /// and errors.
    fn answer(&mut self, i: usize, request: DiscoveryRequest) -> DiscoveryResult {
        let stats = &mut self.report.sources[i];
        stats.requests += 1;
        let outcome = match self.detectors.get_mut(i).and_then(Option::as_mut) {
            Some(detector) => detector.detect(&request.frame, &request.queries),
            None => Err(VisionError::Model("this source has no detector".into())),
        };
        let outcome = outcome.map_err(|e| {
            match e {
                VisionError::NotRecorded { .. } => stats.misses += 1,
                _ => stats.errors += 1,
            }
            e.to_string()
        });
        let latency = self.period * u32::try_from(self.latency_ticks).unwrap_or(u32::MAX);
        DiscoveryResult {
            source: i,
            frame: request.frame,
            queries: request.queries.into_iter().map(|q| q.id).collect(),
            outcome,
            latency,
        }
    }

    /// Puts the first available wide shot on program, unless the director
    /// takes its own shots or something is on program already.
    fn take_first_wide(&mut self) {
        if self.auto || self.core.switcher().program().is_some() {
            return;
        }
        let wide = self
            .core
            .catalog()
            .entries()
            .iter()
            .find(|e| e.available && matches!(e.shot.target, ShotTarget::Wide { .. }))
            .map(|e| e.shot.id.clone());
        if let Some(shot) = wide {
            // The shot exists and is available, so neither can be refused.
            let _ = self
                .core
                .apply(&EngineCommand::SetPreview { shot: shot.clone() });
            let _ = self.core.apply(&EngineCommand::Cut {
                preview: Some(shot),
            });
        }
    }
}

/// The number of ticks to run: `seconds` at `fps` ticks a second, rounded
/// (at least one), or `None` to run until every input has ended, which
/// needs every input to end.
fn tick_limit(seconds: Option<f64>, fps: u32, inputs: &[Input]) -> Result<Option<u64>, RunError> {
    match seconds {
        Some(s) if s.is_finite() && s > 0.0 => {
            let ticks = (s * f64::from(fps.max(1))).round().max(1.0);
            if ticks > 1e12 {
                return Err(RunError::Setup(format!("{s} seconds is too long a run")));
            }
            // A whole number of at most 1e12, so the cast is exact.
            Ok(Some(ticks as u64))
        }
        Some(s) => Err(RunError::Setup(format!(
            "the run length must be a positive number of seconds; got {s}"
        ))),
        None if inputs.iter().any(Input::is_endless) => Err(RunError::Setup(
            "a synthetic or looping source never ends, so the run needs a length in seconds".into(),
        )),
        None => Ok(None),
    }
}

/// Passes of a replayed log whose source is none of the studio's.
fn foreign_passes(config: &StudioConfig, detections: &Detections) -> usize {
    let Detections::Replay { log, .. } = detections else {
        return 0;
    };
    log.iter()
        .filter(|p| !config.sources.iter().any(|e| e.id == p.source.0))
        .count()
}

/// Passes of source `id` in a replayed log; `None` for a live run.
fn recorded(id: &str, detections: &Detections) -> Option<usize> {
    let Detections::Replay { log, .. } = detections else {
        return None;
    };
    Some(log.iter().filter(|p| p.source.0 == id).count())
}

#[cfg(test)]
mod tests;
