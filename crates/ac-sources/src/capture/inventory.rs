//! Asking the system which cameras are connected: the two listings the
//! resolver matches, behind a trait.
//!
//! # Theory
//!
//! Resolving a unique ID ([`super::devices`]) needs two listings, made by
//! running two tools: `system_profiler SPCameraDataType -json` (names and
//! unique IDs, on stdout) and `ffmpeg -f avfoundation -list_devices true
//! -i ""` (indexes and names, on stderr; ffmpeg then exits with an error,
//! since the empty input is not a device, so its exit status says nothing).
//! [`DeviceInventory`] returns their raw text, so the I/O is this narrow:
//! tests put canned listings behind it, change them between opens (a
//! replug) and count how often they were asked, while every parsing rule
//! runs on the real text either way. [`SystemInventory`] runs the tools.
//!
//! # Invariants
//!
//! * A listing never blocks a source for long: a tool that has not finished
//!   within [`SystemInventory::timeout`] is killed and reaped, and its
//!   listing is an error. Each pipe is read on its own thread, so a tool
//!   that fills one pipe never stalls on the other, and at most
//!   [`MAX_LISTING`] bytes of each are kept.
//! * Nothing is cached: every call runs the tool again, because the answer
//!   is only true until a camera is plugged or unplugged.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ac_media::MediaError;

use crate::probe::{FFMPEG_ENV, FfmpegTools};

/// Where macOS keeps `system_profiler`; named in full because an app
/// started from Finder may not have `/usr/sbin` on its `PATH`.
pub const SYSTEM_PROFILER: &str = "/usr/sbin/system_profiler";

/// Most bytes of a tool's output kept per pipe (the listings are a few
/// kilobytes).
pub(crate) const MAX_LISTING: u64 = 1 << 20;

/// Poll interval while waiting for a listing tool to exit.
const LISTING_POLL: Duration = Duration::from_millis(5);

/// The raw listings a unique ID is resolved from; see the module docs.
pub trait DeviceInventory: Send + Sync {
    /// The output of `system_profiler SPCameraDataType -json` (stdout).
    fn camera_report(&self) -> Result<String, MediaError>;
    /// The output of `ffmpeg -f avfoundation -list_devices true -i ""`
    /// (stderr).
    fn ffmpeg_devices(&self) -> Result<String, MediaError>;
}

/// The real listings, from running the tools on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInventory {
    /// The ffmpeg whose avfoundation input will open the device (the same
    /// one must list it, since the indexes are its own).
    pub ffmpeg: PathBuf,
    /// The `system_profiler` to run ([`SYSTEM_PROFILER`] by default).
    pub system_profiler: PathBuf,
    /// How long either tool may take (10 s by default).
    pub timeout: Duration,
}

impl SystemInventory {
    /// Default for [`SystemInventory::timeout`].
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

    /// The listings of `tools`' ffmpeg and macOS's `system_profiler`.
    pub fn new(tools: &FfmpegTools) -> Self {
        Self {
            ffmpeg: tools.ffmpeg.clone(),
            system_profiler: SYSTEM_PROFILER.into(),
            timeout: Self::DEFAULT_TIMEOUT,
        }
    }
}

impl DeviceInventory for SystemInventory {
    fn camera_report(&self) -> Result<String, MediaError> {
        let mut cmd = Command::new(&self.system_profiler);
        cmd.args(["SPCameraDataType", "-json"]);
        let listing = run_listing(cmd, self.timeout).map_err(|e| {
            MediaError::Open(format!(
                "cannot list cameras with {} (capture sources need macOS): {e}",
                self.system_profiler.display()
            ))
        })?;
        if !listing.status.success() {
            return Err(MediaError::Open(format!(
                "{} failed ({}): {}",
                self.system_profiler.display(),
                listing.status,
                listing.stderr.trim()
            )));
        }
        Ok(listing.stdout)
    }

    fn ffmpeg_devices(&self) -> Result<String, MediaError> {
        let mut cmd = Command::new(&self.ffmpeg);
        cmd.args(["-nostdin", "-hide_banner", "-f", "avfoundation"])
            .args(["-list_devices", "true", "-i", ""]);
        let listing = run_listing(cmd, self.timeout).map_err(|e| {
            MediaError::Open(format!(
                "cannot list devices with {} (install ffmpeg or set {FFMPEG_ENV}): {e}",
                self.ffmpeg.display()
            ))
        })?;
        Ok(listing.stderr)
    }
}

