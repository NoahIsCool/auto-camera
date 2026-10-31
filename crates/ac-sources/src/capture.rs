//! A live capture device (a USB/UVC webcam, an HDMI-to-USB dongle) read
//! through ffmpeg's avfoundation input on macOS, named by its unique ID.
//!
//! # Identity
//!
//! A source is configured by the camera's unique ID, which macOS keeps for
//! as long as the camera is plugged into the same port, and never by ffmpeg's
//! device index, which moves whenever the connected cameras change. Every
//! open asks a [`DeviceInventory`] for the current listings and resolves the
//! ID to the index it has now ([`devices::resolve`]); a supervisor
//! ([`crate::supervisor`]) opens again after every failure, so a camera that
//! was unplugged and plugged back in, even at another index, comes back as
//! the same source, and a camera that is not there is an error with no
//! fallback to another one.
//!
//! # Connection
//!
//! One connection is one ffmpeg child, started with the process code a file
//! source uses ([`crate::process`]): the avfoundation input in the requested
//! mode ([`format`]), out as raw RGBA frames of exactly the requested size
//! at the requested rate. The **worker** reads whole frames, stamps them
//! with `seq` and `pts = seq / fps` and publishes each into the
//! [`LatestFrame`] mailbox as soon as it is read: a camera runs in real
//! time, so its frames are not paced on a clock (there is nothing to wait
//! for, and waiting would only add latency). ffmpeg does not wait either:
//! its encoder takes no frame threads, because a frame-threaded one hands
//! a frame out only when a later one comes in ([`format`]), which would
//! add a frame of latency, several under load, and keep a device's last
//! frames from the source when it falls silent. Its filter chain, which
//! converts the picture to RGBA, keeps its threads, which 4K needs. The
//! first frame moves the source from `Starting` to `Running`.
//!
//! # Failure
//!
//! A device has no end, so a capture source never stops without an error
//! unless asked to:
//!
//! * ffmpeg's output ending, whatever its exit status, is a lost input. With
//!   no frame delivered it is a refused open, most often a mode the device
//!   does not offer, and the error quotes what ffmpeg said from its start
//!   (up to [`TAIL_OPENING`] lines), so "Selected framerate is not
//!   supported by the device" and every mode it lists are on the
//!   operator's screen and in the error's log line; the log's own copy of
//!   ffmpeg's lines holds back all but the first and last mode, which
//!   differ only in their numbers ([`crate::logs`]). After frames it is a
//!   device that went away, and the error quotes ffmpeg's first and last
//!   lines ([`crate::logs::StderrTail`]).
//! * A device can also fall silent without ffmpeg exiting (a dongle that
//!   lost its HDMI signal, a camera that froze). The **watchdog** waits on
//!   the mailbox and fails the connection when no first frame came within
//!   [`CaptureOptions::open_timeout`] or no next one within
//!   [`CaptureOptions::stall_timeout`], in real time, since the device runs
//!   in real time whatever clock the rest of the studio is paced by.
//!
//! A failure stops the connection (worker, watchdog and child), and a
//! supervisor then opens it again, resolving the unique ID anew.
//!
//! # Invariants
//!
//! * The resolver runs before every open, and its listings are never
//!   cached ([`inventory`]).
//! * The process invariants of [`crate::process`] hold: after
//!   [`VideoSource::stop`] (or a drop) no child is alive and every one was
//!   reaped; the worker kills its child before dropping its stdout.
//! * The first error is kept: a watchdog timeout that killed ffmpeg is the
//!   source's error, not the end of output it caused.
//!
//! # Test seam
//!
//! [`CaptureSource::open_input`] (crate-private) takes the function that
//! turns the resolved device into ffmpeg's input options. Production passes
//! the avfoundation input; this crate's tests pass a lavfi `testsrc` or a
//! FIFO in its place, so the whole lifecycle (resolve, open, kill,
//! reconnect) runs on any machine with ffmpeg. It is not reachable from a
//! config.

use std::ffi::OsString;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{
    AudioBlock, LatestFrame, MediaError, SourceInfo, SourceState, VideoFrame, VideoSource,
};

