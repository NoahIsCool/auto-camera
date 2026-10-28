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
//! * **Runs** means `tool -version` exited successfully within the deadline
//!   ([`SystemInspector::timeout`]) and printed a line; a tool still
//!   running then is killed and reaped, so the check leaves no process
//!   behind. Its output is read on a helper thread while the tool runs (the
//!   first 64 KiB kept, the rest drained), so a chatty tool can neither
//!   block on a full pipe nor die on a closed one. A tool that leaves a
//!   child of its own holding the pipe would hold up the read until that
//!   child exits; ffmpeg does not fork.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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
    let reader = thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut stdout) = stdout {
            let _ = stdout
                .by_ref()
                .take(MAX_VERSION_OUTPUT)
                .read_to_end(&mut out);
            // Drain the rest, so the tool is not killed by a closed pipe.
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        }
        out
    });
    let status = wait(&mut child, timeout);
    let out = reader.join().unwrap_or_default();
    let status = status?;
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

/// Waits up to `timeout` for `child`; kills and reaps it after that.
fn wait(child: &mut std::process::Child, timeout: Duration) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            other => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match other {
                    Err(e) => e.to_string(),
                    _ => format!(
                        "`-version` did not finish within {} s and was stopped",
                        timeout.as_secs_f64()
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests;
