//! Supervision: reopening a source that fails, with backoff, under a new
//! epoch.
//!
//! A [`SupervisedSource`] wraps a [`SourceOpener`] (something that can open
//! a connection to one input, such as a file, as often as asked) and keeps
//! the input on air for as long as it is wanted:
//!
//! * **Start.** The first connection is opened synchronously, so an input
//!   that is there starts exactly as an unsupervised source would. When that
//!   attempt fails, the source starts [`SourceState::Offline`] instead of
//!   failing: [`SupervisedSource::start_error`] says why, and
//!   [`VideoSource::info`] is the caller's description of the source, since
//!   the input's own is not known yet. [`VideoSource::current_info`] is that
//!   description too until a connection delivers, and from then on the
//!   description of the newest connection that delivered, so whoever was
//!   given the caller's learns the input's own (its frame rate, its audio)
//!   once it is there.
//! * **Failure.** A connection that stops with an error (ffmpeg killed or
//!   crashed, a device unplugged) is stopped, its children reaped, and the
//!   source goes `Offline`. After a [`Backoff`] delay it goes
//!   `Reconnecting` and tries again. A failed attempt goes back to `Offline`
//!   for a longer delay; an attempt that opens stays `Reconnecting` until
//!   its first frame, and the source is then `Running` again. The delays
//!   double from [`Backoff::initial`] (0.5 s) up to [`Backoff::max`] (8 s),
//!   so an input that comes back is picked up at most 8 s later, plus the
//!   time to open it (spec C.3: a source reconnects within 8 s). A
//!   connection that stayed up for `max` resets the delay; one that fails
//!   sooner keeps backing off, so a flapping input is not reopened twice a
//!   second.
//! * **End.** A connection that stops without an error (a file played to
//!   its end) is not restarted: the source is `Stopped`, as an unsupervised
//!   one would be, and audio it still delivers is passed on.
//!   [`VideoSource::stop`] stops the source in any state, at once even
//!   during a backoff.
//!
//! # Epochs and timestamps
//!
//! Every connection is a new epoch (see `ac_media`): the supervisor tags its
//! frames with it and restamps them onto one timeline spanning all
//! connections. Sequence numbers continue after the last frame delivered;
//! gaps within a connection (frames its mailbox replaced) are kept, so they
//! still count as missed. PTS continues from the clock: a connection whose
//! attempt began `t` after the supervisor started stamps its first frame `t`,
//! and never at or before what was delivered last, so the time the input was
//! away counts as elapsed media time. The first connection, opened by
//! `start`, therefore has epoch 0 and offset zero: a source that never fails
//! delivers exactly the frames an unsupervised one would. Audio moves by the
//! same PTS offset as its connection's frames.
//!
//! # Threads
//!
//! One thread per source (`ac-supervise-<id>`) owns the current connection.
//! It passes each new frame on as soon as the connection publishes it
//! (waiting on the connection's mailbox, not polling), restamped, into the
//! supervisor's own latest-frame mailbox, moves the audio into a bounded
//! queue, watches the connection's state, and does all the opening,
//! stopping and waiting. The handle the engine holds reads only those
//! mailboxes and the health, so a reader never waits on I/O and no lock is
//! held across I/O. Backoff waits run on the injected [`Clock`]: on a
//! [`crate::VirtualClock`] they pass at once, which is how tests run an
//! outage of seconds in microseconds.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_media::{
    AudioBlock, LatestFrame, MediaError, SourceInfo, SourceState, VideoFrame, VideoSource,
};

use crate::audio::AudioQueue;
use crate::clock::{Clock, MAX_SLEEP_SLICE, sleep_until};
use crate::config::{SourceConfig, open_source_with_clock};
use crate::file::DEFAULT_AUDIO_QUEUE_BLOCKS;
use crate::status::lock;

/// Opens connections to one input; a supervisor calls it again after every
/// failure. Any `FnMut` closure returning a source is one.
pub trait SourceOpener: Send {
    /// Opens a new connection to the input.
    fn open(&mut self) -> Result<Box<dyn VideoSource>, MediaError>;
}

impl<F> SourceOpener for F
where
    F: FnMut() -> Result<Box<dyn VideoSource>, MediaError> + Send,
{
    fn open(&mut self) -> Result<Box<dyn VideoSource>, MediaError> {
        self()
    }
}