use crate::ffmpeg::command;
use crate::frame_rate::FrameRate;
use crate::logs::{StderrTail, TAIL_FIRST, TAIL_OPENING};
use crate::pool::FramePool;
use crate::probe::{FFMPEG_ENV, FfmpegTools};
use crate::process::{Children, END_GRACE, Spawned, read_full};
use crate::status::{Status, lock};

pub mod devices;
pub mod format;
pub mod inventory;

pub use devices::{
    AvDevice, ResolvedDevice, SystemCamera, parse_camera_report, parse_device_list, resolve,
};
pub use format::{CaptureFormat, DEFAULT_PIXEL_FORMAT, PIXEL_FORMATS, request_problems};
pub use inventory::{DeviceInventory, SYSTEM_PROFILER, SystemInventory};

/// The target of a capture source's own lines in the app's log.
const TARGET: &str = "ac_sources::capture";

/// How often the watchdog looks at the stop flag while it waits.
const WATCH_SLICE: Duration = Duration::from_millis(50);

/// How long a failed connection waits for ffmpeg's last stderr lines.
const LAST_WORDS: Duration = Duration::from_millis(500);

/// How a capture device is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOptions {
    /// The ffmpeg that captures (and lists the devices).
    pub tools: FfmpegTools,
    /// Longest wait for the first frame of a connection (10 s by default:
    /// a camera takes a second or two to start).
    pub open_timeout: Duration,
    /// Longest wait for each later frame (3 s by default, many frame
    /// intervals of any camera).
    pub stall_timeout: Duration,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            tools: FfmpegTools::from_env(),
            open_timeout: Duration::from_secs(10),
            stall_timeout: Duration::from_secs(3),
        }
    }
}

/// Resolves `unique_id` with `inventory`: both listings, parsed and
/// matched ([`devices::resolve`]). Every error names the unique ID.
pub fn resolve_device(
    inventory: &dyn DeviceInventory,
    unique_id: &str,
) -> Result<ResolvedDevice, MediaError> {
    let named = |e: MediaError| match e {
        MediaError::Open(why) => {
            MediaError::Open(format!("capture device {:?}: {why}", unique_id.trim()))
        }
        other => other,
    };
    let cameras = parse_camera_report(&inventory.camera_report().map_err(named)?).map_err(named)?;
    let devices = parse_device_list(&inventory.ffmpeg_devices().map_err(named)?).map_err(named)?;
    resolve(unique_id, &cameras, &devices).map_err(named)
}

/// State shared by a [`CaptureSource`] and its threads.
///
/// Only the worker ends the connection's status: it records the error, if
/// any, before the state turns `Stopped`, so a reader that sees `Stopped`
/// already sees why (a supervisor reopens only a source that stopped with
/// an error). The watchdog leaves its reason in `cut` and kills the child,
/// which ends the worker.
struct Shared {
    /// Raised once by `stop`, a cut or a failure; the threads exit when
    /// they see it.
    stop: AtomicBool,
    /// Why the watchdog cut the connection, if it did.
    cut: Mutex<Option<String>>,
    /// Health and the stopping error.
    status: Status,
    /// The newest frame.
    latest: LatestFrame,
    /// The ffmpeg child.
    children: Children,
    /// The child's first and last stderr lines.
    tail: Arc<StderrTail>,
    /// What the device is, for messages.
    device: ResolvedDevice,
}

impl Shared {
    /// True once a stop was requested.
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Asks both threads to exit and unblocks the worker.
    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.children.close();
    }

    /// Ends the connection because of `reason`, which the worker makes the
    /// source's error (see [`Shared`]).
    fn cut(&self, reason: String) {
        lock(&self.cut).get_or_insert(reason);
        self.request_stop();
    }

    /// Stops the source with `message` as its error, adding what ffmpeg
    /// said, if anything (its drain ends with the child, which is dead or
    /// dying by now): all of its opening when the connection `delivered`
    /// no frame, its first and last lines when it did (see the module
    /// docs).
    fn fail(&self, message: String, delivered: bool) {
        let first = if delivered { TAIL_FIRST } else { TAIL_OPENING };
        let message = match self.tail.summary(LAST_WORDS, first) {
            Some(said) => format!("{message}: {said}"),
            None => message,
        };
        tracing::warn!(target: TARGET, device = %self.device, "{message}");
        self.status.fail(message);
        self.request_stop();
    }
}

