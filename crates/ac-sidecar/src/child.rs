//! One sidecar process and the three threads that serve its pipes.
//!
//! # Ownership
//!
//! A [`Process`] owns the [`Child`] and three threads:
//!
//! * the **writer** owns the child's stdin. It takes encoded frames from a
//!   bounded channel of capacity 1 (one request in flight, so a full
//!   channel means the child stopped reading) and writes them. Dropping
//!   the channel's sender ends the thread, which drops stdin: end of input
//!   is the child's signal to exit;
//! * the **reader** owns stdout. It decodes frames and forwards them as
//!   [`Event`]s, and reports end of stream or a protocol error as its last
//!   event;
//! * the **log drain** owns stderr and feeds the [`LogRing`].
//!
//! Events reach the owner of the `Process` on one bounded channel, so a
//! child that floods stdout blocks in its own write instead of growing the
//! host's memory. Nothing is shared but the channels and the log ring; no
//! lock is held across I/O.
//!
//! # Ending a process
//!
//! [`Process::finish`] closes stdin, gives the child `grace` to exit by
//! itself, then kills its whole process group and reaps it, and joins the
//! threads (detaching, and counting, any thread still blocked a second
//! later, which only a helper that left the group while holding a pipe
//! can cause). The child was spawned with `process_group(0)` (no `unsafe`
//! needed): it leads its own group, so a terminal's Ctrl-C does not reach
//! it behind the host's back, and killing the group also takes any helper
//! processes it started, which would otherwise hold its pipes open and
//! outlive the host.
//!
//! The group is signalled before the child is reaped when the host kills
//! it, so its id is still reserved. When the child exits by itself the
//! group is signalled just after the reap, to take helpers it left
//! behind: while any member lives the group id cannot be reused (POSIX),
//! and with none left it could only be reused if the pid counter wrapped
//! around within those microseconds. Dropping a `Process` that was not
//! finished kills it at once.
//!
//! # Invariants
//!
//! * After `finish` (or drop) the child has been waited on: no zombie and
//!   no live child remain.
//! * Every event channel sender belongs to one process; the receiver is
//!   dropped before the threads are joined, so a thread blocked on a full
//!   channel is released.

use std::io::{self, BufReader, BufWriter};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::logs::{LogRing, lock};
use crate::protocol::{Message, read_message, write_frame};

/// Capacity of the event channel from the pipe threads.
const EVENT_CAPACITY: usize = 4;

/// Poll interval while waiting for a child to exit.
const EXIT_POLL: Duration = Duration::from_millis(5);

/// How long to wait for a pipe thread to end once the child is dead.
const JOIN_LIMIT: Duration = Duration::from_secs(1);

/// Buffer size for the child's stdout.
const READ_BUFFER: usize = 64 * 1024;

/// What a pipe thread reports.
#[derive(Debug)]
pub(crate) enum Event {
    /// A decoded message from stdout.
    Message(Message),
    /// Stdout ended (the child exited or closed it).
    Closed,
    /// Stdout carried something that is not a valid frame; the reader
    /// stopped.
    Broken(String),
    /// The writer finished writing request `id`.
    Written(u64),
    /// Writing to stdin failed; the writer stopped.
    WriteFailed(String),
    /// A [`Wake`] was triggered: stop waiting.
    Interrupt,
}

/// An encoded frame for the writer.
#[derive(Debug)]
pub(crate) struct Outgoing {
    /// Request id, reported back in [`Event::Written`].
    pub(crate) id: u64,
    /// The JSON header.
    pub(crate) header: Vec<u8>,
    /// The payload.
    pub(crate) payload: Vec<u8>,
}

/// Why a frame could not be handed to the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendFailure {
    /// The previous frame is still being written: the child is not reading.
    Busy,
    /// The writer has stopped.
    Gone,
}

