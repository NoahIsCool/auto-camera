//! The host's configuration, requests, errors and status types.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::prep::RgbImage;
use crate::protocol::{Found, Hello, Phrase};

/// How to run the sidecar; see the module docs for the deadlines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConfig {
    /// Program and arguments, such as `[".../python", "server.py",
    /// "--side", "672"]`.
    pub command: Vec<String>,
    /// Working directory of the child; the host's own when `None`.
    pub current_dir: Option<PathBuf>,
    /// Longest wait for `hello` after a spawn.
    pub startup_timeout: Duration,
    /// How long a child may take to exit after its stdin closes at
    /// shutdown, before it is killed.
    pub exit_grace: Duration,
    /// Wait before the first restart after a failure; doubles with each
    /// further consecutive failure.
    pub restart_backoff: Duration,
    /// Longest wait between restarts.
    pub max_restart_backoff: Duration,
    /// Number of the child's stderr lines kept.
    pub log_lines: usize,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            current_dir: None,
            startup_timeout: Duration::from_secs(60),
            exit_grace: Duration::from_secs(2),
            restart_backoff: Duration::from_secs(1),
            max_restart_backoff: Duration::from_secs(30),
            log_lines: 200,
        }
    }
}

impl HostConfig {
    /// The defaults with `command`.
    pub fn new<S: Into<String>>(command: impl IntoIterator<Item = S>) -> Self {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// Checks the values: a program to run, a positive startup deadline, a
    /// backoff no longer than its cap, at least one log line.
    pub fn validate(&self) -> Result<(), HostError> {
        let bad = |m: &str| Err(HostError::Config(m.into()));
        if self.command.first().is_none_or(|p| p.trim().is_empty()) {
            return bad("command must name a program");
        }
        if self.startup_timeout.is_zero() {
            return bad("startup_timeout must be positive");
        }
        if self.restart_backoff > self.max_restart_backoff {
            return bad("restart_backoff must not exceed max_restart_backoff");
        }
        if self.log_lines == 0 {
            return bad("log_lines must be at least 1");
        }
        Ok(())
    }
}

/// How long a request's frame stays worth a pass: the engine drops an
/// answer about a frame older than `max_age`, so the host does not send a
/// request that has already waited that long (behind a restart, or behind
/// other sources' passes) and answers it with [`HostError::Expired`]
/// instead of spending a pass on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    /// When the frame was handed to the detector.
    pub taken: Instant,
    /// Oldest the frame may be when the pass starts.
    pub max_age: Duration,
}

impl Freshness {
    /// A frame taken now that stays useful for `max_age`.
    pub fn now(max_age: Duration) -> Self {
        Self {
            taken: Instant::now(),
            max_age,
        }
    }

    /// The frame's age at `now`.
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.taken)
    }

    /// `Ok` while a pass starting at `now` is still useful, else the
    /// [`HostError::Expired`] to answer with.
    pub fn check(&self, now: Instant) -> Result<(), HostError> {
        let waited = self.age(now);
        if waited > self.max_age {
            Err(HostError::Expired {
                waited,
                max_age: self.max_age,
            })
        } else {
            Ok(())
        }
    }
}

/// One detector pass to run.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectRequest {
    /// Source the frame came from.
    pub source: String,
    /// The frame's per-source sequence number.
    pub seq: u64,
    /// The frame's presentation time.
    pub pts: Duration,
    /// The prepared image ([`crate::prep::prepare`]).
    pub image: RgbImage,
    /// Prompts, with the score each needs; at least one.
    pub phrases: Vec<Phrase>,
    /// How long the frame is worth a pass; `None` sends it however long
    /// it waited.
    pub freshness: Option<Freshness>,
}

/// The answer to a [`DetectRequest`].
#[derive(Debug, Clone, PartialEq)]
pub struct DetectReply {
    /// Instances at or above their phrase's threshold; `phrase` indexes the
    /// request's phrases.
    pub detections: Vec<Found>,
    /// Time the sidecar reports for the pass.
    pub latency: Duration,
    /// Time from handing the request to the writer to the answer.
    pub round_trip: Duration,
}

