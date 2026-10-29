//! A device's stream as an [`AudioInput`]: the worker that carries blocks
//! from the hand-off to the input's ring, and the input's health.
//!
//! # Theory
//!
//! [`DeviceInput::open`] lists the devices, selects the configured UID
//! ([`super::select`]), makes the hand-off and starts the backend's stream,
//! then reads the host clock and the pacing clock back to back to fix the
//! stamper's origin ([`super::timing`]). One worker thread
//! (`ac-audio-dev-<id>`) owns the stream from then on. Every
//! [`HANDOFF_POLL`] it
//!
//! * turns new overloads the stream reported into the input's notice
//!   ([`AudioInput::error`] while it runs) and a log line;
//! * takes every filled buffer from the hand-off, stamps it, gives the
//!   buffer back to the pool and queues the block in the input's
//!   [`AudioRing`] (the first one makes the input `Running`), and makes
//!   each discontinuity the stamper found the notice and a log line;
//! * stops the input with an error once the stream reports the device
//!   lost, or when no block came for the stall timeout (the open timeout
//!   before the first block), measured in real time: the device runs in
//!   real time whatever clock the studio is paced by.
//!
//! # Counting
//!
//! What the stamper measured and what the device reported are counted
//! apart, and each notice carries both counts: `audio discontinuity (3 so
//! far; the device reported 2 overloads): ...`. An overload that lost
//! audio is measured too, as the gap the stamper finds in the stream after
//! it, so counting the two together would count it twice; an overload that
//! lost nothing is still shown, on its own count. Within a poll the
//! overloads are noted first, so a discontinuity measured in the same poll
//! is the notice that stays: it says what was lost, the overload only that
//! something may have been.
//!
//! Polling, rather than waiting on the hand-off, is what keeps the
//! device's callback free of locks (see [`mod@super::handoff`]); a few
//! milliseconds of poll add no more than that to the input's latency.
//!
//! # Invariants
//!
//! * The worker drops the stream, which stops the device's callbacks,
//!   before it ends; [`AudioInput::stop`] joins it, so a stopped input has
//!   no callback left running.
//! * A failure is kept as the input's error, ahead of any notice; blocks
//!   queued before it stay in the ring for the reader.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{AudioBlock, AudioInput, AudioInputInfo, AudioRing, MediaError, SourceState};

use super::handoff::{HandoffReader, StreamEvents, handoff};
use super::timing::{Discontinuity, Stamper};
use super::{AudioDevice, DeviceBackend, DeviceStream, select};
use crate::clock::Clock;
use crate::file::DEFAULT_AUDIO_QUEUE_BLOCKS;
use crate::status::{Status, lock};

/// The target of a device input's own lines in the app's log.
const TARGET: &str = "ac_sources::audio_device";

/// How often the worker looks at the hand-off.
pub const HANDOFF_POLL: Duration = Duration::from_millis(2);

/// Least time between two log lines about one input's discontinuities;
/// the ones in between are counted on the next line.
pub const LOG_QUIET: Duration = Duration::from_secs(10);

/// How a device is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceOptions {
    /// Most blocks queued between drains (at least one); older blocks are
    /// dropped.
    pub queue_blocks: usize,
    /// Buffers in the hand-off's pool (at least one): how far the worker
    /// may fall behind the device's callback before audio is dropped.
    pub pool_buffers: usize,
    /// Most sample frames in a block (at least one); a longer callback
    /// buffer is cut.
    pub max_block_frames: usize,
    /// Longest wait for the first block of a connection (5 s by default).
    pub open_timeout: Duration,
    /// Longest wait for each later block (2 s by default, hundreds of a
    /// device's buffers).
    pub stall_timeout: Duration,
}

impl Default for DeviceOptions {
    fn default() -> Self {
        Self {
            queue_blocks: DEFAULT_AUDIO_QUEUE_BLOCKS,
            // 64 buffers of up to 1024 frames: 0.7 s to 1.4 s of audio at
            // 48 kHz, 8 MiB for 32 channels.
            pool_buffers: 64,
            max_block_frames: 1024,
            open_timeout: Duration::from_secs(5),
            stall_timeout: Duration::from_secs(2),
        }
    }
}

