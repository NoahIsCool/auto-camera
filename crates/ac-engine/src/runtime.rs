//! The engine runtime: threads, channels, mailboxes and the master clock.
//!
//! [`Engine::start`] opens every source, builds one detector per source
//! ([`Detectors::build`]; a sidecar detector starts its one shared process
//! here), and spawns:
//!
//! * the **engine thread** (`ac-engine`), which owns the [`EngineCore`],
//!   the sources, the discovery workers' inboxes, the sidecar service (if
//!   any) and the [`OutputController`], and runs the master clock: one tick
//!   per output frame on the injected [`Clock`] ([`TickSchedule`]);
//! * one **discovery worker** per source with a detector
//!   (`ac-discovery-<source>`);
//! * whatever the sources, the sidecar service and the encoder spawn
//!   internally (decoders, the sidecar process and its `ac-sam-*` threads,
//!   ffmpeg pipes), owned by those objects.
//!
//! Communication is by message passing only:
//!
//! | From → to | Channel | Policy |
//! |---|---|---|
//! | source → engine | [`ac_media::LatestFrame`] inside the source | newest frame only |
//! | engine → worker | [`Slot`](crate::mailbox::Slot) per worker | newest request only |
//! | worker → engine | `mpsc` channel | drained every tick; stale results dropped |
//! | caller → engine | `mpsc` channel of [`EngineCommand`] + reply channel | applied in order at tick start |
//! | engine → callers | [`Latest`]`<Arc<EngineSnapshot>>` | newest snapshot only |
//! | engine → encoder | bounded queues inside [`ac_output::ProgramEncoder`] | drop and repay |
//!
//! No lock is shared between threads except inside these mailboxes, and none
//! is held across I/O. [`Engine::shutdown`] (or dropping the [`Engine`])
//! sends [`EngineCommand::Shutdown`]; the engine thread then stops the
//! sidecar (interrupting a pass or a load in progress, so the workers end
//! at once; see [`crate::detectors`]), stops and joins the workers,
//! finishes the encoder (the recording is finalized), stops every source,
//! publishes a final snapshot with `running == false` and exits, and
//! `shutdown` joins it. No thread or child process outlives the engine.
//!
//! The engine thread can also die: a bug that panics anywhere in a tick or
//! a command unwinds it. Unwinding drops what the thread owned (sources
//! stop, the encoder is finished), and the thread then publishes a last
//! snapshot with `running == false` and [`EngineSnapshot::fatal`] saying
//! why, before the panic continues to [`Engine::shutdown`] (which reports
//! [`EngineError::Panicked`]). Every reader therefore learns that the engine
//! is gone from the snapshots alone: the operator window shows it,
//! [`EngineHandle::wait_until`] returns at once and a headless run fails,
//! instead of watching a frozen "running" snapshot forever.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ac_media::{SourceInfo, VideoSource};
use ac_output::AudioFormat;
use ac_sidecar::SidecarService;
use ac_sources::{Clock, WallClock, open_source_with_clock};
use thiserror::Error;

use crate::command::{CommandError, CommandReply, EngineCommand};
use crate::config::{ConfigError, StudioConfig};
use crate::core::{EngineCore, SourceFeed};
use crate::detectors::Detectors;
use crate::discovery::{DiscoveryResult, DiscoveryWorker};
use crate::mailbox::Latest;
use crate::output::OutputController;
use crate::schedule::{PhaseLock, TickDecision, TickSchedule, TickTimer};
use crate::snapshot::{EngineSnapshot, OutputState, OutputView, TimingView};

mod handle;

use handle::Envelope;
pub use handle::{EngineHandle, PendingReply};

/// Longest single wait of the engine thread between ticks.
const WAIT_SLICE: Duration = Duration::from_millis(10);
/// How long [`EngineHandle::send`] waits for the engine's answer.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors from starting or stopping an engine.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A source could not be opened.
    #[error("source {id}: {message}")]
    Source {
        /// The source id.
        id: String,
        /// Why.
        message: String,
    },
    /// A detector could not be built.
    #[error("detector for source {id}: {message}")]
    Detector {
        /// The source id.
        id: String,
        /// Why.
        message: String,
    },
    /// The shared detector sidecar could not be started (a program or path
    /// in its command is missing, or the process cannot be spawned).
    #[error("detector sidecar: {0}")]
    Sidecar(String),
    /// The core rejected the setup.
    #[error("engine setup: {0}")]
    Setup(String),
    /// A thread could not be spawned.
    #[error("cannot spawn thread: {0}")]
    Thread(#[from] std::io::Error),
    /// The engine thread panicked.
    #[error("the engine thread panicked")]
    Panicked,
}

