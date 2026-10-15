//! [`SamHost`]: the single owner of one sidecar process, with deadlines,
//! stale-reply dropping and restarts.
//!
//! # Lifecycle
//!
//! ```text
//!        start / first request                hello
//! Idle ─────────────────────────▶ Starting ───────────▶ Ready ◀──┐
//!   ▲                               │                     │ detect │ result / error reply
//!   │                               │ deadline, exit,     └────────┘
//!   │ backoff passed and            │ protocol error         │ deadline, exit, protocol
//!   │ a request arrives             ▼                        │ error, fatal error
//!   └──────────────────────── Unavailable ◀──────────────────┘
//! ```
//!
//! * **Start.** The host spawns the command with stdin, stdout and stderr
//!   piped (`child` module) and waits for `hello`, at most
//!   [`HostConfig::startup_timeout`] (60 s by default; SAM 3.1 loads in
//!   4.6 s, measured) counted from the spawn.
//! * **Requests.** One at a time: [`SamHost::detect`] takes `&mut self`,
//!   so the type system enforces the protocol's one-in-flight rule. Each
//!   request gets a fresh id and its own deadline, counted from when it is
//!   handed to the writer. The answer must echo the request's id, source,
//!   sequence number and PTS; a reply that does not is dropped and counted
//!   in [`HostStats::stale`], and the wait goes on. An `error` reply with
//!   the request's id fails that request only; the process stays up.
//! * **Failure.** A missed deadline (startup or request), the child
//!   exiting, a protocol error or a fatal `error` (id null) all end the
//!   process: it is killed (after a short grace when it is exiting
//!   anyway, to collect its exit status), reaped, its threads joined, and
//!   the request fails with the reason, which names the exit and the
//!   child's last stderr line. The host is then *unavailable*: requests are
//!   refused at once until the backoff delay passes (`backoff` module),
//!   and the first request after it starts a new process.
//! * **Shutdown.** [`SamHost::shutdown`] and `Drop` close stdin (the
//!   child's signal to exit), wait up to [`HostConfig::exit_grace`] (2 s by
//!   default), then kill the process group and reap. [`StopHandle::stop`]
//!   interrupts a wait in progress from another thread and makes the host
//!   refuse further work, so quitting never waits out a 60 s startup.
//!
//! # Ownership
//!
//! The host owns the `Process` (and through it the child and its pipe
//! threads), the restart state and the counters; nothing else can touch
//! them. The pipe threads report through a bounded channel the host reads
//! only while it waits. The stderr ring is shared with the log drain under
//! a lock held only to push or copy lines. No lock is held across I/O.
//!
//! # Invariants
//!
//! * At most one child exists per host, and none outlives it.
//! * A result is returned only for the request it answers.
//! * Every child exit or kill is followed by a wait: no zombies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::backoff::Backoff;
use crate::child::{Event, Outgoing, Process, SendFailure, Wake};
use crate::logs::LogRing;
use crate::protocol::{
    Detect, ErrorReply, Hello, Message, PixelFormat, check_payload, encode_header,
};

mod types;

pub use types::{
    DetectReply, DetectRequest, HostConfig, HostError, HostState, HostStats, HostStatus,
};

/// How long a child that is exiting by itself (its stdout closed, or it
/// sent a fatal error) gets before it is killed, so its exit status can
/// be reported.
const EXITING_GRACE: Duration = Duration::from_millis(200);

/// Prefix of the lines the host itself adds to the log ring.
const HOST_LINE: &str = "[host]";

/// Stops a [`SamHost`] from another thread: interrupts its current wait
/// and makes it refuse further work. Cheap to clone.
#[derive(Debug, Clone)]
pub struct StopHandle {
    /// The host's waker.
    wake: Arc<Wake>,
}

impl StopHandle {
    /// Stops the host for good. Idempotent. The process itself is ended by
    /// the host's owner ([`SamHost::shutdown`] or drop).
    pub fn stop(&self) {
        self.wake.trigger();
    }

