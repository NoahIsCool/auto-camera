//! The stream's health, read from ffmpeg's log (spec M11, and M14:
//! encoder status, visible errors).
//!
//! # What ffmpeg says
//!
//! The RTMP slave runs behind ffmpeg's `fifo` muxer (see [`crate::args`]).
//! Its consumer thread opens the ingest and writes the packets the tee
//! hands it through a queue of [`FIFO_QUEUE_PACKETS`]; when anything fails
//! it closes the connection and opens it again, at once and then every
//! [`RTMP_RECOVERY_WAIT_SECONDS`](crate::args::RTMP_RECOVERY_WAIT_SECONDS)
//! for as long as packets keep coming, dropping them meanwhile. On the way
//! it logs these lines, and the tee one of its own:
//!
//! | Line | Level | Meaning |
//! |---|---|---|
//! | `[fifo] Error opening <url>: <why>` | error | opening the ingest failed |
//! | `[fifo] Recovery failed: <why>` | info | a reconnection attempt failed; the next is due after the wait |
//! | `[fifo] Recovery successful` | info | a reconnection attempt opened the ingest again |
//! | `[fifo] FIFO queue full` | warning | the consumer has not taken a queue's worth of packets (it is stuck opening or writing); they are dropped |
//! | `[fifo] FIFO queue flushed` | info | the consumer is back and dropped its backlog |
//! | `[fifo] Maximal number of <n> recovery attempts reached.` | error | ffmpeg gave up (only with an attempt limit, which `args` does not set) |
//! | `[tee] Slave muxer #<k> failed: <why>, continuing with ...` | error | the tee abandoned slave `k` for good |
//!
//! A connection that breaks while streaming is not logged as such: the
//! first sign is the failed reopen of the immediate first attempt (after
//! the `tcp` and `rtmp` layers' own error lines). One that breaks and
//! reopens at once shows only `Recovery successful`. The canned stderr of
//! each case, captured from ffmpeg, is in `tests/fixtures/stderr`.
//!
//! # Connected is inferred
//!
//! Nothing is logged when the first connection succeeds, so the monitor
//! infers it: the stream counts as connected once ffmpeg has muxed more
//! media than the fifo queue holds since the start, with no error and no
//! overflow. A consumer that had not opened the ingest (or could not
//! write to it) would have let the queue overflow by then and said so,
//! and a failed open says so at once. [`settle_time`] is that much media:
//! the queue's packets at the stream's packet rate (one per video frame
//! plus about 47 AAC packets a second), plus [`SETTLE_MARGIN`] for the
//! half-second `-progress` period in which media time arrives. The same
//! proof ends [`StreamHealth::stalled`] after an overflow while connected:
//! an ingest that cannot keep up, or a cable pulled (TCP can take minutes
//! to report that). After `Recovery successful` no proof is needed, since
//! ffmpeg says so.
//!
//! # States
//!
//! ```text
//! Connecting --proof--> Connected
//!     |                     |
//!     +------open failed / attempt failed------> Reconnecting { attempt: 1 }
//! Reconnecting { attempt: n } --attempt failed--> Reconnecting { attempt: n + 1 }
//! Reconnecting --Recovery successful--> Connected
//! any --slave abandoned / ffmpeg gave up / ffmpeg exited--> Down (final)
//! ```
//!
//! `attempt` is the reconnection attempt under way or due next. Each entry
//! into `Reconnecting` counts an outage and each `Recovery successful` a
//! reconnection (and an outage when none was seen: the connection broke and
//! opened again between two lines).
//!
//! The monitor is pure: log lines and media time in, health out. The
//! stderr reader owns the one monitor and publishes copies; a reader
//! evaluates its copy at the current media time, so the inference needs no
//! thread or clock of its own.

use std::fmt;
use std::time::Duration;

use crate::args::FIFO_QUEUE_PACKETS;
use crate::logline::LogLine;

/// Media time added to the fifo queue's length before a stream counts as
/// taking packets; see the module docs.
pub const SETTLE_MARGIN: Duration = Duration::from_secs(1);

/// AAC packets per second at the 48 kHz the encoder resamples to (1024
/// samples a packet).
const AAC_PACKETS_PER_SECOND: f64 = 48_000.0 / 1024.0;

/// Classes of the objects that carry the stream: the fifo muxer, the FLV
/// muxer inside it and the protocols under it (see [`is_stream_line`]).
const STREAM_CONTEXTS: &[&str] = &[
    "fifo", "flv", "rtmp", "rtmps", "rtmpe", "rtmpt", "rtmpte", "rtmpts", "tls", "tcp",
];

/// The stream's connection, as far as ffmpeg's log tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    /// Opening the ingest; not yet known to take the stream.
    Connecting,
    /// The ingest takes the stream.
    Connected,
    /// The connection failed and ffmpeg opens it again every
    /// [`RTMP_RECOVERY_WAIT_SECONDS`](crate::args::RTMP_RECOVERY_WAIT_SECONDS),
    /// dropping the stream meanwhile; the recording goes on regardless.
    Reconnecting {
        /// The attempt under way or due next, from 1.
        attempt: u32,
    },
    /// Over for this session: ffmpeg abandoned the stream or has exited.
    /// Only a new session streams again.
    Down,
}