/// Lets another thread interrupt the host's waits for good (see
/// [`crate::host::StopHandle`]).
#[derive(Debug, Default)]
pub(crate) struct Wake {
    /// Set once; every later wait returns at once.
    stopped: AtomicBool,
    /// The current process's event sender, to wake a wait in progress.
    current: Mutex<Option<SyncSender<Event>>>,
}

impl Wake {
    /// Stops all waits, now and later.
    pub(crate) fn trigger(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let tx = lock(&self.current).clone();
        if let Some(tx) = tx {
            // A full channel wakes the waiter anyway; it checks the flag
            // after every event.
            let _ = tx.try_send(Event::Interrupt);
        }
    }

    /// True once triggered.
    pub(crate) fn is_set(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Installs (or clears) the sender a trigger wakes.
    fn install(&self, tx: Option<SyncSender<Event>>) {
        *lock(&self.current) = tx;
    }
}

/// How a process ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Exit {
    /// Its exit status, when it could be collected.
    pub(crate) status: Option<ExitStatus>,
    /// The host killed it.
    pub(crate) killed: bool,
    /// Pipe threads that did not end in time and were detached.
    pub(crate) detached: usize,
}

impl Exit {
    /// "exited with exit status: 3", "killed by the host", ...
    pub(crate) fn describe(&self) -> String {
        match (self.killed, self.status) {
            (true, _) => "killed by the host".into(),
            (false, Some(status)) => format!("exited with {status}"),
            (false, None) => "exit status unknown".into(),
        }
    }
}

/// A running sidecar process; see the module docs.
#[derive(Debug)]
pub(crate) struct Process {
    /// The child.
    child: Child,
    /// Its process id (also its process group id on Unix).
    pid: u32,
    /// Frames for the writer; `None` once stdin is being closed.
    to_writer: Option<SyncSender<Outgoing>>,
    /// Events from the pipe threads; `None` once finishing.
    events: Option<Receiver<Event>>,
    /// The pipe threads.
    threads: Vec<JoinHandle<()>>,
    /// The waker that knows this process's event sender.
    wake: Arc<Wake>,
    /// Set by [`Process::finish`].
    finished: bool,
}

