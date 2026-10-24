//! [`SidecarService`]: one sidecar process shared by every source's
//! detector.
//!
//! # Theory
//!
//! SAM 3.1 peaks at about 3.3 GiB at 672 px (measured, `sam31-vs-owlv2.md`)
//! and saturates the GPU, so a studio runs one sidecar process, not one per
//! camera (design §4.6, "Shared host"). The engine, though, gives each
//! source its own detector on its own discovery thread. The service joins
//! the two without a lock held across I/O: a dedicated thread
//! (`ac-sam-host`) owns the [`SamHost`] and serves jobs from a channel in
//! arrival order, and every [`SidecarDetector`] is a client that sends a
//! job and waits for its reply.
//!
//! Each detector blocks in [`Detector::detect`](ac_vision::Detector) until
//! its reply comes, so it has at most one job queued: the queue is bounded
//! by the number of sources, and the oldest dispatch is served first.
//! Deadlines count from when a job reaches the sidecar, not from when it
//! was queued.
//!
//! A queue alone would break the latest-frame rule: a frame is fixed when
//! its job is queued, so with N sources each answer would come back about
//! N passes old (two sources at 1.05 s a pass: 2.1 s, past the engine's
//! 2 s age limit, so every answer would be thrown away while the sidecar
//! ran flat out). Two things keep frames fresh instead:
//!
//! * the engine keeps one request in flight for the whole studio when its
//!   detector is shared, taking the sources in turn, each with its newest
//!   frame (`ac-engine`'s core), so the queue normally holds one job;
//! * a job whose frame has waited longer than its
//!   [`Freshness`](crate::host::Freshness) allows when it reaches the head
//!   (behind a restart, or behind a request the engine gave up on) is
//!   answered with [`HostError::Expired`] without a pass.
//!
//! # Lifecycle
//!
//! [`SidecarService::start`] spawns the process on the caller's thread (a
//! wrong command fails here, at engine start), so the model loads while the
//! rest of the engine starts. [`SidecarClient::detector`] waits for the
//! first hello, so a detector made after a good start is named after its
//! model and knows its input side. A first start that fails (a missed startup deadline, an exit
//! before hello, a refused restart) does not fail it: the host restarts
//! the process under its backoff, so a cold first load or one crash during
//! load does not turn detection off for the session. The detector is
//! returned anyway, named [`UNNAMED`], and its passes fail with the reason
//! (the host's backoff refusal names the failed start) until a restart
//! says hello. Only an invalid configuration, or a service that is gone,
//! fails it. Later failures are the host's: kill, reap, refuse while
//! backing off, restart on a later request.
//!
//! Dropping the service stops it whatever clients remain: it interrupts
//! the wait in progress, the thread shuts the process down (stdin closed,
//! 2 s grace, then kill and reap) and exits, and the drop joins it. Jobs
//! still queued, and every later call of a client, fail with
//! `Unavailable`.
//!
//! # Invariants
//!
//! * The service is the process's only owner: a client can only queue
//!   jobs, and no process outlives the service.
//! * Status is published as a whole snapshot after every job; the board's
//!   lock is held only to replace or copy it.

use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ac_vision::VisionError;

use crate::detector::{DetectorConfig, SidecarDetector};
use crate::host::{
    DetectReply, DetectRequest, HostConfig, HostError, HostStatus, SamHost, StopHandle,
};
use crate::logs::{LogRing, lock};
use crate::protocol::Hello;

/// The name of a detector made while the sidecar had not said hello (its
/// first start failed; see the module docs).
pub const UNNAMED: &str = "sidecar";

/// Work for the service thread.
enum Job {
    /// Answer with the hello, starting a process if needed.
    Ready(SyncSender<Result<Hello, HostError>>),
    /// Run one pass.
    Detect {
        /// The pass.
        request: DetectRequest,
        /// Its deadline once sent.
        timeout: Duration,
        /// Where the answer goes.
        reply: SyncSender<Result<DetectReply, HostError>>,
    },
    /// End the thread.
    Stop,
}

/// Owner of the shared sidecar process; see the module docs.
#[derive(Debug)]
pub struct SidecarService {
    /// Jobs for the service thread.
    jobs: Sender<Job>,
    /// Interrupts the host's waits.
    stop: StopHandle,
    /// The service thread.
    thread: Option<JoinHandle<()>>,
    /// The newest status snapshot.
    board: Arc<Mutex<HostStatus>>,
    /// The child's recent stderr lines.
    logs: LogRing,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Job::Ready(_) => "Ready",
            Job::Detect { .. } => "Detect",
            Job::Stop => "Stop",
        })
    }
}