impl fmt::Display for StreamState {
    /// `connecting`, `connected`, `reconnecting (attempt 2)` or `down`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamState::Connecting => f.write_str("connecting"),
            StreamState::Connected => f.write_str("connected"),
            StreamState::Reconnecting { attempt } => write!(f, "reconnecting (attempt {attempt})"),
            StreamState::Down => f.write_str("down"),
        }
    }
}

/// A snapshot of the stream's health, in
/// [`EncoderStats::stream`](crate::EncoderStats::stream).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamHealth {
    /// The connection.
    pub state: StreamState,
    /// The ingest stopped taking packets for a queue's worth (ffmpeg's
    /// queue to it overflowed), so they are being dropped: while
    /// connecting, the ingest does not answer; while connected, it cannot
    /// keep up or has gone silent. Ends once the ingest has taken a
    /// queue's worth again (see the module docs).
    pub stalled: bool,
    /// Times the connection was lost (each began a `Reconnecting` spell).
    pub outages: u32,
    /// Times ffmpeg opened the connection again after losing it.
    pub reconnects: u32,
    /// What the ingest takes in kbit/s of encoded audio and video
    /// (container overhead aside), over the newest
    /// [`STREAM_BITRATE_WINDOW`](crate::progress::STREAM_BITRATE_WINDOW)
    /// of media: the program's bitrate while connected and not stalled,
    /// zero otherwise. `None` before the first measurement and when this
    /// ffmpeg cannot report the packets it muxes (older than 6.1).
    pub bitrate_kbps: Option<f64>,
}

/// What a log line told the monitor; [`StreamMonitor::feed`] returns it so
/// the reader can act on it (a reconnection clears the stream's errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent {
    /// `Error opening`: opening the ingest failed.
    OpenFailed,
    /// `Recovery failed`: a reconnection attempt failed.
    AttemptFailed,
    /// `Recovery successful`: the connection is open again.
    Reconnected,
    /// `FIFO queue full`: packets are being dropped.
    QueueFull,
    /// `FIFO queue flushed`: the consumer is back.
    QueueFlushed,
    /// The tee abandoned the stream's slave, or ffmpeg stopped trying.
    Abandoned,
}

/// Where the connection stands, as the log last said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The first connection is being opened (`Connecting` until proven).
    Opening,
    /// Open: proven, or opened again per ffmpeg.
    Open,
    /// Reconnecting after `failed` failed attempts.
    Retrying {
        /// Attempts that failed in this outage.
        failed: u32,
    },
    /// Abandoned for good.
    Abandoned,
}

/// The state machine of the module docs. `Copy`, so the reader that owns
/// it publishes a copy after every event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamMonitor {
    /// Media time the ingest must run past a mark to prove it takes
    /// packets ([`settle_time`]).
    settle: Duration,
    /// Index of the stream's slave in the tee.
    slave: usize,
    /// Where the connection stands.
    phase: Phase,
    /// Media time since which it is unproven that the ingest takes packets
    /// (the start, or the newest overflow or flush); `None` once proven or
    /// told by ffmpeg.
    unproven_since: Option<Duration>,
    /// An overflow was logged and no flush since.
    overflowing: bool,
    /// Outages so far.
    outages: u32,
    /// Reconnections so far.
    reconnects: u32,
}

/// The media time after which a stream with `fps` video frames per second,
/// and AAC audio when `audio`, counts as taking packets: the fifo queue's
/// [`FIFO_QUEUE_PACKETS`] at its packet rate, plus [`SETTLE_MARGIN`].
pub fn settle_time(fps: u32, audio: bool) -> Duration {
    let audio_rate = if audio { AAC_PACKETS_PER_SECOND } else { 0.0 };
    let packets_per_second = f64::from(fps.max(1)) + audio_rate;
    Duration::from_secs_f64(f64::from(FIFO_QUEUE_PACKETS) / packets_per_second) + SETTLE_MARGIN
}

/// True when `line` was logged by an object that carries the stream, so
/// an error in it is the stream's. The audio input is a TCP connection
/// too, but its errors come only while ffmpeg starts, which they end.
pub fn is_stream_line(line: &LogLine) -> bool {
    line.context.is_some_and(|c| STREAM_CONTEXTS.contains(&c))
}

/// The slave index a `Slave muxer #<k> failed` message names.
fn failed_slave(message: &str) -> Option<usize> {
    let rest = message.strip_prefix("Slave muxer #")?;
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (index, rest) = rest.split_at(digits);
    if rest.starts_with(" failed") {
        index.parse().ok()
    } else {
        None
    }
}

