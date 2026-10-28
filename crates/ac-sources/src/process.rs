//! Ownership of decoder child processes.
//!
//! Every ffmpeg process a source starts is registered in its [`Children`], so
//! the source can always find and kill all of them. Killing a child is also how
//! a stop request reaches a worker blocked in `read` on that child's stdout:
//! the pipe closes and the read returns.
//!
//! Invariants:
//!
//! * After [`Children::close`] no registered child is alive, and a spawn that
//!   races with the close kills its own newborn child before returning, so no
//!   process can outlive the source.
//! * Every child is waited on after it exits or is killed, so none is left as a
//!   zombie.
//! * The registry lock is held only to move `Child` handles in and out (and
//!   for non-blocking `try_wait`); spawning, killing and blocking waits happen
//!   outside it.
//! * Every child's stderr goes to the app's log, through a drain thread
//!   that ends with the child ([`crate::logs`]).

use std::io::{self, Read};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::status::lock;

/// How long a child that closed its stdout may take to exit before it is
/// killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// Poll interval while waiting for a child to exit.
const EXIT_POLL: Duration = Duration::from_millis(5);

/// A child process started through [`Children::spawn`].
#[derive(Debug)]
pub(crate) struct Spawned {
    /// Registry key, used to finish or kill the child.
    pub(crate) key: u64,
    /// The child's standard output.
    pub(crate) stdout: ChildStdout,
}

/// Registry of a source's live child processes; see the module docs.
#[derive(Debug, Default)]
pub(crate) struct Children {
    /// Registered children and whether new ones are still allowed.
    inner: Mutex<Registry>,
}

/// Contents of [`Children`].
#[derive(Debug, Default)]
struct Registry {
    /// Set by [`Children::close`]; no child may be registered afterwards.
    closed: bool,
    /// Key for the next registered child.
    next_key: u64,
    /// Live children by key.
    live: Vec<(u64, Child)>,
}

impl Children {
    /// An empty, open registry.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Starts `command` with stdin closed, stdout piped and stderr piped to
    /// the log, and registers it. `Ok(None)` when the registry is closed,
    /// in which case no process is left running.
    pub(crate) fn spawn(&self, command: &mut Command) -> io::Result<Option<Spawned>> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let Some(stdout) = child.stdout.take() else {
            reap(child);
            return Err(io::Error::other("child has no stdout pipe"));
        };
        if let Err(e) = crate::logs::forward(&mut child, command) {
            reap(child);
            return Err(e);
        }
        let mut reg = lock(&self.inner);
        if reg.closed {
            drop(reg);
            reap(child);
            return Ok(None);
        }
        let key = reg.next_key;
        reg.next_key += 1;
        reg.live.push((key, child));
        Ok(Some(Spawned { key, stdout }))
    }

    /// Waits for child `key` to exit on its own (it has closed its stdout),
    /// killing it after a grace period, and returns its exit status. `None`
    /// when the child was already taken by [`Children::close`] or killed.
    pub(crate) fn finish(&self, key: u64) -> Option<ExitStatus> {
        let deadline = Instant::now() + EXIT_GRACE;
        loop {
            {
                let mut reg = lock(&self.inner);
                let i = reg.live.iter().position(|(k, _)| *k == key)?;
                if let Ok(Some(status)) = reg.live[i].1.try_wait() {
                    reg.live.swap_remove(i);
                    return Some(status);
                }
            }
            if Instant::now() >= deadline {
                self.kill(key);
                return None;
            }
            std::thread::sleep(EXIT_POLL);
        }
    }

    /// Kills and reaps child `key`, if it is still registered.
    pub(crate) fn kill(&self, key: u64) {
        let child = {
            let mut reg = lock(&self.inner);
            let i = reg.live.iter().position(|(k, _)| *k == key);
            i.map(|i| reg.live.swap_remove(i).1)
        };
        if let Some(child) = child {
            reap(child);
        }
    }

    /// Refuses further spawns, then kills and reaps every registered child.
    /// Idempotent.
    pub(crate) fn close(&self) {
        let live = {
            let mut reg = lock(&self.inner);
            reg.closed = true;
            std::mem::take(&mut reg.live)
        };
        for (_, child) in live {
            reap(child);
        }
    }

    /// Process ids of the registered children.
    pub(crate) fn pids(&self) -> Vec<u32> {
        lock(&self.inner).live.iter().map(|(_, c)| c.id()).collect()
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        self.close();
    }
}