/// How an engine is run.
#[derive(Clone)]
pub struct EngineOptions {
    /// Master clock, also used to pace the sources. [`WallClock`] for real
    /// time. A faster clock runs the identical code sooner, but discovery
    /// still runs in real time, so results older than the configured maximum
    /// age (in media time) are dropped.
    pub clock: Arc<dyn Clock>,
    /// Falling further behind than this skips ticks instead of bursting.
    pub max_lag: Duration,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            clock: Arc::new(WallClock::new()),
            max_lag: Duration::from_secs(1),
        }
    }
}

impl std::fmt::Debug for EngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineOptions")
            .field("max_lag", &self.max_lag)
            .finish_non_exhaustive()
    }
}

/// What the engine thread reports when it ends.
#[derive(Debug, Clone)]
pub struct EngineReport {
    /// Ticks run.
    pub ticks: u64,
    /// Threads joined by the engine thread (workers and output helpers).
    pub threads_joined: usize,
    /// The final snapshot.
    pub final_snapshot: Arc<EngineSnapshot>,
}

/// A running engine; see the module docs.
pub struct Engine {
    /// Handle shared with callers.
    handle: EngineHandle,
    /// The engine thread.
    thread: Option<JoinHandle<EngineReport>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("running", &self.thread.is_some())
            .finish_non_exhaustive()
    }
}

/// Everything the engine thread owns.
struct Runtime {
    /// The deterministic core.
    core: EngineCore,
    /// Sources, in config order.
    sources: Vec<Box<dyn VideoSource>>,
    /// The detector sidecar the workers share, if any. Declared before the
    /// workers so that, when a panic unwinds the thread, it is dropped
    /// first too (see [`Runtime::shutdown`]).
    sidecar: Option<SidecarService>,
    /// Discovery workers, by source.
    workers: Vec<Option<DiscoveryWorker>>,
    /// Discovery results.
    results: Receiver<DiscoveryResult>,
    /// Commands.
    commands: Receiver<Envelope>,
    /// Snapshot mailbox.
    snapshots: Arc<Latest<Arc<EngineSnapshot>>>,
    /// Program output.
    output: OutputController,
    /// Source whose audio is program audio.
    audio_source: Option<usize>,
    /// Format of that audio.
    audio_format: Option<AudioFormat>,
    /// Master clock.
    clock: Arc<dyn Clock>,
    /// Tick interval.
    period: Duration,
    /// Lag before skipping.
    max_lag: Duration,
    /// Tick processing time.
    timer: TickTimer,
    /// Late ticks.
    late: u64,
    /// Skipped ticks.
    skipped: u64,
}

impl Engine {
    /// Starts an engine for `config` in real time.
    pub fn start(config: StudioConfig) -> Result<Self, EngineError> {
        Self::start_with(config, EngineOptions::default())
    }

