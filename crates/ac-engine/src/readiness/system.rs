//! [`SystemInspector`]: the readiness check's questions answered from the
//! file system and by running the ffmpeg tools.
//!
//! # Theory
//!
//! Each answer is the cheapest look that tells the truth:
//!
//! * **Readable** means it opens: a file is opened for reading and closed
//!   unread, a folder is listed. Permission bits alone would miss ACLs,
//!   sandboxes and read-only mounts. A pipe or socket is never opened,
//!   since opening one can block.
//! * **Executable** is the permission bits (any of user, group, other):
//!   finding out more would mean running the program, which the check
//!   never does for anything but the ffmpeg tools.
//! * **Takes new files** means an empty probe file could be created in the
//!   folder (and was removed at once), for the same reason; it is the one
//!   write the check makes.
//! * **Runs** means `tool -version` exited successfully, closed its output
//!   and printed a line, all within the deadline
//!   ([`SystemInspector::timeout`]). Its output is read on a helper thread
//!   while the tool runs (the first 64 KiB kept, the rest drained), so a
//!   chatty tool can neither block on a full pipe nor die on a closed one.
//!
//! # Stopping a tool
//!
//! A tool may be a wrapper script whose own children hold its output open
//! after it has been killed, so killing the tool alone would leave them
//! running and the read waiting on them. On Unix the tool therefore runs in
//! a process group of its own, and at the deadline the whole group is
//! killed (by the `kill` utility, since the standard library has no call
//! for it and the workspace has no unsafe code) and the tool is reaped:
//! the check never waits past the deadline and leaves nothing of the
//! group behind. The read is not waited for then; a process that left the
//! group still holds the pipe, and the helper thread ends when it does (or
//! with the program). Its own group also keeps the tool out of the
//! terminal's, so an interrupt at the terminal stops the check but not a
//! tool still running then, which `ffmpeg -version`, done in milliseconds,
//! never is.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use ac_sources::FfmpegTools;

use super::{Entry, Inspect};

/// The most of a tool's `-version` output kept, in bytes.
const MAX_VERSION_OUTPUT: u64 = 64 * 1024;

/// How often a running tool is polled.
const POLL: Duration = Duration::from_millis(10);

/// Probe files created so far, for unique names.
static PROBES: AtomicU64 = AtomicU64::new(0);

/// Answers the check's questions from this machine; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemInspector {
    /// Longest a tool's `-version` may take before it is killed.
    pub timeout: Duration,
}

impl Default for SystemInspector {
    /// Ten seconds per tool: `ffmpeg -version` takes milliseconds, but a
    /// cold disk or a virus scanner can make a first start slow.
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }
}

impl Inspect for SystemInspector {
    fn entry(&self, path: &Path) -> Entry {
        match fs::metadata(path) {
            Ok(m) if m.is_file() => Entry::File {
                readable: File::open(path).is_ok(),
                executable: executable(&m),
            },
            Ok(m) if m.is_dir() => Entry::Dir {
                readable: fs::read_dir(path).is_ok(),
            },
            Ok(_) => Entry::Other,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match fs::read_link(path) {
                Ok(target) => Entry::BrokenLink(target),
                Err(_) => Entry::Missing,
            },
            Err(e) => Entry::Unknown(e.to_string()),
        }
    }

    fn can_create_in(&self, dir: &Path) -> Result<(), String> {
        let n = PROBES.fetch_add(1, Ordering::Relaxed);
        let probe = dir.join(format!(".auto-camera-check-{}-{n}", std::process::id()));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .map_err(|e| e.to_string())?;
        fs::remove_file(&probe)
            .map_err(|e| format!("made {} but cannot remove it: {e}", probe.display()))
    }

    fn version(&self, tool: &Path) -> Result<String, String> {
        let mut command = Command::new(tool);
        command.arg("-version");
        run_version(command, self.timeout)
    }

    fn find_program(&self, name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && executable(&m)))
    }

    fn read_text(&self, path: &Path) -> Result<String, String> {
        fs::read_to_string(path).map_err(|e| e.to_string())
    }

    fn decoder_tools(&self) -> FfmpegTools {
        FfmpegTools::from_env()
    }
}

/// True when any execute bit is set.
#[cfg(unix)]
fn executable(m: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o111 != 0
}

/// True: without permission bits, any file may be run.
#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
    true
}

/// Runs `command` (a tool with `-version`) for at most `timeout` and
/// returns the first line it printed, without the copyright notice ffmpeg
/// appends; see the module docs.
fn run_version(mut command: Command, timeout: Duration) -> Result<String, String> {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let deadline = Instant::now() + timeout;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "not found".to_owned(),
            _ => e.to_string(),
        })?;
    let stdout = child.stdout.take();
    let (sender, output) = mpsc::channel();
    thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut stdout) = stdout {
            let _ = stdout
                .by_ref()
                .take(MAX_VERSION_OUTPUT)
                .read_to_end(&mut out);
            // Drain the rest, so the tool is not killed by a closed pipe.
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        }
        let _ = sender.send(out);
    });
    let finished = match wait_until(&mut child, deadline) {
        Ok(Some(status)) => {
            match output.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(out) => Some((status, out)),
                // The helper is gone without a word: nothing was read.
                Err(RecvTimeoutError::Disconnected) => Some((status, Vec::new())),
                Err(RecvTimeoutError::Timeout) => None,
            }
        }
        Ok(None) => None,
        Err(e) => {
            stop(&mut child);
            return Err(e);
        }
    };
    let Some((status, out)) = finished else {
        stop(&mut child);
        return Err(format!(
            "`-version` did not finish within {} s and was stopped",
            timeout.as_secs_f64()
        ));
    };
    if !status.success() {
        return Err(format!("`-version` failed ({status})"));
    }
    let text = String::from_utf8_lossy(&out);
    let line = text.lines().next().unwrap_or("").trim();
    let line = line.split(" Copyright").next().unwrap_or(line).trim();
    if line.is_empty() {
        return Err("`-version` printed nothing; is it ffmpeg?".into());
    }
    Ok(line.to_owned())
}

/// Waits for `child` until `deadline`: its exit status, or `None` when it
/// is still running then.
fn wait_until(child: &mut Child, deadline: Instant) -> Result<Option<ExitStatus>, String> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            Ok(None) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Kills `child` and, on Unix, the process group it leads (see the module
/// docs), then reaps it.
fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests;