/// What a listing tool printed, and how it exited.
#[derive(Debug)]
pub(crate) struct Listing {
    /// Its exit status.
    pub(crate) status: ExitStatus,
    /// Its standard output, lossily decoded.
    pub(crate) stdout: String,
    /// Its standard error, lossily decoded.
    pub(crate) stderr: String,
}

/// Runs `cmd` with no input, collecting both outputs, and kills it when it
/// has not exited within `timeout`; see the module docs. The error says
/// why in plain words.
pub(crate) fn run_listing(mut cmd: Command, timeout: Duration) -> Result<Listing, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let pipes: [(bool, Option<Box<dyn Read + Send>>); 2] = [
        (true, child.stdout.take().map(|p| Box::new(p) as _)),
        (false, child.stderr.take().map(|p| Box::new(p) as _)),
    ];
    for (is_stdout, pipe) in pipes {
        let tx = tx.clone();
        let Some(pipe) = pipe else { continue };
        let reader = std::thread::Builder::new()
            .name("ac-capture-list".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                // A pipe that fails mid-read keeps what it gave.
                let _ = pipe.take(MAX_LISTING).read_to_end(&mut bytes);
                let _ = tx.send((is_stdout, bytes));
            });
        if let Err(e) = reader {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("cannot read its output: {e}"));
        }
    }
    drop(tx);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(LISTING_POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("it did not finish within {timeout:?}"));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    // The pipes close with the tool; a grandchild that kept one open is not
    // waited for past the deadline.
    let left = deadline.saturating_duration_since(Instant::now());
    while let Ok((is_stdout, bytes)) = rx.recv_timeout(left.max(LISTING_POLL)) {
        if is_stdout {
            stdout = bytes;
        } else {
            stderr = bytes;
        }
    }
    Ok(Listing {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell command line.
    #[cfg(unix)]
    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[cfg(unix)]
    #[test]
    fn a_listing_collects_both_outputs_and_the_status() {
        let got = run_listing(
            sh("echo out; echo err >&2; exit 3"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(got.stdout, "out\n");
        assert_eq!(got.stderr, "err\n");
        assert_eq!(got.status.code(), Some(3));
    }

    #[cfg(unix)]
    #[test]
    fn a_listing_that_hangs_is_killed_at_its_deadline() {
        let started = Instant::now();
        let e = run_listing(sh("exec sleep 30"), Duration::from_millis(100)).unwrap_err();
        assert!(e.contains("did not finish within 100ms"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn a_listing_keeps_at_most_its_cap() {
        let script = format!("head -c {} /dev/zero", MAX_LISTING + 4096);
        let got = run_listing(sh(&script), Duration::from_secs(10)).unwrap();
        assert_eq!(got.stdout.len() as u64, MAX_LISTING);
    }

    #[test]
    fn missing_tools_are_errors_that_say_what_is_needed() {
        let inventory = SystemInventory {
            ffmpeg: "/nonexistent/auto-camera/ffmpeg".into(),
            system_profiler: "/nonexistent/auto-camera/system_profiler".into(),
            timeout: Duration::from_secs(1),
        };
        let e = inventory.camera_report().unwrap_err().to_string();
        assert!(e.contains("capture sources need macOS"), "{e}");
        let e = inventory.ffmpeg_devices().unwrap_err().to_string();
        assert!(e.contains("install ffmpeg or set AC_FFMPEG"), "{e}");
    }

    #[test]
    fn the_system_inventory_uses_the_decoders_ffmpeg() {
        let tools = FfmpegTools {
            ffmpeg: "/opt/ff/ffmpeg".into(),
            ffprobe: "/opt/ff/ffprobe".into(),
        };
        let inventory = SystemInventory::new(&tools);
        assert_eq!(inventory.ffmpeg, PathBuf::from("/opt/ff/ffmpeg"));
        assert_eq!(inventory.system_profiler, PathBuf::from(SYSTEM_PROFILER));
        assert_eq!(inventory.timeout, SystemInventory::DEFAULT_TIMEOUT);
    }

    #[cfg(unix)]
    #[test]
    fn ffmpeg_lists_devices_on_stderr_whatever_its_exit() {
        let tools = FfmpegTools::from_env();
        if !tools.is_available() {
            eprintln!("skipping: ffmpeg not found");
            return;
        }
        // Off macOS the answer is that avfoundation is unknown, which the
        // parser turns into its error; either way the text is stderr's.
        let text = SystemInventory::new(&tools).ffmpeg_devices().unwrap();
        assert!(
            text.contains("avfoundation") || text.contains("AVFoundation"),
            "{text}"
        );
    }
}