/// State shared by a [`DeviceInput`] and its worker.
struct Shared {
    /// Raised once by `stop`.
    stop: AtomicBool,
    /// Health and the stopping error.
    status: Status,
    /// Blocks awaiting `drain`.
    ring: AudioRing,
    /// Blocks the hand-off dropped.
    handoff_lost: Arc<AtomicU64>,
    /// Discontinuities the stamper measured so far.
    discontinuities: AtomicU64,
    /// Overloads the device reported so far.
    overloads: AtomicU64,
    /// The newest notice: a discontinuity or an overload.
    notice: Mutex<Option<String>>,
}

impl Shared {
    /// True once a stop was requested.
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// An audio device read as a live input; see the module docs.
pub struct DeviceInput {
    /// What it delivers.
    info: AudioInputInfo,
    /// The device, as listed when it opened.
    device: AudioDevice,
    /// State shared with the worker.
    shared: Arc<Shared>,
    /// The worker, joined by `stop`.
    worker: Option<JoinHandle<()>>,
}

impl fmt::Debug for DeviceInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceInput")
            .field("info", &self.info)
            .field("device", &self.device)
            .field("state", &self.shared.status.get())
            .finish_non_exhaustive()
    }
}

impl DeviceInput {
    /// Opens the device with UID `uid` among `backend`'s as input `id`,
    /// with PTS on `clock` from now; see the module docs.
    pub fn open(
        id: SourceId,
        uid: &str,
        backend: &dyn DeviceBackend,
        options: DeviceOptions,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, MediaError> {
        let opened_at = clock.now();
        let devices = backend.devices()?;
        let device = select(&devices, uid)?.clone();
        let (rate, channels) = (device.sample_rate, device.input_channels);
        let (sink, reader) = handoff(
            channels,
            rate,
            options.max_block_frames,
            options.pool_buffers,
        );
        let events = StreamEvents::new();
        let stream = backend.start(&device, sink, events.clone()).map_err(|e| {
            let why = match e {
                MediaError::Open(why) => why,
                other => other.to_string(),
            };
            MediaError::Open(format!("cannot open audio device {device}: {why}"))
        })?;
        // PTS 0 is the pacing clock's reading when the open began, put on
        // the host clock.
        let origin = stream
            .host_now()
            .saturating_sub(clock.now().saturating_sub(opened_at));
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            status: Status::new(),
            ring: AudioRing::new(options.queue_blocks),
            handoff_lost: reader.lost_counter(),
            discontinuities: AtomicU64::new(0),
            overloads: AtomicU64::new(0),
            notice: Mutex::new(None),
        });
        let worker = Worker {
            stamper: Stamper::new(id.clone(), rate, channels, origin),
            stream,
            reader,
            events,
            overloads: 0,
            log: QuietLog::default(),
            input: id.0.clone(),
            name: device.name.clone(),
            options,
        };
        let state = shared.clone();
        let handle = std::thread::Builder::new()
            .name(format!("ac-audio-dev-{}", id.0))
            .spawn(move || worker.run(&state))?;
        tracing::info!(target: TARGET, input = %id.0, "opened audio device {device}");
        Ok(Self {
            info: AudioInputInfo {
                id,
                format: Some((rate, channels)),
            },
            device,
            shared,
            worker: Some(handle),
        })
    }

    /// The device, as listed when the input opened.
    pub fn device(&self) -> &AudioDevice {
        &self.device
    }

    /// Discontinuities the stamper measured so far; see [`Discontinuity`].
    pub fn discontinuities(&self) -> u64 {
        self.shared.discontinuities.load(Ordering::Relaxed)
    }

    /// Overloads the device reported so far, counted apart from the
    /// discontinuities (see the module docs).
    pub fn overloads(&self) -> u64 {
        self.shared.overloads.load(Ordering::Relaxed)
    }
}

impl AudioInput for DeviceInput {
    fn info(&self) -> &AudioInputInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        self.shared.status.get()
    }

    fn drain(&self) -> Vec<AudioBlock> {
        self.shared.ring.drain()
    }

    fn wait(&self, timeout: Duration) -> bool {
        self.shared.ring.wait(timeout)
    }

    fn dropped(&self) -> u64 {
        self.shared.ring.dropped() + self.shared.handoff_lost.load(Ordering::Relaxed)
    }

    fn error(&self) -> Option<String> {
        self.shared
            .status
            .error()
            .or_else(|| lock(&self.shared.notice).clone())
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.shared.status.fail("the audio device worker panicked");
        }
        self.shared.status.advance(SourceState::Stopped);
    }
}

impl Drop for DeviceInput {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Decides which notices are logged: one line per [`LOG_QUIET`] at most,
/// the ones held back counted on the next.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct QuietLog {
    /// When the newest line was logged.
    last: Option<Instant>,
    /// Discontinuities held back since.
    held: u64,
}

