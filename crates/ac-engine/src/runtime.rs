//! The engine runtime: threads, channels, mailboxes and the master clock.
//!
//! [`Engine::start`] opens every source, builds one detector per source
//! ([`Detectors::build`]; a sidecar detector starts its one shared process
//! here), and spawns:
//!
//! * one **supervisor** per source (`ac-supervise-<source>`, see
//!   `ac_sources::supervisor`), which owns the source's current connection
//!   and opens it again, with a backoff of 0.5 s doubling to 8 s, whenever
//!   it fails;
//! * the **engine thread** (`ac-engine`), which owns the [`EngineCore`],
//!   the sources, the discovery workers' inboxes, the sidecar service (if
//!   any) and the [`OutputController`], and runs the master clock: one tick
//!   per output frame on the injected [`Clock`]
//!   ([`TickSchedule`](crate::schedule::TickSchedule));
//! * one **discovery worker** per source with a detector
//!   (`ac-discovery-<source>`);
//! * whatever the sources, the sidecar service and the encoder spawn
//!   internally (decoders, the sidecar process and its `ac-sam-*` threads,
//!   ffmpeg pipes), owned by those objects.
//!
//! A source that cannot be opened at start (a file that is not there yet)
//! does not stop the engine: it starts offline, described by its
//! placeholder ([`crate::config::SourceEntry::placeholder_info`]), its
//! snapshot says why ([`crate::SourceView::error`]), and its supervisor
//! keeps trying, so it goes on air once the input appears. The other
//! sources run meanwhile. Once it delivers, its description is the input's
//! own (`VideoSource::current_info`): the core takes its size and rate,
//! and an output started from then on carries its audio when it is the
//! program audio source (an output already running keeps the format it
//! started with). Only a configuration mistake that no retry can mend
//! fails the start ([`StudioConfig::validate`]).
//!
//! Communication is by message passing only:
//!
//! | From → to | Channel | Policy |
//! |---|---|---|
//! | source → engine | [`ac_media::LatestFrame`] inside the source | newest frame only |
//! | engine → worker | [`Slot`](crate::mailbox::Slot) per worker | newest request only |
//! | worker → engine | `mpsc` channel | drained every tick; stale results dropped |
//! | worker → engine | `mpsc` channel per worker | one "loaded" message, read every tick |
//! | caller → engine | `mpsc` channel of [`EngineCommand`] + reply channel | applied in order at tick start |
//! | engine → callers | [`Latest`]`<Arc<EngineSnapshot>>` | newest snapshot only |
//! | engine → encoder | bounded queues inside [`ac_output::ProgramEncoder`] | drop and repay |
//! | engine → trace writer | bounded [`TraceSender`] of [`EngineOptions::trace`] | never waits: a full channel drops the record ([`crate::trace`]) |
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
//!
//! This module starts and stops the engine and owns the state the engine
//! thread is given; the loop that thread runs is in the private
//! `engine_thread` module, and the callers' side, [`EngineHandle`], in
//! `handle`.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_media::{SourceInfo, VideoSource};
use ac_sidecar::SidecarService;
use ac_sources::{Backoff, Clock, WallClock, open_supervised};
use thiserror::Error;

use crate::command::{CommandError, CommandReply, EngineCommand};
use crate::config::{ConfigError, StudioConfig};
use crate::core::EngineCore;
use crate::detectors::Detectors;
use crate::discovery::{DiscoveryResult, DiscoveryWorker};
use crate::mailbox::Latest;
use crate::output::OutputController;
use crate::schedule::TickTimer;
use crate::snapshot::{EngineSnapshot, OutputView, TimingView};
use crate::trace::{TraceSender, Tracer};

mod engine_thread;
mod handle;

use engine_thread::{panic_message, publish_fatal};
use handle::Envelope;
pub use handle::{EngineHandle, PendingReply};

/// How long [`EngineHandle::send`] waits for the engine's answer.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors from starting or stopping an engine.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A source's supervisor could not be started (a source that cannot be
    /// opened yet starts offline instead).
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
    /// Where the engine thread sends one trace record per tick
    /// ([`crate::trace`]); `None` for no trace. The engine's thread owns
    /// this sender, so the channel closes when the engine stops, unless a
    /// clone of these options still holds one.
    pub trace: Option<TraceSender>,
}

impl Default for EngineOptions {
    /// Real time, a one-second lag limit and no trace.
    fn default() -> Self {
        Self {
            clock: Arc::new(WallClock::new()),
            max_lag: Duration::from_secs(1),
            trace: None,
        }
    }
}

impl std::fmt::Debug for EngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineOptions")
            .field("max_lag", &self.max_lag)
            .field("trace", &self.trace.is_some())
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
    /// Sources, in config order, each under its supervisor.
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
    /// Source whose audio is program audio; its format is read when an
    /// output starts (`Runtime::program_audio_format`).
    audio_source: Option<usize>,
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
    /// Where each tick's trace record goes.
    trace: Tracer,
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
        let program_size = (config.output.width, config.output.height);
        for entry in &config.sources {
            let offline = entry.placeholder_info(program_size);
            let clock = options.clock.clone();
            let src =
                open_supervised(&entry.input, offline, clock, Backoff::default()).map_err(|e| {
                    EngineError::Source {
                        id: entry.id.clone(),
                        message: e.to_string(),
                    }
                })?;
            sources.push(Box::new(src));
        }
        let Detectors {
            loaders: detectors,
            sidecar,
        } = Detectors::build(&config)?;
        let infos: Vec<SourceInfo> = sources.iter().map(|s| s.info().clone()).collect();
        let enabled: Vec<bool> = detectors.iter().map(Option::is_some).collect();
        let core = EngineCore::new(&config, infos, &enabled).map_err(EngineError::Setup)?;
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
            clock: options.clock,
            period: config.frame_period(),
            max_lag: options.max_lag,
            timer: TickTimer::default(),
            late: 0,
            skipped: 0,
            trace: Tracer::new(options.trace),
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