/// Delays between attempts to reopen a failed source; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// Delay before the first attempt after a failure; above zero.
    pub initial: Duration,
    /// Longest delay, at least `initial`; also how long a connection must
    /// stay up for the delay to start again from `initial`.
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(8),
        }
    }
}

impl Backoff {
    /// The delay before the attempt after the `failures`-th failure in a row
    /// (counted from 1): `initial` doubled `failures - 1` times, at most
    /// `max`.
    pub fn delay(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(31);
        self.initial.saturating_mul(1 << doublings).min(self.max)
    }

    /// Checks that the delays are usable: `initial` above zero (a zero
    /// delay would reopen a missing input as fast as the machine allows) and
    /// `max` at least `initial`.
    pub fn validate(&self) -> Result<(), String> {
        if self.initial.is_zero() || self.max < self.initial {
            return Err(format!(
                "reconnect backoff {:?}..{:?} needs 0 < initial <= max",
                self.initial, self.max
            ));
        }
        Ok(())
    }
}

/// How one connection's frames and audio are restamped onto the
/// supervisor's timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    /// The connection's epoch.
    epoch: u64,
    /// Added to the connection's sequence numbers.
    seq: u64,
    /// Added to the connection's PTS.
    pts: Duration,
}

impl Stamp {
    /// `frame` on the supervisor's timeline, tagged with the epoch.
    fn frame(&self, frame: VideoFrame) -> VideoFrame {
        VideoFrame {
            seq: self.seq.saturating_add(frame.seq),
            pts: self.pts.saturating_add(frame.pts),
            ..frame
        }
        .with_epoch(self.epoch)
    }

    /// `block` on the supervisor's timeline.
    fn audio(&self, block: AudioBlock) -> AudioBlock {
        AudioBlock {
            pts: self.pts.saturating_add(block.pts),
            ..block
        }
    }
}

/// The supervisor's bookkeeping: epochs, restamping offsets and backoff.
/// Plain data, driven with clock readings by the supervisor thread, so its
/// rules are tested step by step.
#[derive(Debug, Clone)]
struct Ledger {
    /// The delays.
    backoff: Backoff,
    /// Clock reading when the supervisor started; PTS counts from it.
    origin: Duration,
    /// Epoch of the next connection.
    next_epoch: u64,
    /// Failures in a row since the delay last started from `initial`.
    failures: u32,
    /// Sequence number the next connection's first frame gets: one past the
    /// newest frame delivered.
    next_seq: u64,
    /// Media time up to which frames and audio were delivered: the newest
    /// frame's PTS, or the end of the newest audio block if later.
    delivered: Option<Duration>,
}

impl Ledger {
    /// Bookkeeping for a supervisor started at clock reading `origin`.
    fn new(backoff: Backoff, origin: Duration) -> Self {
        Self {
            backoff,
            origin,
            next_epoch: 0,
            failures: 0,
            next_seq: 0,
            delivered: None,
        }
    }

    /// The stamp of a connection whose attempt began at clock reading
    /// `now`: the next epoch, sequence numbers after the newest frame, and
    /// PTS from the clock but after everything delivered.
    fn connect(&mut self, now: Duration) -> Stamp {
        let elapsed = now.saturating_sub(self.origin);
        let pts = match self.delivered {
            Some(until) => elapsed.max(until.saturating_add(Duration::from_nanos(1))),
            None => elapsed,
        };
        let stamp = Stamp {
            epoch: self.next_epoch,
            seq: self.next_seq,
            pts,
        };
        self.next_epoch += 1;
        stamp
    }

    /// Notes that `frame` (restamped) was delivered.
    fn frame_delivered(&mut self, frame: &VideoFrame) {
        self.next_seq = self.next_seq.max(frame.seq.saturating_add(1));
        self.note_delivered(frame.pts);
    }

    /// Notes that `block` (restamped) was delivered.
    fn audio_delivered(&mut self, block: &AudioBlock) {
        self.note_delivered(block.pts.saturating_add(block.duration()));
    }

    /// Moves the delivered media time up to `until`.
    fn note_delivered(&mut self, until: Duration) {
        self.delivered = Some(self.delivered.map_or(until, |d| d.max(until)));
    }