/// Why the host could not answer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostError {
    /// A [`HostConfig`] value is out of range.
    #[error("invalid sidecar configuration: {0}")]
    Config(String),
    /// The request breaks the protocol's rules (nothing was sent).
    #[error("invalid sidecar request: {0}")]
    Invalid(String),
    /// The command could not be started.
    #[error("{0}")]
    Spawn(String),
    /// A deadline passed; the process was killed and will be restarted.
    #[error("{what} timed out after {after:?}")]
    Timeout {
        /// What was waited for, with the exit and last log line.
        what: String,
        /// The deadline.
        after: Duration,
    },
    /// The process is down: it just failed, or the host is waiting to
    /// restart it.
    #[error("the sidecar is unavailable: {reason}{}", retry_note(.retry_in))]
    Unavailable {
        /// Why the last process was lost.
        reason: String,
        /// Time until a request may start a new process, if it must wait.
        retry_in: Option<Duration>,
    },
    /// The sidecar answered the request with an error; it stays up.
    #[error("the sidecar rejected the request: {0}")]
    Rejected(String),
    /// The request was not sent: its frame had waited longer than its
    /// [`Freshness::max_age`] by the time the sidecar was free, so its
    /// answer would be thrown away. The process is fine.
    #[error(
        "skipped: the frame waited {:.1} s for the sidecar, longer than the {:.1} s a result may be old",
        .waited.as_secs_f64(),
        .max_age.as_secs_f64()
    )]
    Expired {
        /// How long the frame had waited.
        waited: Duration,
        /// The limit it passed.
        max_age: Duration,
    },
    /// The host was stopped ([`StopHandle::stop`]).
    #[error("the sidecar host was stopped")]
    Stopped,
}

/// "; retrying in 1.5 s", "; retrying in 40 ms", or nothing.
fn retry_note(retry_in: &Option<Duration>) -> String {
    match retry_in {
        Some(d) if *d < Duration::from_secs(1) => {
            format!("; retrying in {} ms", d.as_micros().div_ceil(1000))
        }
        Some(d) => format!("; retrying in {:.1} s", d.as_secs_f64()),
        None => String::new(),
    }
}

/// What the host is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostState {
    /// No process and no failure: not started yet, or shut down.
    Idle,
    /// A process is running and has not said hello yet.
    Starting,
    /// A process said hello and can take requests.
    Ready,
    /// The last process failed.
    Unavailable {
        /// Why, with its exit and last log line.
        reason: String,
        /// Earliest time a request may start a new process.
        retry_at: Option<Instant>,
    },
    /// Stopped for good ([`StopHandle::stop`]).
    Stopped,
}

/// Counters since the host was made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostStats {
    /// Processes started.
    pub spawns: u64,
    /// Processes lost to a failure (also counts failed spawns).
    pub failures: u64,
    /// Requests handed to a process.
    pub requests: u64,
    /// Results accepted.
    pub results: u64,
    /// Requests the sidecar answered with an error reply.
    pub rejected: u64,
    /// Startups and requests that missed their deadline.
    pub timeouts: u64,
    /// Replies dropped because they did not match the outstanding request.
    pub stale: u64,
    /// Requests refused while waiting to restart.
    pub refused: u64,
    /// Requests not sent because their frame was too old
    /// ([`HostError::Expired`]).
    pub expired: u64,
    /// Protocol errors (each also a failure).
    pub protocol_errors: u64,
    /// Pipe threads that did not end after their child died (a grandchild
    /// kept a pipe open) and were detached.
    pub detached_threads: u64,
}

/// A snapshot of the host for display.
#[derive(Debug, Clone, PartialEq)]
pub struct HostStatus {
    /// What the host is doing.
    pub state: HostState,
    /// The running child's process id.
    pub pid: Option<u32>,
    /// The running child's hello.
    pub hello: Option<Hello>,
    /// Counters.
    pub stats: HostStats,
    /// Failures since the last healthy answer.
    pub consecutive_failures: u32,
    /// Pass time the sidecar reported for the last result.
    pub last_latency: Option<Duration>,
    /// Round trip of the last result.
    pub last_round_trip: Option<Duration>,
}
