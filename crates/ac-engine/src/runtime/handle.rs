//! The caller's side of a running engine: [`EngineHandle`] posts commands
//! and reads snapshots, [`PendingReply`] waits for one command's answer.
//!
//! A handle never touches engine state. Commands travel to the engine
//! thread over an `mpsc` channel, each in an [`Envelope`] with its own
//! reply channel, and are applied in order at the start of the next tick;
//! snapshots come back through the latest-value mailbox, so a reader never
//! blocks the engine and never sees a half-updated state. Once the engine
//! is gone, sends fail quietly and every wait reports
//! [`CommandError::Unavailable`].

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use super::COMMAND_TIMEOUT;
use crate::command::{CommandError, CommandReply, EngineCommand};
use crate::mailbox::Latest;
use crate::snapshot::EngineSnapshot;

/// A command with an optional reply channel.
pub(super) struct Envelope {
    /// The command.
    pub(super) command: EngineCommand,
    /// Where to send the result.
    pub(super) reply: Option<Sender<Result<CommandReply, CommandError>>>,
}

/// The answer to a command sent with [`EngineHandle::post`].
#[derive(Debug)]
pub struct PendingReply {
    /// The reply channel.
    rx: Receiver<Result<CommandReply, CommandError>>,
}

impl PendingReply {
    /// The answer if it has arrived, without waiting.
    pub fn try_get(&self) -> Option<Result<CommandReply, CommandError>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err(CommandError::Unavailable("engine stopped")))
            }
        }
    }

    /// Waits up to `timeout` for the answer.
    pub fn wait(self, timeout: Duration) -> Result<CommandReply, CommandError> {
        match self.rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => Err(CommandError::Unavailable("no answer in time")),
            Err(RecvTimeoutError::Disconnected) => Err(CommandError::Unavailable("engine stopped")),
        }
    }
}

/// A cloneable handle for sending commands and reading snapshots.
#[derive(Clone)]
pub struct EngineHandle {
    /// Command channel.
    pub(super) tx: Sender<Envelope>,
    /// Snapshot mailbox.
    pub(super) snapshots: Arc<Latest<Arc<EngineSnapshot>>>,
    /// The snapshot published before the engine thread started.
    pub(super) initial: Arc<EngineSnapshot>,
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("snapshot_version", &self.snapshots.version())
            .finish_non_exhaustive()
    }
}

impl EngineHandle {
    /// Sends `command` and returns a handle to its answer.
    pub fn post(&self, command: EngineCommand) -> PendingReply {
        let (reply, rx) = channel();
        // A send error means the engine is gone; the reply then reports it.
        let _ = self.tx.send(Envelope {
            command,
            reply: Some(reply),
        });
        PendingReply { rx }
    }

    /// Sends `command` and waits up to [`COMMAND_TIMEOUT`] for the answer.
    pub fn send(&self, command: EngineCommand) -> Result<CommandReply, CommandError> {
        self.post(command).wait(COMMAND_TIMEOUT)
    }

    /// The newest snapshot.
    pub fn snapshot(&self) -> Arc<EngineSnapshot> {
        self.snapshots.get().unwrap_or_else(|| self.initial.clone())
    }

    /// Version of the newest snapshot (increases by one per publish).
    pub fn snapshot_version(&self) -> u64 {
        self.snapshots.version()
    }

    /// Polls snapshots until `done` holds or `timeout` (real time) passes.
    pub fn wait_until(
        &self,
        timeout: Duration,
        done: impl Fn(&EngineSnapshot) -> bool,
    ) -> Option<Arc<EngineSnapshot>> {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.snapshot();
            if done(&s) {
                return Some(s);
            }
            if Instant::now() >= deadline || !s.running {
                return None;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