    /// The delay before the next attempt, after a failure at clock reading
    /// `now` of a connection that delivered frames since `up_since` (`None`
    /// for an attempt that failed to open, or a connection that delivered
    /// nothing).
    fn failed(&mut self, now: Duration, up_since: Option<Duration>) -> Duration {
        if up_since.is_some_and(|t| now.saturating_sub(t) >= self.backoff.max) {
            self.failures = 0;
        }
        self.failures = self.failures.saturating_add(1);
        self.backoff.delay(self.failures)
    }
}

/// What a [`SupervisedSource`] reports, written by its thread.
#[derive(Debug, Clone)]
struct Health {
    /// The source's state.
    state: SourceState,
    /// Why the source went offline; cleared once a connection delivers.
    error: Option<String>,
    /// The description of the newest connection that delivered (the first
    /// connection's from the start, when `start` opened one), else the
    /// caller's; set before that connection's first frame is passed on.
    info: SourceInfo,
}

/// State shared by a [`SupervisedSource`] and its thread.
#[derive(Debug)]
struct Shared {
    /// Raised once by `stop`.
    stop: AtomicBool,
    /// The newest frame of any connection, restamped.
    latest: LatestFrame,
    /// Audio of every connection, restamped, awaiting `drain_audio`.
    audio: AudioQueue,
    /// State and error.
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

    /// Marks the source `Running` on a connection described by `info`,
    /// clearing the error: called before the connection's first frame is
    /// passed on.
    fn delivering(&self, info: SourceInfo) {
        *lock(&self.health) = Health {
            state: SourceState::Running,
            error: None,
            info,
        };
    }
}

/// A source that is reopened after failures; see the module docs.
pub struct SupervisedSource {
    /// The first connection's description, or the caller's when the first
    /// attempt failed.
    info: SourceInfo,
    /// Why the first attempt failed, if it did.
    start_error: Option<String>,
    /// State shared with the thread.
    shared: Arc<Shared>,
    /// The supervisor thread, joined by `stop`.
    worker: Option<JoinHandle<()>>,
}

impl fmt::Debug for SupervisedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SupervisedSource")
            .field("info", &self.info)
            .field("health", &*lock(&self.shared.health))
            .finish_non_exhaustive()
    }
}

impl SupervisedSource {
    /// Opens the first connection with `opener` and starts supervising it,
    /// with `backoff` delays on `clock`. When the first attempt fails, the
    /// source starts offline and is described by `offline` (whose id is the
    /// source's). Fails only on an unusable `backoff` or when the thread
    /// cannot be spawned.
    pub fn start(
        mut opener: impl SourceOpener + 'static,
        offline: SourceInfo,
        clock: Arc<dyn Clock>,
        backoff: Backoff,
    ) -> Result<Self, MediaError> {
        backoff.validate().map_err(MediaError::Open)?;
        let mut ledger = Ledger::new(backoff, clock.now());
        let name = format!("ac-supervise-{}", offline.id.0);
        let (first, state, info, start_error) = match opener.open() {
            Ok(source) => {
                let info = source.info().clone();
                let stamp = ledger.connect(ledger.origin);
                (Some((source, stamp)), SourceState::Starting, info, None)
            }
            Err(e) => (None, SourceState::Offline, offline, Some(e.to_string())),
        };
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            latest: LatestFrame::new(),
            audio: AudioQueue::new(DEFAULT_AUDIO_QUEUE_BLOCKS),
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

    /// Why the first attempt to open the source, made by
    /// [`SupervisedSource::start`], failed; `None` when it opened.
    pub fn start_error(&self) -> Option<&str> {
        self.start_error.as_deref()
    }
}

impl VideoSource for SupervisedSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        lock(&self.shared.health).state
    }

    fn latest(&self) -> Option<VideoFrame> {
        self.shared.latest.get()
    }

    fn wait_newer(&self, after: Option<u64>, timeout: Duration) -> Option<VideoFrame> {
        self.shared.latest.wait_newer(after, timeout)
    }

    fn drain_audio(&self) -> Vec<AudioBlock> {
        self.shared.audio.drain()
    }

    fn error(&self) -> Option<String> {
        lock(&self.shared.health).error.clone()
    }

    fn current_info(&self) -> SourceInfo {
        lock(&self.shared.health).info.clone()
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.shared.set(
                SourceState::Stopped,
                Some("the source supervisor panicked".into()),
            );
        }
        self.shared.set_state(SourceState::Stopped);
    }
}