impl StreamMonitor {
    /// A monitor for a stream that is just being opened, proven after
    /// `settle` of media ([`settle_time`]), carried by tee slave `slave`
    /// ([`rtmp_slave_index`](crate::args::rtmp_slave_index)).
    pub fn new(settle: Duration, slave: usize) -> Self {
        Self {
            settle,
            slave,
            phase: Phase::Opening,
            unproven_since: Some(Duration::ZERO),
            overflowing: false,
            outages: 0,
            reconnects: 0,
        }
    }

    /// What `line` says about the connection, if anything.
    pub fn event(&self, line: &LogLine) -> Option<StreamEvent> {
        let m = line.message;
        match line.context? {
            "fifo" if m.starts_with("Error opening ") => Some(StreamEvent::OpenFailed),
            "fifo" if m.starts_with("Recovery failed") => Some(StreamEvent::AttemptFailed),
            "fifo" if m.starts_with("Recovery successful") => Some(StreamEvent::Reconnected),
            "fifo" if m.starts_with("FIFO queue full") => Some(StreamEvent::QueueFull),
            "fifo" if m.starts_with("FIFO queue flushed") => Some(StreamEvent::QueueFlushed),
            "fifo" if m.starts_with("Maximal number of") => Some(StreamEvent::Abandoned),
            "tee" if m.starts_with("All tee outputs failed") => Some(StreamEvent::Abandoned),
            "tee" if failed_slave(m) == Some(self.slave) => Some(StreamEvent::Abandoned),
            _ => None,
        }
    }

    /// Takes in one log line, logged when ffmpeg had muxed `media_time`;
    /// returns what it said about the connection.
    pub fn feed(&mut self, line: &LogLine, media_time: Duration) -> Option<StreamEvent> {
        let event = self.event(line)?;
        if self.proven_by(media_time) {
            // The proof held until this line: keep it.
            self.unproven_since = None;
            if self.phase == Phase::Opening {
                self.phase = Phase::Open;
            }
        }
        let live = matches!(self.phase, Phase::Opening | Phase::Open);
        match event {
            StreamEvent::OpenFailed => self.retry(0),
            StreamEvent::AttemptFailed => {
                let failed = match self.phase {
                    Phase::Retrying { failed } => failed,
                    _ => 0,
                };
                self.retry(failed + 1);
            }
            StreamEvent::Reconnected if self.phase != Phase::Abandoned => {
                if live {
                    self.outages += 1;
                }
                self.reconnects += 1;
                self.phase = Phase::Open;
                self.overflowing = false;
                self.unproven_since = None;
            }
            StreamEvent::QueueFull | StreamEvent::QueueFlushed if live => {
                self.overflowing = event == StreamEvent::QueueFull;
                self.unproven_since = Some(media_time);
            }
            StreamEvent::Abandoned => self.phase = Phase::Abandoned,
            _ => {}
        }
        Some(event)
    }

    /// Enters `Reconnecting`, or stays there, with at least `failed`
    /// failed attempts; counts an outage on entry.
    fn retry(&mut self, failed: u32) {
        self.phase = match self.phase {
            Phase::Abandoned => return,
            Phase::Retrying { failed: before } => Phase::Retrying {
                failed: failed.max(before),
            },
            Phase::Opening | Phase::Open => {
                self.outages += 1;
                Phase::Retrying { failed }
            }
        };
        self.overflowing = false;
        self.unproven_since = None;
    }

    /// True when the connection, open or opening, has taken packets for
    /// [`settle_time`] since the newest mark by `media_time`.
    fn proven_by(&self, media_time: Duration) -> bool {
        matches!(self.phase, Phase::Opening | Phase::Open)
            && !self.overflowing
            && self
                .unproven_since
                .is_some_and(|since| media_time >= since + self.settle)
    }

    /// The health at `media_time` (ffmpeg's newest `out_time`, `None`
    /// before its first report), while ffmpeg is `alive`, with `muxed_kbps`
    /// the bitrate ffmpeg is muxing (`None` when not measured).
    pub fn health(
        &self,
        media_time: Option<Duration>,
        alive: bool,
        muxed_kbps: Option<f64>,
    ) -> StreamHealth {
        let proven = self.proven_by(media_time.unwrap_or_default());
        let state = match self.phase {
            _ if !alive => StreamState::Down,
            Phase::Opening if proven => StreamState::Connected,
            Phase::Opening => StreamState::Connecting,
            Phase::Open => StreamState::Connected,
            Phase::Retrying { failed } => StreamState::Reconnecting {
                attempt: failed + 1,
            },
            Phase::Abandoned => StreamState::Down,
        };
        let stalled = match state {
            StreamState::Connecting => self.overflowing,
            StreamState::Connected => {
                self.overflowing || (self.unproven_since.is_some() && !proven)
            }
            _ => false,
        };
        let taking = state == StreamState::Connected && !stalled;
        StreamHealth {
            state,
            stalled,
            outages: self.outages,
            reconnects: self.reconnects,
            bitrate_kbps: muxed_kbps.map(|kbps| if taking { kbps } else { 0.0 }),
        }
    }
}

#[cfg(test)]
mod tests;
