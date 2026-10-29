//! Supervision of audio-only inputs: reopening one that fails, with
//! backoff, its blocks carrying on across connections.
//!
//! A [`SupervisedAudio`] is to an [`AudioInput`] what
//! [`crate::SupervisedSource`] is to a video source, with the same rules
//! and the same books ([`crate::supervisor`]):
//!
//! * **Start.** The first connection is opened synchronously; when that
//!   fails, the input starts `Offline`, described by the caller's
//!   placeholder (no format), and [`SupervisedAudio::start_error`] says why.
//! * **Failure.** A connection that stops with an error (ffmpeg killed, a
//!   device unplugged) is stopped and the input goes `Offline`, then
//!   `Reconnecting` after a [`Backoff`] delay (0.5 s doubling to 8 s; a
//!   connection that stayed up for the cap starts the delays over). An
//!   attempt that opens stays `Reconnecting` until its first block, and the
//!   input is then `Running`, described by that connection
//!   ([`AudioInput::current_info`]), so a reader learns its format.
//! * **End.** A connection that stops without an error (a file played to
//!   its end) is not reopened: the input is `Stopped`.
//! * **Trouble.** A connection that keeps delivering but reports an error
//!   (a device input's discontinuities, [`crate::audio_input::device`])
//!   stays on: the input stays `Running` with that error shown, until a
//!   newer one replaces it or a new connection delivers.
//!
//! # Sequence numbers and PTS
//!
//! Each connection numbers its blocks from 0 and starts its PTS at 0. The
//! supervisor restamps them onto one timeline: numbers continue after the
//! newest block delivered, and a connection whose attempt began `t` after
//! the supervisor started has its PTS moved to start at `t`, never at or
//! before the end of the audio delivered last, so the time an input was
//! away counts as elapsed and PTS never goes back. The first connection is
//! moved by nothing: an input that never fails delivers exactly what an
//! unsupervised one would.
//!
//! # Threads
//!
//! One thread per input (`ac-audio-supervise-<id>`) owns the current
//! connection. It waits on the connection's ring ([`AudioInput::wait`]),
//! passes each block on as soon as it arrives, restamped, into the
//! supervisor's own bounded ring, and does all the opening, stopping and
//! waiting. The handle the engine holds reads only that ring and the
//! health, so a reader never waits on I/O and no lock is held across I/O.
//! Backoff waits run on the injected [`Clock`], so on a
//! [`crate::VirtualClock`] an outage of seconds passes at once.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_media::{AudioBlock, AudioInput, AudioInputInfo, AudioRing, MediaError, SourceState};

use crate::clock::{Clock, MAX_SLEEP_SLICE, sleep_until};
use crate::file::DEFAULT_AUDIO_QUEUE_BLOCKS;
use crate::status::lock;
use crate::supervisor::{Backoff, Ledger, Stamp};

/// Opens connections to one audio input; a supervisor calls it again after
/// every failure. Any `FnMut` closure returning an input is one.
pub trait AudioOpener: Send {
    /// Opens a new connection to the input.
    fn open(&mut self) -> Result<Box<dyn AudioInput>, MediaError>;
}

impl<F> AudioOpener for F
where
    F: FnMut() -> Result<Box<dyn AudioInput>, MediaError> + Send,
{
    fn open(&mut self) -> Result<Box<dyn AudioInput>, MediaError> {
        self()
    }
}

/// What a [`SupervisedAudio`] reports, written by its thread.
#[derive(Debug, Clone)]
struct Health {
    /// The input's state.
    state: SourceState,
    /// Why the input went offline; cleared once a connection delivers.
    /// While one delivers, the trouble it reports, if any.
    error: Option<String>,
    /// The description of the newest connection that delivered (the first
    /// connection's from the start, when `start` opened one), else the
    /// caller's.
    info: AudioInputInfo,
}

/// State shared by a [`SupervisedAudio`] and its thread.
#[derive(Debug)]
struct Shared {
    /// Raised once by `stop`.
    stop: AtomicBool,
    /// Blocks of every connection, restamped, awaiting `drain`.
    ring: AudioRing,
    /// Blocks the connections' own rings dropped, before this ring.
    upstream_drops: AtomicU64,
    /// State, error and description.
    health: Mutex<Health>,
}