/// Kills `child` (a no-op if it already exited) and waits for it.
fn reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Starts the decoder for each pass over a file, keeping the next pass's
/// decoder warm while looping.
///
/// A fresh ffmpeg needs tens of milliseconds to open the file and decode its
/// first frame. When looping, the decoder for pass `k + 1` is spawned as soon
/// as pass `k` starts; it decodes until its stdout pipe is full and then
/// blocks, so the restart at the end of the file is seamless.
pub(crate) struct Respawner {
    /// Builds the command for one pass.
    make: Box<dyn Fn() -> Command + Send>,
    /// Whether further passes will be wanted.
    looping: bool,
    /// The already started decoder for the next pass.
    next: Option<Spawned>,
}

impl Respawner {
    /// A respawner whose first pass uses the already started `first`.
    pub(crate) fn new(
        first: Spawned,
        looping: bool,
        make: impl Fn() -> Command + Send + 'static,
    ) -> Self {
        Self {
            make: Box::new(make),
            looping,
            next: Some(first),
        }
    }

    /// The decoder for the next pass; when looping, also starts the one after.
    /// `Ok(None)` once `children` is closed.
    pub(crate) fn start(&mut self, children: &Children) -> io::Result<Option<Spawned>> {
        let current = match self.next.take() {
            Some(s) => Some(s),
            None => children.spawn(&mut (self.make)())?,
        };
        if self.looping && current.is_some() {
            // A failed warm-up is retried synchronously at the next pass.
            self.next = children.spawn(&mut (self.make)()).ok().flatten();
        }
        Ok(current)
    }

    /// Kills the warm decoder, if any.
    pub(crate) fn discard(&mut self, children: &Children) {
        if let Some(s) = self.next.take() {
            children.kill(s.key);
        }
    }
}

/// Reads until `buf` is full or the reader reaches end of input, and returns
/// the number of bytes read (less than `buf.len()` only at end of input).
pub(crate) fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that returns at most `step` bytes per call.
    struct Trickle {
        /// Remaining data.
        data: Vec<u8>,
        /// Bytes per read.
        step: usize,
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.step.min(buf.len()).min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data.drain(..n);
            Ok(n)
        }
    }

    #[test]
    fn read_full_assembles_short_reads() {
        let mut r = Trickle {
            data: (0..10).collect(),
            step: 3,
        };
        let mut buf = [0u8; 4];
        assert_eq!(read_full(&mut r, &mut buf).unwrap(), 4);
        assert_eq!(buf, [0, 1, 2, 3]);
        assert_eq!(read_full(&mut r, &mut buf).unwrap(), 4);
        assert_eq!(read_full(&mut r, &mut buf).unwrap(), 2);
        assert_eq!(read_full(&mut r, &mut buf).unwrap(), 0);
    }

    #[test]
    fn read_full_reports_errors() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
        }
        assert!(read_full(&mut Broken, &mut [0u8; 2]).is_err());
    }

    #[test]
    fn missing_program_is_an_error() {
        let children = Children::new();
        let mut cmd = Command::new("/nonexistent/definitely-not-ffmpeg");
        assert!(children.spawn(&mut cmd).is_err());
        assert!(children.pids().is_empty());
    }

    /// True while `pid` exists, zombies included.
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[cfg(unix)]
    #[test]
    fn close_kills_and_reaps_every_child() {
        let children = Children::new();
        let mut sleep = Command::new("sleep");
        sleep.arg("30");
        let a = children.spawn(&mut sleep).unwrap().unwrap();
        let _b = children.spawn(&mut sleep).unwrap().unwrap();
        let pids = children.pids();
        assert_eq!(pids.len(), 2);
        assert!(pids.iter().all(|&p| alive(p)));
        children.close();
        children.close();
        assert!(children.pids().is_empty());
        assert!(pids.iter().all(|&p| !alive(p)));
        assert_eq!(children.finish(a.key), None);
        assert!(children.spawn(&mut sleep).unwrap().is_none());
        assert!(children.pids().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn finish_returns_the_exit_status() {
        let children = Children::new();
        let s = children.spawn(&mut Command::new("true")).unwrap().unwrap();
        let mut out = s.stdout;
        assert_eq!(read_full(&mut out, &mut [0u8; 8]).unwrap(), 0);
        assert!(children.finish(s.key).unwrap().success());
        assert!(children.pids().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn respawner_keeps_one_warm_decoder_while_looping() {
        let children = Children::new();
        let make = || {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        let first = children.spawn(&mut make()).unwrap().unwrap();
        let mut r = Respawner::new(first, true, make);
        let pass0 = r.start(&children).unwrap().unwrap();
        assert_eq!(children.pids().len(), 2);
        children.kill(pass0.key);
        let pass1 = r.start(&children).unwrap().unwrap();
        assert_eq!(children.pids().len(), 2);
        children.kill(pass1.key);
        r.discard(&children);
        assert!(children.pids().is_empty());
        children.close();
        assert!(r.start(&children).unwrap().is_none());
    }
}