/// A live capture device as a source; see [`crate::capture`].
pub struct CaptureSource {
    /// Static description: the requested size and rate, no audio.
    info: SourceInfo,
    /// The device this connection opened.
    device: ResolvedDevice,
    /// State shared with the threads.
    shared: Arc<Shared>,
    /// Worker and watchdog, joined by `stop`.
    threads: Vec<JoinHandle<()>>,
}

impl fmt::Debug for CaptureSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaptureSource")
            .field("info", &self.info)
            .field("device", &self.device)
            .field("state", &self.shared.status.get())
            .finish_non_exhaustive()
    }
}

impl CaptureSource {
    /// Resolves `unique_id` with `inventory` and starts capturing that
    /// device in `format`. Fails when the camera is not connected (or
    /// cannot be told apart) or ffmpeg cannot be started; a device that
    /// refuses the mode fails the source just after, with ffmpeg's reason
    /// as its error (see the module docs).
    pub fn open(
        id: SourceId,
        unique_id: &str,
        format: &CaptureFormat,
        inventory: &dyn DeviceInventory,
        options: CaptureOptions,
    ) -> Result<Self, MediaError> {
        Self::open_input(
            id,
            unique_id,
            format,
            inventory,
            options,
            &format::avfoundation_input,
        )
    }

    /// [`CaptureSource::open`] with `input` making ffmpeg's input options
    /// from the resolved device: the test seam of the module docs.
    pub(crate) fn open_input(
        id: SourceId,
        unique_id: &str,
        format: &CaptureFormat,
        inventory: &dyn DeviceInventory,
        options: CaptureOptions,
        input: &dyn Fn(&ResolvedDevice, &CaptureFormat) -> Vec<OsString>,
    ) -> Result<Self, MediaError> {
        let device = resolve_device(inventory, unique_id)?;
        tracing::info!(target: TARGET, source = %id.0, device = %device, "opening capture device");
        let args = format::capture_args(input(&device, format), format);
        let (width, height) = format.size();
        let info = SourceInfo {
            id: id.clone(),
            width,
            height,
            fps: format.rate().as_f64(),
            audio: None,
        };
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            cut: Mutex::new(None),
            status: Status::new(),
            latest: LatestFrame::new(),
            children: Children::new(),
            tail: Arc::new(StderrTail::new()),
            device: device.clone(),
        });
        let tools = &options.tools;
        let spawned = shared
            .children
            .spawn_keeping(&mut command(tools, &args), Some(shared.tail.clone()))
            .map_err(|e| {
                MediaError::Open(format!(
                    "cannot start {} to capture {device} (install ffmpeg or set {FFMPEG_ENV}): {e}",
                    tools.ffmpeg.display()
                ))
            })?
            .ok_or_else(|| MediaError::Open("source stopped while opening".into()))?;
        let mut source = Self {
            info,
            device,
            shared,
            threads: Vec::new(),
        };
        let worker = Worker {
            source: id.clone(),
            size: (width, height),
            rate: format.rate(),
            decoder: spawned,
        };
        source.spawn(format!("ac-capture-{}", id.0), move |s| worker.run(s))?;
        let watch = Watchdog {
            open: options.open_timeout,
            stall: options.stall_timeout,
        };
        source.spawn(format!("ac-capture-watch-{}", id.0), move |s| watch.run(s))?;
        Ok(source)
    }

    /// Runs `work` on a named thread; it is joined by `stop`. A thread that
    /// cannot be spawned stops the source.
    fn spawn(
        &mut self,
        name: String,
        work: impl FnOnce(&Shared) + Send + 'static,
    ) -> Result<(), MediaError> {
        let shared = self.shared.clone();
        match std::thread::Builder::new()
            .name(name)
            .spawn(move || work(&shared))
        {
            Ok(handle) => {
                self.threads.push(handle);
                Ok(())
            }
            Err(e) => {
                self.stop();
                Err(e.into())
            }
        }
    }

    /// The device this connection opened.
    pub fn device(&self) -> &ResolvedDevice {
        &self.device
    }

    /// Process ids of the running ffmpeg children (empty once stopped).
    pub fn child_pids(&self) -> Vec<u32> {
        self.shared.children.pids()
    }
}

