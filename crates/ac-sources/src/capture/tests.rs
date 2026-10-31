//! Lifecycle tests of capture sources with ffmpeg stand-ins for the device:
//! a lavfi `testsrc` (a camera that delivers), a FIFO (a camera that is
//! silent, or falls silent) and a refused input (a mode the device lacks),
//! behind a fake inventory whose listings change between opens as a replug
//! would change them.
//!
//! Every test that runs ffmpeg skips (passes with a note) when it is not
//! installed, unless `AC_REQUIRE_FFMPEG` is set.

use super::*;
use crate::clock::WallClock;
use crate::config::{SourceConfig, open_source};
use crate::supervisor::{Backoff, SupervisedSource, open_supervised};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The fixtures the fake inventory serves.
mod listings {
    /// ffmpeg's list: built-in camera, webcam at 1, dongle at 2.
    pub(super) const FFMPEG_THREE: &str =
        include_str!("../../tests/fixtures/capture/ffmpeg-three-cameras.txt");
    /// system_profiler's report of the same cameras.
    pub(super) const PROFILER_THREE: &str =
        include_str!("../../tests/fixtures/capture/system-profiler-three-cameras.json");
    /// ffmpeg's list after the webcam was replugged: now at 2.
    pub(super) const FFMPEG_REPLUGGED: &str =
        include_str!("../../tests/fixtures/capture/ffmpeg-brio-replugged.txt");
    /// system_profiler's report after the replug.
    pub(super) const PROFILER_REPLUGGED: &str =
        include_str!("../../tests/fixtures/capture/system-profiler-brio-replugged.json");
    /// ffmpeg's list with the webcam unplugged.
    pub(super) const FFMPEG_UNPLUGGED: &str =
        include_str!("../../tests/fixtures/capture/ffmpeg-brio-unplugged.txt");
    /// system_profiler's report with the webcam unplugged.
    pub(super) const PROFILER_UNPLUGGED: &str =
        include_str!("../../tests/fixtures/capture/system-profiler-brio-unplugged.json");
}
use listings::*;

/// The webcam's unique ID in the fixtures.
const BRIO: &str = "0x14200000046d085e";

/// A fake inventory: canned listings that a test swaps, and a count of how
/// often the cameras were listed.
#[derive(Debug, Default)]
struct FakeInventory {
    /// system_profiler's and ffmpeg's text.
    listings: Mutex<(String, String)>,
    /// Calls of `camera_report`.
    asked: Mutex<u32>,
}

impl FakeInventory {
    /// An inventory serving `profiler` and `ffmpeg`.
    fn new(profiler: &str, ffmpeg: &str) -> Arc<Self> {
        let fake = Arc::new(Self::default());
        fake.set(profiler, ffmpeg);
        fake
    }

    /// Serves `profiler` and `ffmpeg` from now on (a replug).
    fn set(&self, profiler: &str, ffmpeg: &str) {
        *lock(&self.listings) = (profiler.into(), ffmpeg.into());
    }

    /// How often the cameras were listed.
    fn asked(&self) -> u32 {
        *lock(&self.asked)
    }
}

impl DeviceInventory for FakeInventory {
    fn camera_report(&self) -> Result<String, MediaError> {
        *lock(&self.asked) += 1;
        Ok(lock(&self.listings).0.clone())
    }

    fn ffmpeg_devices(&self) -> Result<String, MediaError> {
        Ok(lock(&self.listings).1.clone())
    }
}

/// The ffmpeg tools, or `None` (and a note) when ffmpeg is not installed;
/// panics instead when `AC_REQUIRE_FFMPEG` is set.
fn tools() -> Option<FfmpegTools> {
    let tools = FfmpegTools::from_env();
    if tools.is_available() {
        return Some(tools);
    }
    let required = std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
    assert!(!required, "ffmpeg/ffprobe are required (AC_REQUIRE_FFMPEG)");
    eprintln!("skipping: ffmpeg/ffprobe not found");
    None
}