impl Drop for SupervisedSource {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Opens `config` as a supervised source paced by `clock`; `offline`
/// describes it (and gives its id) while it has not opened.
pub fn open_supervised(
    config: &SourceConfig,
    offline: SourceInfo,
    clock: Arc<dyn Clock>,
    backoff: Backoff,
) -> Result<SupervisedSource, MediaError> {
    let (config, id, pacing) = (config.clone(), offline.id.clone(), clock.clone());
    let opener = move || open_source_with_clock(&config, id.clone(), pacing.clone());
    SupervisedSource::start(opener, offline, clock, backoff)
}

/// Why a connection ended.
#[derive(Debug)]
enum Ended {
    /// A stop was requested.
    Stopping,
    /// The connection stopped with `error` after delivering frames since
    /// `up_since` (a clock reading), if it delivered any.
    Failed {
        /// The connection's error.
        error: String,
        /// When its first frame was passed on.
        up_since: Option<Duration>,
    },
}

/// The supervisor thread's state; see the module docs.
struct Supervisor {
    /// Opens connections.
    opener: Box<dyn SourceOpener>,
    /// Paces backoff waits and stamps PTS.
    clock: Arc<dyn Clock>,
    /// Epochs, offsets and delays.
    ledger: Ledger,
    /// State shared with the handle.
    shared: Arc<Shared>,
}

impl Supervisor {
    /// Supervises from `first` (the connection `start` opened, if any)
    /// until a stop request.
    fn run(mut self, first: Option<(Box<dyn VideoSource>, Stamp)>) {
        let mut next = first;
        if next.is_none() {
            let delay = self.ledger.failed(self.clock.now(), None);
            next = self.reconnect(delay);
        }
        while let Some((mut source, stamp)) = next.take() {
            let ended = self.forward(&*source, stamp);
            // Stopping a connection joins its workers and reaps its children.
            source.stop();
            drop(source);
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
    fn reconnect(&mut self, mut delay: Duration) -> Option<(Box<dyn VideoSource>, Stamp)> {
        loop {
            let due = self.clock.now().saturating_add(delay);
            if !sleep_until(&*self.clock, due, &self.shared.stop) {
                return None;
            }
            self.shared.set_state(SourceState::Reconnecting);
            let began = self.clock.now();
            match self.opener.open() {
                Ok(source) => return Some((source, self.ledger.connect(began))),
                Err(e) => {
                    self.shared.set(SourceState::Offline, Some(e.to_string()));
                    delay = self.ledger.failed(self.clock.now(), None);
                }
            }
        }
    }

    /// Passes `source`'s frames and audio on under `stamp` until a stop
    /// request or the connection's failure. A connection that ends without
    /// an error makes the source `Stopped`; its remaining audio is still
    /// passed on until the stop request.
    fn forward(&mut self, source: &dyn VideoSource, stamp: Stamp) -> Ended {
        let mut last: Option<u64> = None;
        let mut up_since: Option<Duration> = None;
        let mut ended = false;
        loop {
            // Read the state first: whatever the connection published before
            // it stopped is then passed on below.
            let stopped = source.state() == SourceState::Stopped;
            let wait = if stopped && !ended {
                Duration::ZERO
            } else {
                MAX_SLEEP_SLICE
            };
            if let Some(frame) = source.wait_newer(last, wait) {
                last = Some(frame.seq);
                // Running, and described as this connection, before the
                // first frame is out, so a reader that sees a frame of this
                // epoch never sees an older state or description.
                if up_since.is_none() {
                    up_since = Some(self.clock.now());
                    self.shared.delivering(source.info().clone());
                }
                let frame = stamp.frame(frame);
                self.ledger.frame_delivered(&frame);
                self.shared.latest.publish(frame);
            }
            for block in source.drain_audio() {
                let block = stamp.audio(block);
                self.ledger.audio_delivered(&block);
                self.shared.audio.push(block);
            }
            if stopped && !ended {
                match source.error() {
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

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