impl QuietLog {
    /// `Some(held back since the last line)` when a notice at `now` is to
    /// be logged, else `None` (and it is counted).
    pub(crate) fn admit(&mut self, now: Instant) -> Option<u64> {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < LOG_QUIET => {
                self.held += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.held))
            }
        }
    }
}

/// The worker's state; see the module docs.
struct Worker {
    /// Stamps the blocks.
    stamper: Stamper,
    /// The device's stream, dropped when the worker ends.
    stream: Box<dyn DeviceStream>,
    /// Filled buffers from the callback.
    reader: HandoffReader,
    /// What the error callback reported.
    events: StreamEvents,
    /// Overloads already turned into notices.
    overloads: u64,
    /// Which notices are logged.
    log: QuietLog,
    /// The input's id, for the log.
    input: String,
    /// The device's name, for messages.
    name: String,
    /// The timeouts.
    options: DeviceOptions,
}

impl Worker {
    /// Runs until a stop request or the stream's failure.
    fn run(mut self, shared: &Shared) {
        let mut last_block = Instant::now();
        let mut delivered = false;
        let failure = loop {
            if shared.stopping() {
                break None;
            }
            // Overloads first: see the module docs.
            let overloads = self.events.overloads();
            for _ in self.overloads..overloads {
                self.note_overload(shared);
            }
            self.overloads = overloads;
            let mut got = false;
            while let Some(captured) = self.reader.take() {
                let (block, found) = self.stamper.stamp(&captured);
                self.reader.recycle(captured);
                for d in found {
                    self.note(shared, d);
                }
                shared.ring.push(block);
                shared.status.advance(SourceState::Running);
                got = true;
            }
            if let Some(why) = self.events.lost_reason() {
                break Some(format!("audio device {:?} went away: {why}", self.name));
            }
            if got {
                (last_block, delivered) = (Instant::now(), true);
            } else {
                let (limit, when) = if delivered {
                    (self.options.stall_timeout, "for")
                } else {
                    (self.options.open_timeout, "within")
                };
                if last_block.elapsed() >= limit {
                    break Some(format!(
                        "audio device {:?} delivered no audio {when} {:.1} s",
                        self.name,
                        limit.as_secs_f64()
                    ));
                }
            }
            std::thread::sleep(HANDOFF_POLL);
        };
        // Stops the callbacks before the hand-off's queues close.
        drop(self.stream);
        if let Some(error) = failure {
            tracing::warn!(target: TARGET, input = %self.input, "{error}");
            shared.status.fail(error);
        }
        shared.status.advance(SourceState::Stopped);
    }

    /// Counts the discontinuity `d` and publishes its notice, with the
    /// overloads reported so far.
    fn note(&mut self, shared: &Shared, d: Discontinuity) {
        let n = shared.discontinuities.fetch_add(1, Ordering::Relaxed) + 1;
        let reported = match shared.overloads.load(Ordering::Relaxed) {
            0 => String::new(),
            1 => "; the device reported 1 overload".into(),
            m => format!("; the device reported {m} overloads"),
        };
        self.publish(
            shared,
            format!("audio discontinuity ({n} so far{reported}): {d}"),
        );
    }

    /// Counts an overload the device reported and publishes its notice,
    /// with the discontinuities measured so far.
    fn note_overload(&mut self, shared: &Shared) {
        let m = shared.overloads.fetch_add(1, Ordering::Relaxed) + 1;
        let measured = match shared.discontinuities.load(Ordering::Relaxed) {
            0 => String::new(),
            1 => "; 1 audio discontinuity".into(),
            n => format!("; {n} audio discontinuities"),
        };
        self.publish(
            shared,
            format!(
                "the device reported an overload ({m} so far{measured}): its I/O cycle ran \
                 late, so its audio may have a gap"
            ),
        );
    }

    /// Makes `notice` the input's notice and logs it unless a line was
    /// logged within [`LOG_QUIET`].
    fn publish(&mut self, shared: &Shared, notice: String) {
        if let Some(held) = self.log.admit(Instant::now()) {
            let more = match held {
                0 => String::new(),
                h => format!(" ({h} more since the last line)"),
            };
            tracing::warn!(
                target: TARGET,
                input = %self.input,
                device = %self.name,
                "{notice}{more}"
            );
        }
        *lock(&shared.notice) = Some(notice);
    }
}