/// The test source id.
fn cam() -> SourceId {
    SourceId("cam".into())
}

/// The tiny mode every stand-in delivers.
fn tiny() -> CaptureFormat {
    CaptureFormat::new(64, 36, 30.0, None).unwrap()
}

/// Options with `tools` and the given watchdog limits.
fn options(tools: &FfmpegTools, open: Duration, stall: Duration) -> CaptureOptions {
    CaptureOptions {
        tools: tools.clone(),
        open_timeout: open,
        stall_timeout: stall,
    }
}

/// The stand-in for a camera that delivers: ffmpeg's `testsrc` at the
/// requested size and rate, read in real time (`-re`) as a device is.
fn testsrc(_: &ResolvedDevice, format: &CaptureFormat) -> Vec<OsString> {
    let (w, h) = format.size();
    let graph = format!("testsrc=size={w}x{h}:rate={}", format.rate());
    ["-re", "-f", "lavfi", "-i", &graph]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// The stand-in for a camera that delivers what is written into `fifo`:
/// raw RGBA frames of the requested size.
fn from_fifo(fifo: &Path, format: &CaptureFormat) -> Vec<OsString> {
    let (w, h) = format.size();
    let mut args: Vec<OsString> = ["-f", "rawvideo", "-pixel_format", "rgba", "-video_size"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(format!("{w}x{h}").into());
    args.extend(["-framerate".into(), format.rate().to_string().into()]);
    args.extend(["-i".into(), fifo.as_os_str().to_owned()]);
    args
}

/// A fresh FIFO in a scratch folder of its own; the folder is removed by
/// [`Scratch`]'s drop.
#[cfg(unix)]
fn fifo(name: &str) -> (Scratch, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ac-capture-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("device.fifo");
    let made = Command::new("mkfifo").arg(&path).status().unwrap();
    assert!(made.success(), "mkfifo {}", path.display());
    (Scratch(dir), path)
}

/// A scratch folder removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// True while `pid` exists, zombies included.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Kills process `pid` with `SIGKILL`.
#[cfg(unix)]
fn kill(pid: u32) {
    let killed = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success(), "kill {pid}");
}

/// Polls `cond` every 2 ms until it holds or `timeout` passes.
fn eventually(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

#[cfg(unix)]
#[test]
fn a_capture_source_delivers_the_requested_mode_and_stops_clean() {
    let Some(tools) = tools() else { return };
    let inventory = FakeInventory::new(PROFILER_THREE, FFMPEG_THREE);
    let limits = options(&tools, Duration::from_secs(20), Duration::from_secs(20));
    let mut src =
        CaptureSource::open_input(cam(), BRIO, &tiny(), &*inventory, limits, &testsrc).unwrap();
    assert_eq!(src.device().index, 1);
    assert_eq!(src.device().name, "Logitech BRIO");
    let info = src.info().clone();
    assert_eq!((info.width, info.height, info.fps), (64, 36, 30.0));
    assert_eq!(info.audio, None, "camera audio is not captured");
    let pids = src.child_pids();
    assert_eq!(pids.len(), 1, "one ffmpeg per connection");
    let mut frames = Vec::new();
    assert!(eventually(Duration::from_secs(20), || {
        if let Some(f) = src.wait_newer(frames.last().map(|f: &VideoFrame| f.seq), Duration::ZERO) {
            frames.push(f);
        }
        frames.len() >= 5
    }));
    assert_eq!(src.state(), SourceState::Running);
    let f = &frames[0];
    assert_eq!((f.width, f.height, f.source.clone()), (64, 36, cam()));
    for w in frames.windows(2) {
        assert!(w[0].seq < w[1].seq);
        assert_eq!(w[1].pts, src_rate().pts(w[1].seq), "pts = seq / fps");
    }
    assert!(src.drain_audio().is_empty());
    src.stop();
    assert_eq!(src.state(), SourceState::Stopped);
    assert_eq!(src.error(), None, "a stop that was asked for is no error");
    assert!(src.child_pids().is_empty());
    assert!(
        pids.iter().all(|&p| !alive(p)),
        "ffmpeg outlived its source"
    );
}

/// The tiny mode's rate.
fn src_rate() -> FrameRate {
    tiny().rate()
}

/// One connection a supervised capture opened.
#[derive(Debug, Clone)]
struct Connection {
    /// The ffmpeg device index its stand-in was given.
    index: u32,
    /// Its ffmpeg.
    pids: Vec<u32>,
}

/// R1a with a camera: a kill reconnects; a replug that moves the camera to
/// another index reopens the same unique ID there; an unplugged camera is
/// an error with no fallback and no ffmpeg started, until it is back.
#[cfg(unix)]
#[test]
fn a_supervised_capture_reconnects_by_unique_id_after_a_kill_a_replug_and_an_unplug() {
    let Some(tools) = tools() else { return };
    let inventory = FakeInventory::new(PROFILER_THREE, FFMPEG_THREE);
    let opened: Arc<Mutex<Vec<Connection>>> = Arc::default();
    let (fake, record) = (inventory.clone(), opened.clone());
    let limits = options(&tools, Duration::from_secs(20), Duration::from_secs(20));
    let opener = move || -> Result<Box<dyn VideoSource>, MediaError> {
        let src =
            CaptureSource::open_input(cam(), BRIO, &tiny(), &*fake, limits.clone(), &testsrc)?;
        lock(&record).push(Connection {
            index: src.device().index,
            pids: src.child_pids(),
        });
        Ok(Box::new(src))
    };
    let offline = SourceInfo {
        id: cam(),
        width: 64,
        height: 36,
        fps: 30.0,
        audio: None,
    };
    let backoff = Backoff {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(400),
    };
    let mut src =
        SupervisedSource::start(opener, offline, Arc::new(WallClock::new()), backoff).unwrap();
    assert_eq!(src.start_error(), None);
    let mut frames: Vec<VideoFrame> = Vec::new();
    let mut on_air = |src: &SupervisedSource, epoch: u64| {
        eventually(Duration::from_secs(20), || {
            if let Some(f) = src.wait_newer(frames.last().map(|f| f.seq), Duration::ZERO) {
                frames.push(f);
            }
            frames.last().is_some_and(|f| f.epoch == epoch) && src.state() == SourceState::Running
        })
    };
    let connections = || lock(&opened).clone();
    assert!(on_air(&src, 0));
    assert_eq!(connections()[0].index, 1, "the webcam is ffmpeg device 1");
    assert_eq!(inventory.asked(), 1, "resolved before the first open");

    // Replugged into the same port, it now lists after the dongle; then its
    // ffmpeg dies as a device's would when the camera went away.
    inventory.set(PROFILER_REPLUGGED, FFMPEG_REPLUGGED);
    kill(connections()[0].pids[0]);
    assert!(eventually(Duration::from_secs(20), || {
        src.error().is_some_and(|e| e.contains("ended after"))
    }));
    assert!(on_air(&src, 1));
    assert_eq!(
        connections()[1].index,
        2,
        "the same unique ID, at its new index"
    );
    assert_eq!(inventory.asked(), 2, "resolved again before the reconnect");
    assert_eq!(src.error(), None);
    assert!(
        !alive(connections()[0].pids[0]),
        "the killed ffmpeg was reaped"
    );

    // Unplugged: the old index is the dongle's now, and nothing is opened
    // in the camera's place.
    inventory.set(PROFILER_UNPLUGGED, FFMPEG_UNPLUGGED);
    kill(connections()[1].pids[0]);
    assert!(eventually(Duration::from_secs(20), || {
        src.state() == SourceState::Offline
            && src.error().is_some_and(|e| {
                e.contains(r#"capture device "0x14200000046d085e": no camera with unique ID"#)
            })
    }));
    let asked = inventory.asked();
    assert!(eventually(Duration::from_secs(20), || inventory.asked() >= asked + 2));
    assert_eq!(connections().len(), 2, "no ffmpeg without the camera");
    assert!(
        src.latest().is_some_and(|f| f.epoch == 1),
        "no frame of another device"
    );

    // Plugged back in: on air again under a new epoch.
    inventory.set(PROFILER_THREE, FFMPEG_THREE);
    assert!(on_air(&src, 2));
    assert_eq!(connections()[2].index, 1);
    assert!(
        frames
            .windows(2)
            .all(|w| w[0].seq < w[1].seq && w[0].pts < w[1].pts),
        "sequence numbers and PTS keep increasing across connections"
    );
    src.stop();
    for c in connections() {
        assert!(
            c.pids.iter().all(|&p| !alive(p)),
            "an ffmpeg outlived its source"
        );
    }
}

/// A mode the device lacks fails the source at once, with ffmpeg's own
/// words as its error, instead of leaving it starting.
#[cfg(unix)]
#[test]
fn a_refused_mode_is_a_visible_source_error_not_a_hang() {
    let Some(tools) = tools() else { return };
    let inventory = FakeInventory::new(PROFILER_THREE, FFMPEG_THREE);
    // ffmpeg refuses this input as avfoundation refuses an unsupported
    // mode: errors on stderr, exit without a frame.
    let refused = |_: &ResolvedDevice, _: &CaptureFormat| -> Vec<OsString> {
        [
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x36:rate=30:no_such_mode=1",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    };
    let started = Instant::now();
    let limits = options(&tools, Duration::from_secs(60), Duration::from_secs(60));
    let mut src =
        CaptureSource::open_input(cam(), BRIO, &tiny(), &*inventory, limits, &refused).unwrap();
    assert!(eventually(Duration::from_secs(20), || src.state()
        == SourceState::Stopped));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "failed long before the watchdog's minute"
    );
    let e = src.error().unwrap();
    assert!(
        e.starts_with(r#"ffmpeg could not capture from "Logitech BRIO""#),
        "{e}"
    );
    assert!(e.contains("no_such_mode"), "ffmpeg's reason is quoted: {e}");
    assert!(!e.contains(" @ 0x"), "without ffmpeg's log context: {e}");
    assert!(src.latest().is_none());
    src.stop();
    assert!(src.child_pids().is_empty());
}

/// The fake ffmpeg of `tests/fixtures/capture/fake-ffmpeg.sh`, whose
/// input names what the device does; it needs no ffmpeg.
#[cfg(unix)]
fn fake_ffmpeg() -> FfmpegTools {
    FfmpegTools {
        ffmpeg: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capture/fake-ffmpeg.sh"),
        ffprobe: "ffprobe".into(),
    }
}

/// The input options naming `device` to [`fake_ffmpeg`].
#[cfg(unix)]
fn fake_device(device: &'static str) -> impl Fn(&ResolvedDevice, &CaptureFormat) -> Vec<OsString> {
    move |_, _| vec!["-i".into(), device.into()]
}

/// Opens [`fake_ffmpeg`]'s `device` and waits for the connection to fail;
/// the source's error.
#[cfg(unix)]
fn fake_failure(device: &'static str) -> String {
    let inventory = FakeInventory::new(PROFILER_THREE, FFMPEG_THREE);
    let limits = options(
        &fake_ffmpeg(),
        Duration::from_secs(60),
        Duration::from_secs(60),
    );
    let input = fake_device(device);
    let mut src =
        CaptureSource::open_input(cam(), BRIO, &tiny(), &*inventory, limits, &input).unwrap();
    assert!(eventually(Duration::from_secs(20), || src.state()
        == SourceState::Stopped));
    let e = src.error().unwrap();
    src.stop();
    assert!(src.child_pids().is_empty());
    e
}

/// A refusal's whole list of modes is in the error, which is where the
/// operator reads which modes the camera has, not cut to its first lines.
#[cfg(unix)]
#[test]
fn a_refusal_quotes_every_mode_the_device_lists() {
    let e = fake_failure("refuse");
    assert!(
        e.starts_with(r#"ffmpeg could not capture from "Logitech BRIO""#),
        "{e}"
    );
    assert!(!e.contains("more lines"), "nothing left out: {e}");
    assert!(
        e.contains("(exit status: 1): Selected framerate (29.970030) is not supported by the device.; Supported modes:; 96x54@[5.000000 30.000000]fps; "),
        "{e}"
    );
    for i in 1..=20 {
        let mode = format!("; {}x{}@[5.000000 30.000000]fps;", i * 96, i * 54);
        assert!(e.contains(&mode), "{mode} is missing: {e}");
    }
    assert!(
        e.ends_with("; Error opening input: Input/output error"),
        "{e}"
    );
}

/// A failure after frames quotes ffmpeg's first and last lines only.
#[cfg(unix)]
#[test]
fn a_failure_after_frames_quotes_the_ends_of_what_ffmpeg_said() {
    let e = fake_failure("fail-later");
    assert!(
        e.starts_with(r#"capture from "Logitech BRIO" (unique ID 0x14200000046d085e, ffmpeg device 1) ended after 3 frames (exit status: 1): late line 0; late line 1; late line 2; late line 3; (12 more lines); late line 16; "#),
        "{e}"
    );
    assert!(e.ends_with("; late line 19"), "{e}");
}

/// A device that opens but never delivers (here a FIFO no one writes) is
/// cut at the open timeout.
#[cfg(unix)]
#[test]
fn a_device_that_never_delivers_is_cut_at_the_open_timeout() {
    let Some(tools) = tools() else { return };
    let (_dir, fifo) = fifo("silent");
    let inventory = FakeInventory::new(PROFILER_THREE, FFMPEG_THREE);
    let input = |_: &ResolvedDevice, format: &CaptureFormat| from_fifo(&fifo, format);
    let limits = options(&tools, Duration::from_millis(300), Duration::from_secs(60));
    let mut src =
        CaptureSource::open_input(cam(), BRIO, &tiny(), &*inventory, limits, &input).unwrap();
    let pids = src.child_pids();
    assert!(eventually(Duration::from_secs(20), || src.state()
        == SourceState::Stopped));
    let e = src.error().unwrap();
    assert!(
        e.starts_with(r#"no picture from "Logitech BRIO" (unique ID 0x14200000046d085e, ffmpeg device 1) within 0.3 s"#),
        "{e}"
    );
    src.stop();
    assert!(
        pids.iter().all(|&p| !alive(p)),
        "the silent ffmpeg was killed"
    );
}

/// A device behind a FIFO that delivered a few frames and fell silent:
/// its writer holds the FIFO open, so ffmpeg sees no end of input, until
/// [`Silent::end`].
#[cfg(unix)]
struct Silent {
    /// Raised to let the writer close the FIFO.
    done: Arc<AtomicBool>,
    /// Raised after opening or writing had to wait for the reader.
    waiting: Arc<AtomicBool>,
    /// The writer.
    writer: JoinHandle<()>,
}

#[cfg(unix)]
impl Silent {
    /// Writes `frames` mid-grey frames of [`tiny`]'s size into `fifo` once
    /// ffmpeg has opened it, then holds it open. Opening and writing are
    /// nonblocking so a failed reader cannot prevent shutdown.
    fn after(fifo: &Path, frames: usize) -> Self {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let (w, h) = tiny().size();
        let bytes = vec![128u8; w as usize * h as usize * 4 * frames];
        let (path, done) = (fifo.to_owned(), Arc::new(AtomicBool::new(false)));
        let held = done.clone();
        let waiting = Arc::new(AtomicBool::new(false));
        let blocked = waiting.clone();
        let writer = std::thread::spawn(move || {
            let mut device = loop {
                if held.load(Ordering::Acquire) {
                    return;
                }
                match std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(device) => break device,
                    // No reader yet, including ffmpeg failing before open.
                    Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                        blocked.store(true, Ordering::Release);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("open {}: {error}", path.display()),
                }
            };
            let mut remaining = bytes.as_slice();
            while !remaining.is_empty() && !held.load(Ordering::Acquire) {
                match device.write(remaining) {
                    Ok(0) => panic!("FIFO writer made no progress"),
                    Ok(written) => remaining = &remaining[written..],
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        blocked.store(true, Ordering::Release);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => return,
                    Err(error) => panic!("write {}: {error}", path.display()),
                }
            }
            while !held.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Self {
            done,
            waiting,
            writer,
        }
    }

    /// Closes the FIFO and waits for the writer.
    fn end(self) {
        self.done.store(true, Ordering::Release);
        self.writer.join().unwrap();
    }
}

/// The stand-in shuts down even if ffmpeg never opens its reader.
#[cfg(unix)]
#[test]
fn a_silent_device_can_stop_without_a_reader() {
    let (_dir, fifo) = fifo("no-reader");
    let device = Silent::after(&fifo, 3);
    let waiting = eventually(Duration::from_secs(5), || {
        device.waiting.load(Ordering::Acquire)
    });
    let (ended, observed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        device.end();
        ended.send(()).unwrap();
    });
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(waiting, "the writer attempted to open without a reader");
}

/// Cancelling a blocked write does not require the reader to drain it.
#[cfg(unix)]
#[test]
fn a_silent_device_can_stop_while_its_reader_does_not_drain() {
    use std::os::unix::fs::OpenOptionsExt;
    let (_dir, fifo) = fifo("reader-not-draining");
    let _reader = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let device = Silent::after(&fifo, 1024);
    let waiting = eventually(Duration::from_secs(5), || {
        device.waiting.load(Ordering::Acquire)
    });
    let (ended, observed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        device.end();
        ended.send(()).unwrap();
    });
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(waiting, "the write filled the undrained FIFO");
}

/// A device that delivers and then falls silent without its ffmpeg
/// exiting (a dongle that lost its signal) is cut at the stall timeout.
#[cfg(unix)]
#[test]
fn a_device_that_falls_silent_is_cut_at_the_stall_timeout() {
    let Some(tools) = tools() else { return };
    let (_dir, fifo) = fifo("stall");
    let inventory = FakeInventory::new(PROFILER_THREE, FFMPEG_THREE);
    let input = |_: &ResolvedDevice, format: &CaptureFormat| from_fifo(&fifo, format);
    // Both limits end well inside the wait below, so a failure shows which
    // one cut the source, or that neither did.
    let limits = options(&tools, Duration::from_secs(10), Duration::from_millis(300));
    let mut src =
        CaptureSource::open_input(cam(), BRIO, &tiny(), &*inventory, limits, &input).unwrap();
    // Three frames, then silence with the FIFO held open until the source
    // has given up on it.
    let device = Silent::after(&fifo, 3);
    let stopped = eventually(Duration::from_secs(20), || {
        src.state() == SourceState::Stopped
    });
    let (state, newest) = (src.state(), src.latest().map(|f| f.seq));
    device.end();
    assert!(stopped, "still {state:?}, newest frame {newest:?}");
    assert!(newest.is_some(), "it delivered before falling silent");
    let e = src.error().unwrap();
    assert!(
        e.contains("stopped delivering pictures: none for 0.3 s"),
        "{e}"
    );
    src.stop();
    assert!(src.child_pids().is_empty());
}

/// Every frame is published as soon as the device delivers it, not when a
/// later one arrives: the last frame before a silence is on air during the
/// silence. (A frame-threaded encoder in ffmpeg held back one frame, and
/// under load every frame of a short burst, so that a device that fell
/// silent could look as if it had never opened.)
#[cfg(unix)]
#[test]
fn a_frame_is_published_without_waiting_for_the_next() {
    let Some(tools) = tools() else { return };
    let (_dir, fifo) = fifo("held");
    let inventory = FakeInventory::new(PROFILER_THREE, FFMPEG_THREE);
    let input = |_: &ResolvedDevice, format: &CaptureFormat| from_fifo(&fifo, format);
    let limits = options(&tools, Duration::from_secs(20), Duration::from_secs(20));
    let mut src =
        CaptureSource::open_input(cam(), BRIO, &tiny(), &*inventory, limits, &input).unwrap();
    let device = Silent::after(&fifo, 3);
    // Well inside both limits: the source is not cut while it waits.
    let last = src.wait_newer(Some(1), Duration::from_secs(10));
    let newest = src.latest().map(|f| f.seq);
    let state = src.state();
    src.stop();
    device.end();
    assert_eq!(
        last.map(|f| f.seq),
        Some(2),
        "the third frame never came; newest {newest:?}"
    );
    assert_eq!(state, SourceState::Running, "on air while silent");
    assert_eq!(src.error(), None, "a stop that was asked for is no error");
    assert!(src.child_pids().is_empty());
}

/// A camera that is not connected is an open error that names it, and no
/// ffmpeg is started.
#[test]
fn a_missing_camera_fails_the_open_with_no_fallback() {
    let inventory = FakeInventory::new(PROFILER_UNPLUGGED, FFMPEG_UNPLUGGED);
    let called = AtomicBool::new(false);
    let input = |_: &ResolvedDevice, _: &CaptureFormat| {
        called.store(true, Ordering::Release);
        Vec::new()
    };
    let limits = CaptureOptions::default();
    let e = CaptureSource::open_input(cam(), BRIO, &tiny(), &*inventory, limits, &input)
        .unwrap_err()
        .to_string();
    assert!(e.contains("no camera with unique ID"), "{e}");
    assert!(
        !called.load(Ordering::Acquire),
        "nothing opened in its place"
    );
}

/// A capture config opens through the factory, like every source, with the
/// real inventory: here (no camera with this ID, or no macOS) it fails to
/// open, and a supervised one starts offline saying why, naming the ID.
#[test]
fn a_capture_config_resolves_through_the_system_inventory() {
    let config = SourceConfig::Capture {
        unique_id: "auto-camera-test-no-such-camera".into(),
        width: 64,
        height: 36,
        fps: 30.0,
        pixel_format: None,
    };
    assert!(config.validate().is_ok());
    match open_source(&config, cam()) {
        Err(MediaError::Open(e)) => {
            assert!(
                e.contains(r#"capture device "auto-camera-test-no-such-camera""#),
                "{e}"
            );
        }
        other => panic!("opened a camera that is not there: {:?}", other.is_ok()),
    }
    let offline = SourceInfo {
        id: cam(),
        width: 64,
        height: 36,
        fps: 30.0,
        audio: None,
    };
    let mut src = open_supervised(
        &config,
        offline,
        Arc::new(WallClock::new()),
        Backoff::default(),
    )
    .unwrap();
    assert_eq!(src.state(), SourceState::Offline);
    assert!(
        src.start_error()
            .is_some_and(|e| e.contains("auto-camera-test-no-such-camera"))
    );
    src.stop();
}

/// The resolver's errors all name the unique ID, whichever step failed.
#[test]
fn every_resolver_error_names_the_unique_id() {
    let broken = FakeInventory::new("not json", FFMPEG_THREE);
    let e = resolve_device(&*broken, BRIO).unwrap_err().to_string();
    assert!(
        e.contains(r#"capture device "0x14200000046d085e": unreadable"#),
        "{e}"
    );
    let no_avf = FakeInventory::new(
        PROFILER_THREE,
        include_str!("../../tests/fixtures/capture/ffmpeg-no-avfoundation.txt"),
    );
    let e = resolve_device(&*no_avf, BRIO).unwrap_err().to_string();
    assert!(
        e.contains(r#"capture device "0x14200000046d085e": ffmpeg lists no"#),
        "{e}"
    );
    let found = resolve_device(&*FakeInventory::new(PROFILER_THREE, FFMPEG_THREE), BRIO).unwrap();
    assert_eq!(found.index, 1);
}