impl Shared {
    /// True once a stop was requested.
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Sets the state, keeping the error.
    fn set_state(&self, state: SourceState) {
        lock(&self.health).state = state;
    }

    /// Sets the state and the error, keeping the description.
    fn set(&self, state: SourceState, error: Option<String>) {
        let mut health = lock(&self.health);
        health.state = state;
        health.error = error;
    }

    /// Shows `notice`, the trouble a delivering connection reports, as the
    /// input's error.
    fn notice(&self, notice: String) {
        let mut health = lock(&self.health);
        if health.error.as_ref() != Some(&notice) {
            health.error = Some(notice);
        }
    }

    /// Marks the input `Running` on a connection described by `info`,
    /// clearing the error: called before the connection's first block is
    /// passed on.
    fn delivering(&self, info: AudioInputInfo) {
        *lock(&self.health) = Health {
            state: SourceState::Running,
            error: None,
            info,
        };
    }
}

/// An audio input that is reopened after failures; see the module docs.
pub struct SupervisedAudio {
    /// The first connection's description, or the caller's when the first
    /// attempt failed.
    info: AudioInputInfo,
    /// Why the first attempt failed, if it did.
    start_error: Option<String>,
    /// State shared with the thread.
    shared: Arc<Shared>,
    /// The supervisor thread, joined by `stop`.
    worker: Option<JoinHandle<()>>,
}

impl fmt::Debug for SupervisedAudio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SupervisedAudio")
            .field("info", &self.info)
            .field("health", &*lock(&self.shared.health))
            .finish_non_exhaustive()
    }
}

impl SupervisedAudio {
    /// Opens the first connection with `opener` and starts supervising it,
    /// with `backoff` delays on `clock`. When the first attempt fails, the
    /// input starts offline and is described by `offline` (whose id is the
    /// input's). Fails only on an unusable `backoff` or when the thread
    /// cannot be spawned.
    pub fn start(
        mut opener: impl AudioOpener + 'static,
        offline: AudioInputInfo,
        clock: Arc<dyn Clock>,
        backoff: Backoff,
    ) -> Result<Self, MediaError> {
        backoff.validate().map_err(MediaError::Open)?;
        let mut ledger = Ledger::new(backoff, clock.now());
        let name = format!("ac-audio-supervise-{}", offline.id.0);
        let (first, state, info, start_error) = match opener.open() {
            Ok(input) => {
                let info = input.info().clone();
                let stamp = ledger.connect(ledger.origin());
                (Some((input, stamp)), SourceState::Starting, info, None)
            }
            Err(e) => (None, SourceState::Offline, offline, Some(e.to_string())),
        };
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            ring: AudioRing::new(DEFAULT_AUDIO_QUEUE_BLOCKS),
            upstream_drops: AtomicU64::new(0),
            health: Mutex::new(Health {
                state,
                error: start_error.clone(),
                info: info.clone(),
            }),
        });
        let supervisor = Supervisor {
            opener: Box::new(opener),
            clock,
            ledger,
            shared: shared.clone(),
        };
        let worker = std::thread::Builder::new()
            .name(name)
            .spawn(move || supervisor.run(first))?;
        Ok(Self {
            info,
            start_error,
            shared,
            worker: Some(worker),
        })
    }

    /// Why the first attempt to open the input, made by
    /// [`SupervisedAudio::start`], failed; `None` when it opened.
    pub fn start_error(&self) -> Option<&str> {
        self.start_error.as_deref()
    }
}

impl AudioInput for SupervisedAudio {
    fn info(&self) -> &AudioInputInfo {
        &self.info
    }

    fn current_info(&self) -> AudioInputInfo {
        lock(&self.shared.health).info.clone()
    }

    fn state(&self) -> SourceState {
        lock(&self.shared.health).state
    }

    fn drain(&self) -> Vec<AudioBlock> {
        self.shared.ring.drain()
    }

    fn wait(&self, timeout: Duration) -> bool {
        self.shared.ring.wait(timeout)
    }

    fn dropped(&self) -> u64 {
        self.shared.ring.dropped() + self.shared.upstream_drops.load(Ordering::Relaxed)
    }

    fn error(&self) -> Option<String> {
        lock(&self.shared.health).error.clone()
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.shared.set(
                SourceState::Stopped,
                Some("the audio input supervisor panicked".into()),
            );
        }
        self.shared.set_state(SourceState::Stopped);
    }
}