    /// Starts an engine for `config` with `options`.
    pub fn start_with(config: StudioConfig, options: EngineOptions) -> Result<Self, EngineError> {
        config.validate()?;
        let mut sources: Vec<Box<dyn VideoSource>> = Vec::new();
        for entry in &config.sources {
            let src =
                open_source_with_clock(&entry.input, entry.source_id(), options.clock.clone())
                    .map_err(|e| EngineError::Source {
                        id: entry.id.clone(),
                        message: e.to_string(),
                    })?;
            sources.push(src);
        }
        let Detectors {
            loaders: detectors,
            sidecar,
        } = Detectors::build(&config)?;
        let infos: Vec<SourceInfo> = sources.iter().map(|s| s.info().clone()).collect();
        let enabled: Vec<bool> = detectors.iter().map(Option::is_some).collect();
        let core = EngineCore::new(&config, infos.clone(), &enabled).map_err(EngineError::Setup)?;
        let (results_tx, results) = channel();
        let mut workers = Vec::new();
        for ((entry, detector), i) in config.sources.iter().zip(detectors).zip(0..) {
            workers.push(match detector {
                Some(d) => Some(DiscoveryWorker::spawn(i, &entry.id, d, results_tx.clone())?),
                None => None,
            });
        }
        let audio_source = config
            .output
            .audio_source
            .as_ref()
            .and_then(|id| config.sources.iter().position(|s| &s.id == id));
        let audio_format =
            audio_source
                .and_then(|i| infos[i].audio)
                .map(|(sample_rate, channels)| AudioFormat {
                    sample_rate,
                    channels,
                });
        let snapshots = Arc::new(Latest::new());
        let initial = Arc::new(core.snapshot(
            OutputView::default(),
            TimingView {
                budget_ms: config.frame_period().as_secs_f64() * 1e3,
                ..TimingView::default()
            },
            true,
        ));
        snapshots.publish(initial.clone());
        let (tx, commands) = channel();
        let runtime = Runtime {
            core,
            sources,
            sidecar,
            workers,
            results,
            commands,
            snapshots: snapshots.clone(),
            output: OutputController::new(config.output.clone()),
            audio_source,
            audio_format,
            clock: options.clock,
            period: config.frame_period(),
            max_lag: options.max_lag,
            timer: TickTimer::default(),
            late: 0,
            skipped: 0,
        };
        let last_words = snapshots.clone();
        let thread =
            std::thread::Builder::new().name("ac-engine".into()).spawn(
                move || match catch_unwind(AssertUnwindSafe(|| runtime.run())) {
                    Ok(report) => report,
                    Err(payload) => {
                        // `runtime` was dropped while unwinding: sources are
                        // stopped and the encoder finished. Say so, then let the
                        // panic reach `join`.
                        publish_fatal(&last_words, &panic_message(payload.as_ref()));
                        resume_unwind(payload)
                    }
                },
            )?;
        Ok(Self {
            handle: EngineHandle {
                tx,
                snapshots,
                initial,
            },
            thread: Some(thread),
        })
    }

    /// A handle for commands and snapshots.
    pub fn handle(&self) -> EngineHandle {
        self.handle.clone()
    }

    /// The newest snapshot.
    pub fn snapshot(&self) -> Arc<EngineSnapshot> {
        self.handle.snapshot()
    }

    /// Sends `command` and waits for the answer.
    pub fn send(&self, command: EngineCommand) -> Result<CommandReply, CommandError> {
        self.handle.send(command)
    }

    /// Stops the engine and joins its thread.
    pub fn shutdown(mut self) -> Result<EngineReport, EngineError> {
        self.stop_thread()
    }

    /// Sends shutdown (also when every handle is gone) and joins.
    fn stop_thread(&mut self) -> Result<EngineReport, EngineError> {
        let Some(thread) = self.thread.take() else {
            return Err(EngineError::Setup("the engine was already stopped".into()));
        };
        let _ = self.handle.tx.send(Envelope {
            command: EngineCommand::Shutdown,
            reply: None,
        });
        thread.join().map_err(|_| EngineError::Panicked)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.stop_thread();
        }
    }
}

/// The text of a panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Publishes the last snapshot of an engine thread that panicked: the newest
/// one, marked stopped, with the reason and the output no longer running.
fn publish_fatal(snapshots: &Latest<Arc<EngineSnapshot>>, message: &str) {
    let Some(last) = snapshots.get() else {
        return;
    };
    let mut snap = (*last).clone();
    snap.running = false;
    snap.fatal = Some(format!("the engine thread panicked: {message}"));
    if matches!(
        snap.output.state,
        OutputState::Starting | OutputState::Running | OutputState::Stopping
    ) {
        snap.output.state = OutputState::Failed("the engine stopped".into());
    }
    snapshots.publish(Arc::new(snap));
}