    /// True once [`StopHandle::stop`] was called.
    pub fn is_stopped(&self) -> bool {
        self.wake.is_set()
    }
}

/// The outcome of one wait.
enum Wait {
    /// An event arrived.
    Event(Event),
    /// The deadline passed.
    Timeout,
    /// The host was stopped.
    Stopped,
}

/// Owner of one sidecar process; see the module docs.
#[derive(Debug)]
pub struct SamHost {
    /// How to run the sidecar.
    config: HostConfig,
    /// The running process.
    process: Option<Process>,
    /// When the running process was spawned.
    started: Instant,
    /// The running process's hello, once received.
    hello: Option<Hello>,
    /// Restart pacing.
    backoff: Backoff,
    /// Why the last process was lost, until a healthy answer.
    failure: Option<String>,
    /// Id of the next request.
    next_id: u64,
    /// Counters.
    stats: HostStats,
    /// Pass time of the last result.
    last_latency: Option<Duration>,
    /// Round trip of the last result.
    last_round_trip: Option<Duration>,
    /// The child's recent stderr lines (and the host's own notes).
    logs: LogRing,
    /// Interrupts waits; shared with [`StopHandle`]s.
    wake: Arc<Wake>,
}

impl SamHost {
    /// A host for `config`; no process is started yet.
    pub fn new(config: HostConfig) -> Result<Self, HostError> {
        config.validate()?;
        Ok(Self {
            backoff: Backoff::new(config.restart_backoff, config.max_restart_backoff),
            logs: LogRing::new(config.log_lines),
            config,
            process: None,
            started: Instant::now(),
            hello: None,
            failure: None,
            next_id: 1,
            stats: HostStats::default(),
            last_latency: None,
            last_round_trip: None,
            wake: Arc::new(Wake::default()),
        })
    }