impl VideoSource for CaptureSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        self.shared.status.get()
    }

    fn latest(&self) -> Option<VideoFrame> {
        self.shared.latest.get()
    }

    fn wait_newer(&self, after: Option<u64>, timeout: Duration) -> Option<VideoFrame> {
        self.shared.latest.wait_newer(after, timeout)
    }

    fn drain_audio(&self) -> Vec<AudioBlock> {
        Vec::new()
    }

    fn error(&self) -> Option<String> {
        self.shared.status.error()
    }

    fn stop(&mut self) {
        self.shared.request_stop();
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                self.shared.status.fail("a capture thread panicked");
            }
        }
        self.shared.children.close();
        self.shared.status.advance(SourceState::Stopped);
    }
}

impl Drop for CaptureSource {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reads one connection's frames.
struct Worker {
    /// Source the frames are tagged with.
    source: SourceId,
    /// Frame size.
    size: (u32, u32),
    /// Frame rate; defines PTS.
    rate: FrameRate,
    /// The ffmpeg child.
    decoder: Spawned,
}

impl Worker {
    /// Publishes frames until the output ends, an error or a stop request;
    /// anything but a stop fails the source (see the module docs).
    fn run(self, shared: &Shared) {
        let Spawned { key, mut stdout } = self.decoder;
        let (w, h) = self.size;
        let frame_len = w as usize * h as usize * 4;
        let mut pool = FramePool::new(frame_len);
        let mut seq = 0u64;
        let mut failure = None;
        while !shared.stopping() {
            let (data, got) = pool.fill(|buf| read_full(&mut stdout, buf));
            match got {
                Ok(n) if n == frame_len => {}
                Ok(_) => break,
                Err(e) => {
                    failure = Some(format!("reading frames from {}: {e}", shared.device));
                    break;
                }
            }
            match VideoFrame::new(
                self.source.clone(),
                seq,
                self.rate.pts(seq),
                w,
                h,
                data.clone(),
            ) {
                Ok(frame) => shared.latest.publish(frame),
                Err(e) => {
                    failure = Some(e.to_string());
                    break;
                }
            }
            pool.recycle(data);
            shared.status.advance(SourceState::Running);
            seq += 1;
        }
        // Killed before its pipe closes when cut off; waited for, for its
        // exit status, when its output ended.
        let exit = if shared.stopping() || failure.is_some() {
            shared.children.kill(key);
            None
        } else {
            shared.children.finish_within(key, END_GRACE)
        };
        drop(stdout);
        // The stop flag first: a cut is recorded before the watchdog raises
        // it, so a raised flag with no cut is a stop that was asked for.
        let stopping = shared.stopping();
        let cut = lock(&shared.cut).take();
        if !stopping || cut.is_some() {
            let exit = exit.map_or("killed".to_string(), |s| s.to_string());
            let message = cut.or(failure).unwrap_or_else(|| match seq {
                0 => format!("ffmpeg could not capture from {} ({exit})", shared.device),
                n => format!(
                    "capture from {} ended after {n} frames ({exit})",
                    shared.device
                ),
            });
            shared.fail(message, seq > 0);
        }
        shared.status.advance(SourceState::Stopped);
    }
}

/// Fails a connection whose device fell silent; see the module docs.
struct Watchdog {
    /// Longest wait for the first frame.
    open: Duration,
    /// Longest wait for each later frame.
    stall: Duration,
}

impl Watchdog {
    /// Waits on the mailbox until a stop request or a silence too long.
    fn run(self, shared: &Shared) {
        let mut last: Option<u64> = None;
        let mut since = Instant::now();
        while !shared.stopping() {
            let limit = if last.is_some() {
                self.stall
            } else {
                self.open
            };
            let left = limit.saturating_sub(since.elapsed());
            if left.is_zero() {
                let secs = limit.as_secs_f64();
                shared.cut(match last {
                    None => format!(
                        "no picture from {} within {secs:.1} s of opening it",
                        shared.device
                    ),
                    Some(_) => format!(
                        "{} stopped delivering pictures: none for {secs:.1} s",
                        shared.device
                    ),
                });
                return;
            }
            if let Some(frame) = shared.latest.wait_newer(last, left.min(WATCH_SLICE)) {
                last = Some(frame.seq);
                since = Instant::now();
            }
        }
    }
}

#[cfg(test)]
mod tests;
