//! The host's waits: for a hello, for the answer to a request, and for
//! events that arrived while no one waited.
//!
//! # Theory
//!
//! The pipe threads report on one bounded channel ([`Event`]s), and only
//! the host's owner reads it, inside these waits. Every wait has a
//! deadline (the startup deadline for a hello, the request's own for an
//! answer) and ends at once when the host is stopped. An event that has
//! no place in the current wait ends the process ([`SamHost::unexpected`]):
//! the stream can no longer be trusted. A reply for another request (a late
//! answer to one that was given up) is counted as stale and the wait goes
//! on.
//!
//! # Invariants
//!
//! * A wait returns a result only for the request it waits for.
//! * Every path that ends a process goes through `SamHost::lost` or
//!   `SamHost::fail`, which reap it.

use std::time::{Duration, Instant};

use super::{DetectReply, EXITING_GRACE, HostError, SamHost};
use crate::child::{Event, Process};
use crate::protocol::{Detect, ErrorReply, Hello, Message};

/// The outcome of one wait.
enum Wait {
    /// An event arrived.
    Event(Event),
    /// The deadline passed.
    Timeout,
    /// The host was stopped.
    Stopped,
}

impl SamHost {
    /// Waits for the answer to `detect`, sent at `sent`.
    pub(super) fn await_result(
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
    pub(super) fn await_hello(&mut self) -> Result<Hello, HostError> {
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
    pub(super) fn drain_idle(&mut self) -> Result<(), HostError> {
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
    pub(super) fn unexpected(&mut self, event: Event, when: &str) -> HostError {
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
}