    /// The configuration.
    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// A handle that stops this host from another thread.
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle {
            wake: self.wake.clone(),
        }
    }

    /// The child's recent stderr lines, oldest first, with the host's own
    /// `[host]` notes on starts and exits.
    pub fn logs(&self) -> Vec<String> {
        self.logs.lines()
    }

    /// The log ring itself, for readers on other threads.
    pub(crate) fn log_ring(&self) -> LogRing {
        self.logs.clone()
    }

    /// The running child's process id.
    pub fn pid(&self) -> Option<u32> {
        self.process.as_ref().map(Process::pid)
    }

    /// A snapshot for display.
    pub fn status(&self) -> HostStatus {
        let state = if self.wake.is_set() {
            HostState::Stopped
        } else {
            match (&self.process, &self.hello, &self.failure) {
                (Some(_), Some(_), _) => HostState::Ready,
                (Some(_), None, _) => HostState::Starting,
                (None, _, Some(reason)) => HostState::Unavailable {
                    reason: reason.clone(),
                    retry_at: self.backoff.retry_at(),
                },
                (None, _, None) => HostState::Idle,
            }
        };
        HostStatus {
            state,
            pid: self.pid(),
            hello: self.hello.clone(),
            stats: self.stats,
            consecutive_failures: self.backoff.failures(),
            last_latency: self.last_latency,
            last_round_trip: self.last_round_trip,
        }
    }

    /// Starts a process now, without waiting for its hello, unless one is
    /// running. Ignores the restart backoff: this is an explicit start.
    pub fn start(&mut self) -> Result<(), HostError> {
        self.check_stopped()?;
        if self.process.is_none() {
            self.spawn()?;
        }
        Ok(())
    }

    /// The running process's hello, starting a process (when the backoff
    /// allows) and waiting for its hello when needed. A process found dead
    /// meanwhile is reported as that failure.
    pub fn ready(&mut self) -> Result<Hello, HostError> {
        self.check_stopped()?;
        self.drain_idle()?;
        if self.process.is_some()
            && let Some(hello) = &self.hello
        {
            return Ok(hello.clone());
        }
        if self.process.is_none() {
            if let Some(left) = self.backoff.wait_left(Instant::now()) {
                self.stats.refused += 1;
                return Err(HostError::Unavailable {
                    reason: self.failure.clone().unwrap_or_default(),
                    retry_in: Some(left),
                });
            }
            self.spawn()?;
        }
        self.await_hello()
    }

    /// Runs one detector pass; see the module docs. `timeout` counts from
    /// when the request is handed to the writer.
    pub fn detect(
        &mut self,
        request: DetectRequest,
        timeout: Duration,
    ) -> Result<DetectReply, HostError> {
        self.check_stopped()?;
        if timeout.is_zero() {
            return Err(HostError::Invalid("the request timeout is zero".into()));
        }
        let DetectRequest {
            source,
            seq,
            pts,
            image,
            phrases,
        } = request;
        let detect = Detect {
            id: self.next_id,
            source,
            seq,
            pts_ns: u64::try_from(pts.as_nanos()).unwrap_or(u64::MAX),
            width: image.width,
            height: image.height,
            format: PixelFormat::Rgb8,
            phrases,
            want_masks: false,
        };
        let message = Message::Detect(detect.clone());
        let invalid = |e: crate::protocol::ProtocolError| HostError::Invalid(e.to_string());
        let header = encode_header(&message).map_err(invalid)?;
        check_payload(&message, image.data.len()).map_err(invalid)?;
        self.ready()?;
        self.next_id += 1;
        let sent = Instant::now();
        let outgoing = Outgoing {
            id: detect.id,
            header,
            payload: image.data,
        };
        match self.process.as_ref().map(|p| p.send(outgoing)) {
            Some(Ok(())) => {}
            Some(Err(SendFailure::Busy)) => {
                return Err(self.lost("the sidecar is not reading its input", Duration::ZERO));
            }
            Some(Err(SendFailure::Gone)) | None => {
                return Err(self.lost("the sidecar's input is closed", EXITING_GRACE));
            }
        }
        self.stats.requests += 1;
        self.await_result(&detect, sent, timeout)
    }

    /// Waits for the answer to `detect`, sent at `sent`.
    fn await_result(
        &mut self,
        detect: &Detect,
        sent: Instant,
        timeout: Duration,
    ) -> Result<DetectReply, HostError> {
        let deadline = sent + timeout;
        let mut written = false;
        loop {
            let event = match self.next(deadline) {
                Wait::Event(event) => event,
                Wait::Stopped => return Err(HostError::Stopped),
                Wait::Timeout => {
                    self.stats.timeouts += 1;
                    let what = if written {
                        "the sidecar's result"
                    } else {
                        "the sidecar reading its request"
                    };
                    let detail = self.fail(what.into(), Duration::ZERO);
                    return Err(HostError::Timeout {
                        what: detail,
                        after: timeout,
                    });
                }
            };
            match event {
                Event::Written(id) => written |= id == detect.id,
                Event::Message(Message::Result(r)) => {
                    let answers = r.id == detect.id
                        && r.source == detect.source
                        && r.seq == detect.seq
                        && r.pts_ns == detect.pts_ns;
                    if !answers {
                        self.stats.stale += 1;
                        continue;
                    }
                    let n = detect.phrases.len();
                    if let Some(bad) = r.detections.iter().find(|f| f.phrase as usize >= n) {
                        self.stats.protocol_errors += 1;
                        let reason = format!(
                            "protocol error: a detection names phrase {} of a request with {n}",
                            bad.phrase
                        );
                        return Err(self.lost(&reason, Duration::ZERO));
                    }
                    let detections = r
                        .detections
                        .into_iter()
                        .filter(|f| f.score >= detect.phrases[f.phrase as usize].threshold)
                        .collect();
                    let latency =
                        Duration::try_from_secs_f64(r.latency_ms / 1000.0).unwrap_or(Duration::MAX);
                    let round_trip = sent.elapsed();
                    self.healthy();
                    self.stats.results += 1;
                    self.last_latency = Some(latency);
                    self.last_round_trip = Some(round_trip);
                    return Ok(DetectReply {
                        detections,
                        latency,
                        round_trip,
                    });
                }
                Event::Message(Message::Error(ErrorReply {
                    id: Some(id),
                    message,
                })) => {
                    if id == detect.id {
                        self.healthy();
                        self.stats.rejected += 1;
                        return Err(HostError::Rejected(message));
                    }
                    self.stats.stale += 1;
                }
                event => return Err(self.unexpected(event, "during a request")),
            }
        }
    }

    /// Waits for the running process's hello, until its startup deadline.
    fn await_hello(&mut self) -> Result<Hello, HostError> {
        let deadline = self.started + self.config.startup_timeout;
        loop {
            let event = match self.next(deadline) {
                Wait::Event(event) => event,
                Wait::Stopped => return Err(HostError::Stopped),
                Wait::Timeout => {
                    self.stats.timeouts += 1;
                    let detail = self.fail("the sidecar's hello".into(), Duration::ZERO);
                    return Err(HostError::Timeout {
                        what: detail,
                        after: self.config.startup_timeout,
                    });
                }
            };
            match event {
                Event::Message(Message::Hello(hello)) => {
                    self.note_hello(&hello);
                    self.hello = Some(hello.clone());
                    return Ok(hello);
                }
                Event::Written(_) => {}
                event => return Err(self.unexpected(event, "before hello")),
            }
        }
    }

    /// Handles events that arrived while no one waited: accepts the hello
    /// of a process started with [`SamHost::start`], counts late replies
    /// as stale, and notices a process that died meanwhile, returning the
    /// error for it.
    fn drain_idle(&mut self) -> Result<(), HostError> {
        while let Some(event) = self.process.as_ref().and_then(Process::try_event) {
            match event {
                Event::Message(Message::Hello(hello)) if self.hello.is_none() => {
                    self.note_hello(&hello);
                    self.hello = Some(hello);
                }
                Event::Message(Message::Result(_))
                | Event::Message(Message::Error(ErrorReply { id: Some(_), .. })) => {
                    self.stats.stale += 1;
                }
                Event::Written(_) | Event::Interrupt => {}
                event => {
                    let when = if self.hello.is_none() {
                        "before hello"
                    } else {
                        "while idle"
                    };
                    return Err(self.unexpected(event, when));
                }
            }
        }
        Ok(())
    }

    /// Ends the process for an event that has no place `when`, and returns
    /// the error for it.
    fn unexpected(&mut self, event: Event, when: &str) -> HostError {
        match event {
            Event::Message(Message::Error(ErrorReply { id: None, message })) => {
                self.lost(&format!("the sidecar failed: {message}"), EXITING_GRACE)
            }
            Event::Message(message) => {
                self.stats.protocol_errors += 1;
                let reason = format!("protocol error: unexpected {} {when}", message.kind());
                self.lost(&reason, Duration::ZERO)
            }
            Event::Closed => self.lost(&format!("the sidecar exited {when}"), EXITING_GRACE),
            Event::Broken(e) => {
                self.stats.protocol_errors += 1;
                self.lost(&format!("protocol error {when}: {e}"), Duration::ZERO)
            }
            Event::WriteFailed(e) => {
                self.lost(&format!("cannot write to the sidecar: {e}"), EXITING_GRACE)
            }
            // Every wait handles these itself; they never end a process.
            Event::Written(_) | Event::Interrupt => HostError::Stopped,
        }
    }

    /// The next event of the running process, until `until`.
    fn next(&self, until: Instant) -> Wait {
        if self.wake.is_set() {
            return Wait::Stopped;
        }
        let Some(process) = &self.process else {
            return Wait::Event(Event::Closed);
        };
        let event = process.next_event(until);
        if self.wake.is_set() {
            return Wait::Stopped;
        }
        match event {
            Some(Event::Interrupt) => Wait::Stopped,
            Some(event) => Wait::Event(event),
            None => Wait::Timeout,
        }
    }

    /// Starts a process; a failure counts like a lost process.
    fn spawn(&mut self) -> Result<(), HostError> {
        let dir = self.config.current_dir.as_deref();
        match Process::spawn(&self.config.command, dir, &self.logs, &self.wake) {
            Ok(process) => {
                self.logs.push(format!(
                    "{HOST_LINE} started pid {}: {}",
                    process.pid(),
                    self.config.command.join(" ")
                ));
                self.stats.spawns += 1;
                self.started = Instant::now();
                self.hello = None;
                self.process = Some(process);
                Ok(())
            }
            Err(e) => {
                let reason = format!("cannot start {:?}: {e}", self.config.command[0]);
                self.stats.failures += 1;
                self.backoff.fail(Instant::now());
                self.failure = Some(reason.clone());
                Err(HostError::Spawn(reason))
            }
        }
    }

    /// Logs a hello.
    fn note_hello(&self, hello: &Hello) {
        self.logs.push(format!(
            "{HOST_LINE} hello: model {}, input side {}, weights {}",
            hello.model,
            hello.input_side,
            hello.weights_sha256.as_deref().unwrap_or("unchecked")
        ));
    }

    /// Records a healthy answer.
    fn healthy(&mut self) {
        self.backoff.reset();
        self.failure = None;
    }

    /// Ends the running process for `reason` and returns
    /// [`HostError::Unavailable`] with the full reason.
    fn lost(&mut self, reason: &str, grace: Duration) -> HostError {
        let reason = self.fail(reason.into(), grace);
        HostError::Unavailable {
            reason,
            retry_in: self.backoff.wait_left(Instant::now()),
        }
    }

    /// Ends the running process (after `grace` for it to exit by itself),
    /// records the failure and returns `reason` with the exit and the
    /// child's last log line.
    fn fail(&mut self, reason: String, grace: Duration) -> String {
        let mut context = Vec::new();
        if let Some(mut process) = self.process.take() {
            let exit = process.finish(grace);
            self.stats.detached_threads += exit.detached as u64;
            context.push(format!("pid {} {}", process.pid(), exit.describe()));
        }
        if let Some(line) = last_child_line(&self.logs.lines()) {
            context.push(format!("last log line: {line:?}"));
        }
        let reason = if context.is_empty() {
            reason
        } else {
            format!("{reason} ({})", context.join("; "))
        };
        self.logs.push(format!("{HOST_LINE} lost: {reason}"));
        self.hello = None;
        self.stats.failures += 1;
        self.backoff.fail(Instant::now());
        self.failure = Some(reason.clone());
        reason
    }

    /// Refuses work once stopped.
    fn check_stopped(&self) -> Result<(), HostError> {
        if self.wake.is_set() {
            Err(HostError::Stopped)
        } else {
            Ok(())
        }
    }

    /// Ends the running process, if any: closes its stdin, waits up to
    /// [`HostConfig::exit_grace`], then kills and reaps it. A later request
    /// starts a new one (unless the host was stopped). Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(mut process) = self.process.take() {
            let exit = process.finish(self.config.exit_grace);
            self.stats.detached_threads += exit.detached as u64;
            self.logs.push(format!(
                "{HOST_LINE} pid {} {} at shutdown",
                process.pid(),
                exit.describe()
            ));
        }
        self.hello = None;
    }
}

impl Drop for SamHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The newest line the current process wrote to stderr: lines after the
/// last `[host] started` note, skipping the host's own notes.
fn last_child_line(lines: &[String]) -> Option<&String> {
    lines
        .iter()
        .rev()
        .take_while(|l| !l.starts_with(&format!("{HOST_LINE} started")))
        .find(|l| !l.starts_with(HOST_LINE))
}

#[cfg(test)]
mod tests;