impl SidecarService {
    /// Spawns the sidecar and the service thread; see the module docs.
    pub fn start(config: HostConfig) -> Result<Self, HostError> {
        let mut host = SamHost::new(config)?;
        host.start()?;
        let board = Arc::new(Mutex::new(host.status()));
        let (jobs, inbox) = channel();
        let stop = host.stop_handle();
        let logs = host.log_ring();
        let published = board.clone();
        let thread = thread::Builder::new()
            .name("ac-sam-host".into())
            .spawn(move || serve(host, inbox, published))
            .map_err(|e| HostError::Spawn(format!("cannot start the service thread: {e}")))?;
        Ok(Self {
            jobs,
            stop,
            thread: Some(thread),
            board,
            logs,
        })
    }

    /// A handle for making detectors, which may be moved to other threads.
    pub fn client(&self) -> SidecarClient {
        SidecarClient {
            jobs: self.jobs.clone(),
        }
    }

    /// Shorthand for `self.client().detector(config)`.
    pub fn detector(&self, config: DetectorConfig) -> Result<SidecarDetector, VisionError> {
        self.client().detector(config)
    }

    /// The newest status snapshot (published after every job).
    pub fn status(&self) -> HostStatus {
        lock(&self.board).clone()
    }

    /// The child's recent stderr lines, oldest first, with the host's
    /// `[host]` notes.
    pub fn logs(&self) -> Vec<String> {
        self.logs.lines()
    }

    /// Stops the service and its process; the same as dropping it.
    pub fn shutdown(self) {}
}

impl Drop for SidecarService {
    fn drop(&mut self) {
        self.stop.stop();
        let _ = self.jobs.send(Job::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A non-owning handle to a [`SidecarService`]; cheap to clone. Calls fail
/// with `Unavailable` once the service is gone.
#[derive(Debug, Clone)]
pub struct SidecarClient {
    /// Jobs for the service thread.
    jobs: Sender<Job>,
}

impl SidecarClient {
    /// The sidecar's hello, waiting for it if the model is still loading.
    pub fn ready(&self) -> Result<Hello, HostError> {
        let (reply, answer) = sync_channel(1);
        self.jobs
            .send(Job::Ready(reply))
            .map_err(|_| HostError::Stopped)?;
        answer.recv().unwrap_or(Err(HostError::Stopped))
    }

    /// Runs one pass on the shared process.
    pub fn detect(
        &self,
        request: DetectRequest,
        timeout: Duration,
    ) -> Result<DetectReply, HostError> {
        let (reply, answer) = sync_channel(1);
        self.jobs
            .send(Job::Detect {
                request,
                timeout,
                reply,
            })
            .map_err(|_| HostError::Stopped)?;
        answer.recv().unwrap_or(Err(HostError::Stopped))
    }

    /// A detector on the shared process, made once the first start has
    /// said hello or failed; see the module docs for why a failed start
    /// still yields one.
    pub fn detector(&self, config: DetectorConfig) -> Result<SidecarDetector, VisionError> {
        config.validate()?;
        let (name, input_side) = match self.ready() {
            Ok(hello) => (hello.model, Some(hello.input_side)),
            Err(e @ (HostError::Config(_) | HostError::Invalid(_) | HostError::Stopped)) => {
                return Err(e.into());
            }
            Err(HostError::Spawn(_))
            | Err(HostError::Timeout { .. })
            | Err(HostError::Unavailable { .. })
            | Err(HostError::Rejected(_))
            | Err(HostError::Expired { .. }) => (UNNAMED.to_owned(), None),
        };
        Ok(SidecarDetector::new(name, input_side, self.clone(), config))
    }
}

/// The service thread: serves jobs in order until stopped, publishing the
/// status after each. The first `Ready` job waits for the hello of the
/// process [`SidecarService::start`] spawned, so its caller learns how the
/// startup went.
fn serve(mut host: SamHost, inbox: Receiver<Job>, board: Arc<Mutex<HostStatus>>) {
    let publish = |host: &SamHost| *lock(&board) = host.status();
    for job in inbox.iter() {
        match job {
            Job::Stop => break,
            Job::Ready(reply) => {
                let answer = host.ready();
                publish(&host);
                let _ = reply.send(answer);
            }
            Job::Detect {
                request,
                timeout,
                reply,
            } => {
                let answer = host.detect(request, timeout);
                publish(&host);
                let _ = reply.send(answer);
            }
        }
    }
    host.shutdown();
    publish(&host);
}