impl Drop for SupervisedAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Why a connection ended.
#[derive(Debug)]
enum Ended {
    /// A stop was requested.
    Stopping,
    /// The connection stopped with `error` after delivering blocks since
    /// `up_since` (a clock reading), if it delivered any.
    Failed {
        /// The connection's error.
        error: String,
        /// When its first block was passed on.
        up_since: Option<Duration>,
    },
}

/// The supervisor thread's state; see the module docs.
struct Supervisor {
    /// Opens connections.
    opener: Box<dyn AudioOpener>,
    /// Paces backoff waits and stamps PTS.
    clock: Arc<dyn Clock>,
    /// Sequence numbers, offsets and delays.
    ledger: Ledger,
    /// State shared with the handle.
    shared: Arc<Shared>,
}

impl Supervisor {
    /// Supervises from `first` (the connection `start` opened, if any)
    /// until a stop request.
    fn run(mut self, first: Option<(Box<dyn AudioInput>, Stamp)>) {
        let mut next = first;
        if next.is_none() {
            let delay = self.ledger.failed(self.clock.now(), None);
            next = self.reconnect(delay);
        }
        let mut drops = 0;
        while let Some((mut input, stamp)) = next.take() {
            let ended = self.forward(&*input, stamp, drops);
            // Stopping a connection joins its threads and reaps its children.
            input.stop();
            drops += input.dropped();
            drop(input);
            match ended {
                Ended::Stopping => break,
                Ended::Failed { error, up_since } => {
                    self.shared.set(SourceState::Offline, Some(error));
                    let delay = self.ledger.failed(self.clock.now(), up_since);
                    next = self.reconnect(delay);
                }
            }
        }
        self.shared.set_state(SourceState::Stopped);
    }

    /// Waits `delay`, then tries to open the input until an attempt opens
    /// it, each failure backing off further. `None` once a stop is
    /// requested.
    fn reconnect(&mut self, mut delay: Duration) -> Option<(Box<dyn AudioInput>, Stamp)> {
        loop {
            let due = self.clock.now().saturating_add(delay);
            if !sleep_until(&*self.clock, due, &self.shared.stop) {
                return None;
            }
            self.shared.set_state(SourceState::Reconnecting);
            let began = self.clock.now();
            match self.opener.open() {
                Ok(input) => return Some((input, self.ledger.connect(began))),
                Err(e) => {
                    self.shared.set(SourceState::Offline, Some(e.to_string()));
                    delay = self.ledger.failed(self.clock.now(), None);
                }
            }
        }
    }

    /// Passes `input`'s blocks on under `stamp` until a stop request or the
    /// connection's failure; `drops` is what earlier connections dropped. A
    /// connection that ends without an error makes the input `Stopped`; its
    /// remaining blocks are still passed on until the stop request.
    fn forward(&mut self, input: &dyn AudioInput, stamp: Stamp, drops: u64) -> Ended {
        let mut up_since: Option<Duration> = None;
        let mut ended = false;
        loop {
            // Read the state first: whatever the connection queued before it
            // stopped is then passed on below.
            let stopped = input.state() == SourceState::Stopped;
            let wait = if stopped && !ended {
                Duration::ZERO
            } else {
                MAX_SLEEP_SLICE
            };
            input.wait(wait);
            for block in input.drain() {
                // Running, and described as this connection, before its
                // first block is out.
                if up_since.is_none() {
                    up_since = Some(self.clock.now());
                    self.shared.delivering(input.info().clone());
                }
                let block = stamp.audio(block);
                self.ledger.audio_delivered(&block);
                self.shared.ring.push(block);
            }
            let upstream = drops + input.dropped();
            self.shared
                .upstream_drops
                .store(upstream, Ordering::Relaxed);
            if !stopped
                && up_since.is_some()
                && let Some(notice) = input.error()
            {
                self.shared.notice(notice);
            }
            if stopped && !ended {
                match input.error() {
                    Some(error) => return Ended::Failed { error, up_since },
                    None => {
                        ended = true;
                        self.shared.set_state(SourceState::Stopped);
                    }
                }
            }
            if self.shared.stopping() {
                return Ended::Stopping;
            }
        }
    }
}