impl Runtime {
    /// The engine thread's main loop.
    fn run(mut self) -> EngineReport {
        let mut schedule = TickSchedule::new(self.clock.now(), self.period, self.max_lag);
        let mut phase = PhaseLock::new(self.period);
        loop {
            let (media_time, late, skipped) = match schedule.poll(self.clock.now()) {
                TickDecision::Wait(d) => {
                    self.clock.sleep(d.min(WAIT_SLICE));
                    continue;
                }
                TickDecision::Tick {
                    media_time,
                    late,
                    skipped,
                    ..
                } => (media_time, late, skipped),
            };
            let started = Instant::now();
            if self.apply_commands() {
                break;
            }
            let reference = self.tick(media_time, skipped);
            if let Some(by) = phase.observe(reference) {
                schedule.delay(by);
            }
            self.timer.record(started.elapsed());
            self.late += u64::from(late);
            self.skipped += skipped;
            self.publish(true);
        }
        self.shutdown()
    }

    /// Applies pending commands; true when the engine must stop.
    fn apply_commands(&mut self) -> bool {
        loop {
            let envelope = match self.commands.try_recv() {
                Ok(e) => e,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
            };
            let stop = envelope.command == EngineCommand::Shutdown;
            let result = match &envelope.command {
                EngineCommand::Shutdown => Ok(CommandReply::Done),
                EngineCommand::StartOutput {
                    rtmp_url,
                    record_path,
                } => self
                    .output
                    .start(rtmp_url.clone(), record_path.clone(), self.audio_format),
                EngineCommand::StopOutput => self.output.stop(),
                other => self.core.apply(other),
            };
            if let Some(reply) = envelope.reply {
                let _ = reply.send(result);
            }
            if stop {
                return true;
            }
        }
    }

    /// One tick: feed the core, dispatch discovery, push the program
    /// (owing the encoder a repeat for each of the `skipped` ticks before).
    /// Returns the sequence number and PTS of the frame the tick read from
    /// the phase reference (the program audio source, else the first
    /// source), for the [`PhaseLock`].
    fn tick(&mut self, media_time: Duration, skipped: u64) -> Option<(u64, Duration)> {
        self.output.poll();
        let results: Vec<DiscoveryResult> = self.results.try_iter().collect();
        let feeds: Vec<SourceFeed> = self
            .sources
            .iter()
            .map(|s| SourceFeed {
                state: s.state(),
                frame: s.latest(),
            })
            .collect();
        let reference = feeds
            .get(self.audio_source.unwrap_or(0))
            .and_then(|feed| feed.frame.as_ref())
            .map(|frame| (frame.seq, frame.pts));
        let out = self.core.tick(media_time, &feeds, results);
        for (i, request) in out.requests {
            if let Some(Some(worker)) = self.workers.get(i) {
                worker.submit(request);
            }
        }
        let mut program_audio = Vec::new();
        for (i, s) in self.sources.iter().enumerate() {
            let blocks = s.drain_audio();
            if Some(i) == self.audio_source {
                program_audio = blocks;
            }
        }
        self.output.push(&out.program, program_audio, skipped);
        reference
    }

    /// Publishes the current snapshot and returns it.
    fn publish(&self, running: bool) -> Arc<EngineSnapshot> {
        let timing = TimingView {
            tick: self.timer,
            budget_ms: self.period.as_secs_f64() * 1e3,
            late_ticks: self.late,
            skipped_ticks: self.skipped,
        };
        let snapshot = Arc::new(self.core.snapshot(self.output.view(), timing, running));
        self.snapshots.publish(snapshot.clone());
        snapshot
    }

    /// Stops the sidecar, the workers, the output and the sources, and
    /// publishes the final snapshot. The sidecar goes first: dropping its
    /// service interrupts a pass or a load in progress, so the workers can
    /// be joined at once instead of after a request deadline.
    fn shutdown(mut self) -> EngineReport {
        let mut joined = 0;
        if let Some(service) = self.sidecar.take() {
            // Ends the process (stdin closed, 2 s grace, kill, reap) and
            // joins the service thread.
            drop(service);
            joined += 1;
        }
        for w in self.workers.iter_mut().flatten() {
            joined += usize::from(w.stop());
        }
        joined += self.output.shutdown();
        for s in &mut self.sources {
            s.stop();
        }
        EngineReport {
            ticks: self.core.ticks(),
            threads_joined: joined,
            final_snapshot: self.publish(false),
        }
    }
}