impl Process {
    /// Starts `command` (program first) in `dir`, with all three standard
    /// streams piped, and its pipe threads.
    pub(crate) fn spawn(
        command: &[String],
        dir: Option<&std::path::Path>,
        logs: &LogRing,
        wake: &Arc<Wake>,
    ) -> io::Result<Self> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = dir {
            cmd.current_dir(dir);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn()?;
        let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (writer_tx, writer_rx) = sync_channel::<Outgoing>(1);
        let (events_tx, events_rx) = sync_channel::<Event>(EVENT_CAPACITY);
        let mut process = Self {
            pid: child.id(),
            child,
            to_writer: Some(writer_tx),
            events: Some(events_rx),
            threads: Vec::new(),
            wake: wake.clone(),
            finished: false,
        };
        // From here on an early return drops `process`, which kills the child.
        let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
            return Err(io::Error::other("the child's pipes are missing"));
        };
        let tx = events_tx.clone();
        process.threads.push(spawn_named("ac-sam-write", move || {
            write_loop(stdin, writer_rx, tx)
        })?);
        let tx = events_tx.clone();
        process
            .threads
            .push(spawn_named("ac-sam-read", move || read_loop(stdout, tx))?);
        let ring = logs.clone();
        process
            .threads
            .push(spawn_named("ac-sam-log", move || log_loop(stderr, ring))?);
        wake.install(Some(events_tx));
        Ok(process)
    }

    /// The child's process id.
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// Hands a frame to the writer without blocking.
    pub(crate) fn send(&self, out: Outgoing) -> Result<(), SendFailure> {
        let Some(tx) = &self.to_writer else {
            return Err(SendFailure::Gone);
        };
        tx.try_send(out).map_err(|e| match e {
            TrySendError::Full(_) => SendFailure::Busy,
            TrySendError::Disconnected(_) => SendFailure::Gone,
        })
    }

    /// The next event, waiting until `until`; `None` when the time passed.
    /// A channel whose senders are all gone reads as [`Event::Closed`].
    pub(crate) fn next_event(&self, until: Instant) -> Option<Event> {
        let events = self.events.as_ref()?;
        let left = until.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Event::Closed),
        }
    }

    /// An event that is already waiting, if any.
    pub(crate) fn try_event(&self) -> Option<Event> {
        self.events.as_ref()?.try_recv().ok()
    }

    /// Ends the process; see the module docs. Idempotent: later calls
    /// return a default [`Exit`].
    pub(crate) fn finish(&mut self, grace: Duration) -> Exit {
        if self.finished {
            return Exit::default();
        }
        self.finished = true;
        // Closing the writer's channel ends the writer, which drops stdin.
        self.to_writer = None;
        let deadline = Instant::now() + grace;
        let mut exit = Exit::default();
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    exit.status = Some(status);
                    // Helpers it left behind would hold its pipes open and
                    // outlive the host; see the module docs.
                    kill_group(self.pid);
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    // Late output is of no use now; draining it keeps the
                    // reader, and so the child's own writes, from blocking
                    // its exit.
                    while self.try_event().is_some() {}
                    thread::sleep(EXIT_POLL);
                }
                _ => {
                    kill_group(self.pid);
                    let _ = self.child.kill();
                    exit.status = self.child.wait().ok();
                    exit.killed = true;
                    break;
                }
            }
        }
        self.wake.install(None);
        // Releases a reader or writer blocked on a full event channel.
        self.events = None;
        exit.detached = join_within(std::mem::take(&mut self.threads), JOIN_LIMIT);
        exit
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.finish(Duration::ZERO);
    }
}

/// Starts a named thread.
fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name(name.into()).spawn(f)
}

/// Joins `threads`, waiting at most `limit` in all; returns how many were
/// still running and were detached.
fn join_within(threads: Vec<JoinHandle<()>>, limit: Duration) -> usize {
    let deadline = Instant::now() + limit;
    let mut detached = 0;
    for t in threads {
        while !t.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        if t.is_finished() {
            let _ = t.join();
        } else {
            detached += 1;
        }
    }
    detached
}

/// Kills process group `pid`, the child's (see the module docs). Best
/// effort: there is no safe system call for it in `std`, so this runs
/// `kill`; without it, killing the child itself still happens.
#[cfg(unix)]
fn kill_group(pid: u32) {
    let _ = Command::new("kill")
        .args(["-s", "KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// No process groups here; killing the child is all there is.
#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

/// The writer thread: writes each frame, reports it written, and closes
/// stdin when the channel closes.
fn write_loop(stdin: ChildStdin, frames: Receiver<Outgoing>, events: SyncSender<Event>) {
    let mut stdin = BufWriter::new(stdin);
    for out in frames {
        if let Err(e) = write_frame(&mut stdin, &out.header, &out.payload) {
            let _ = events.send(Event::WriteFailed(e.to_string()));
            return;
        }
        if events.send(Event::Written(out.id)).is_err() {
            return;
        }
    }
}

/// The reader thread: forwards messages until stdout ends or breaks.
fn read_loop(stdout: ChildStdout, events: SyncSender<Event>) {
    let mut stdout = BufReader::with_capacity(READ_BUFFER, stdout);
    loop {
        let (event, last) = match read_message(&mut stdout) {
            Ok(Some((message, _payload))) => (Event::Message(message), false),
            Ok(None) => (Event::Closed, true),
            Err(e) => (Event::Broken(e.to_string()), true),
        };
        if events.send(event).is_err() || last {
            return;
        }
    }
}

/// The log drain thread.
fn log_loop(stderr: ChildStderr, logs: LogRing) {
    logs.drain(BufReader::new(stderr));
}
