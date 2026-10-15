//! The threads that move data between the engine and the ffmpeg child.
//!
//! Each pipe has exactly one owner thread: the video writer owns ffmpeg's
//! stdin, the audio writer owns the audio socket, and two readers own stdout
//! (progress) and stderr (errors). The engine talks to the writers only
//! through bounded channels; the threads publish counters and snapshots into
//! [`Shared`], whose locks are held for a copy and never across I/O.
//!
//! # Drops keep the clock
//!
//! ffmpeg timestamps raw input by count: frame `n` is at `n / fps`, sample
//! `n` at `n / rate`. Losing an input would therefore shift everything after
//! it earlier and break A/V sync for the rest of the program. So a drop
//! records a *debt*: before writing its next frame the video writer repeats
//! the previous one once per dropped frame, and after each block the audio
//! writer writes one zero sample per dropped sample. Debt still owed when the
//! input closes is settled before end of file. Output stutters (or goes
//! briefly silent) instead of drifting. Debts are capped at [`MAX_DEBT_SECONDS`] so a stalled ffmpeg
//! cannot cause an unbounded burst when it recovers; beyond the cap, sync is
//! lost and the drop counters say so.
//!
//! # Audio over loopback TCP, not a FIFO
//!
//! ffmpeg opens its second input only after it has read the first video
//! frame. Opening a FIFO for writing blocks until the reader opens it, and if
//! ffmpeg dies first the writer stays stuck in `open(2)` forever: killing
//! ffmpeg does not release it, and a non-blocking open needs `libc` (this
//! workspace forbids `unsafe`). A listener on `127.0.0.1` with an ephemeral
//! port instead accepts with a deadline that is abandoned when ffmpeg exits,
//! needs no filesystem node to clean up, works on every OS, and writes fail
//! with an error rather than hanging once ffmpeg is gone. The listener only
//! accepts one connection, from the local host.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::redact;
use crate::progress::{Progress, ProgressParser, average_kbps};

/// Frames the engine may queue ahead of the video writer.
pub const VIDEO_QUEUE_FRAMES: usize = 4;

/// Audio buffers the engine may queue ahead of the audio writer.
pub const AUDIO_QUEUE_BLOCKS: usize = 256;

/// Largest drop debt, in seconds of media, that is repaid.
pub const MAX_DEBT_SECONDS: u64 = 10;

/// How long ffmpeg has to connect to the audio input.
pub const AUDIO_ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest error line kept, in characters.
const MAX_ERROR_CHARS: usize = 512;

/// Frame buffers kept for reuse, so steady state allocates nothing.
const POOL_FRAMES: usize = VIDEO_QUEUE_FRAMES + 2;

/// Zero samples written per silence chunk.
const SILENCE_CHUNK: usize = 4096;

/// Locks `m`, ignoring poisoning (the data are plain snapshots).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// State shared between the encoder handle and its threads.
#[derive(Default)]
pub struct Shared {
    /// Valid frames pushed.
    pub frames_in: AtomicU64,
    /// Frames not queued.
    pub frames_dropped: AtomicU64,
    /// Repeated frames written to repay drops.
    pub frames_repeated: AtomicU64,
    /// Frames written to ffmpeg, repeats included.
    pub frames_written: AtomicU64,
    /// Frames currently in the video channel.
    pub video_queued: AtomicUsize,
    /// Dropped frames not yet repaid.
    pub video_debt: AtomicU64,
    /// Set when the video writer has exited.
    pub video_closed: AtomicBool,
    /// Valid samples pushed (all channels).
    pub audio_samples_in: AtomicU64,
    /// Samples not queued.
    pub audio_samples_dropped: AtomicU64,
    /// Samples written to ffmpeg, silence included.
    pub audio_samples_written: AtomicU64,
    /// Zero samples written to repay drops.
    pub audio_silence_samples: AtomicU64,
    /// Dropped samples not yet repaid.
    pub audio_debt: AtomicU64,
    /// Set once ffmpeg has connected to the audio input.
    pub audio_connected: AtomicBool,
    /// Set when the audio writer has exited.
    pub audio_closed: AtomicBool,
    /// Set when the encoder is being torn down; threads stop repaying debt.
    pub stop: AtomicBool,
    /// Set when ffmpeg's stdout reached end of file (it has exited).
    pub stdout_closed: AtomicBool,
    /// Newest error line from ffmpeg or a writer, redacted.
    pub last_error: Mutex<Option<String>>,
    /// Newest complete progress report.
    pub progress: Mutex<Progress>,
    /// Spare frame buffers.
    pub pool: Mutex<Vec<Vec<u8>>>,
}

impl Shared {
    /// Records `msg` as the newest error, truncated to a sane length.
    pub fn record_error(&self, msg: &str) {
        let msg: String = msg.chars().take(MAX_ERROR_CHARS).collect();
        *lock(&self.last_error) = Some(msg);
    }

    /// Records a pipe failure unless an error is already known.
    ///
    /// A broken pipe is a symptom of ffmpeg exiting; ffmpeg's own stderr
    /// line says why, and stderr lines always win, so the symptom never
    /// hides the cause regardless of which thread notices first.
    pub fn record_pipe_error(&self, msg: &str) {
        let mut last = lock(&self.last_error);
        if last.is_none() {
            *last = Some(msg.chars().take(MAX_ERROR_CHARS).collect());
        }
    }

    /// The newest error line.
    pub fn last_error(&self) -> Option<String> {
        lock(&self.last_error).clone()
    }

    /// The newest progress report.
    pub fn progress(&self) -> Progress {
        lock(&self.progress).clone()
    }

    /// A spare frame buffer, or a new empty one.
    pub fn take_buffer(&self) -> Vec<u8> {
        lock(&self.pool).pop().unwrap_or_default()
    }

    /// Returns a frame buffer to the pool unless the pool is full.
    pub fn recycle(&self, buf: Vec<u8>) {
        let mut pool = lock(&self.pool);
        if pool.len() < POOL_FRAMES {
            pool.push(buf);
        }
    }

    /// True once teardown has begun.
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// Adds `n` to a debt counter, saturating at `cap`.
pub fn add_debt(debt: &AtomicU64, n: u64, cap: u64) {
    let _ = debt.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |d| {
        Some(d.saturating_add(n).min(cap))
    });
}

/// Writes frames from `rx` to ffmpeg's stdin until the channel closes.
///
/// Dropping `stdin` on return is what signals end of video to ffmpeg.
pub fn video_writer(rx: Receiver<Vec<u8>>, mut stdin: impl Write, shared: Arc<Shared>) {
    let mut last: Option<Vec<u8>> = None;
    let mut run = || -> io::Result<()> {
        while let Ok(frame) = rx.recv() {
            shared.video_queued.fetch_sub(1, Ordering::SeqCst);
            // Frames dropped since `last` was queued belong between it and
            // this frame.
            repay_video(&mut stdin, last.as_deref(), &shared)?;
            stdin.write_all(&frame)?;
            shared.frames_written.fetch_add(1, Ordering::Relaxed);
            if let Some(old) = last.replace(frame) {
                shared.recycle(old);
            }
        }
        if !shared.stopping() {
            repay_video(&mut stdin, last.as_deref(), &shared)?;
        }
        stdin.flush()
    };
    if let Err(e) = run()
        && !shared.stopping()
    {
        shared.record_pipe_error(&format!("video pipe: {e}"));
    }
    shared.video_closed.store(true, Ordering::SeqCst);
}

/// Repeats `frame` once per unpaid dropped frame. Before the first frame
/// there is nothing to repeat, so the debt waits.
fn repay_video(out: &mut impl Write, frame: Option<&[u8]>, shared: &Shared) -> io::Result<()> {
    let Some(frame) = frame else {
        return Ok(());
    };
    let owed = shared.video_debt.swap(0, Ordering::SeqCst);
    for _ in 0..owed {
        out.write_all(frame)?;
        shared.frames_repeated.fetch_add(1, Ordering::Relaxed);
        shared.frames_written.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

/// Accepts ffmpeg's audio connection, then writes samples from `rx` to it
/// as `f32le` until the channel closes.
pub fn audio_writer(
    rx: Receiver<Vec<f32>>,
    listener: TcpListener,
    shared: Arc<Shared>,
    accept_timeout: Duration,
) {
    let run = || -> io::Result<()> {
        let mut stream = accept_until(&listener, Instant::now() + accept_timeout, &shared)?;
        drop(listener);
        shared.audio_connected.store(true, Ordering::SeqCst);
        let mut bytes = Vec::new();
        while let Ok(block) = rx.recv() {
            write_samples(&mut stream, &block, &mut bytes)?;
            let n = block.len() as u64;
            shared.audio_samples_written.fetch_add(n, Ordering::Relaxed);
            repay_audio(&mut stream, &shared, &mut bytes)?;
        }
        if !shared.stopping() {
            repay_audio(&mut stream, &shared, &mut bytes)?;
        }
        stream.flush()?;
        // End of audio for ffmpeg; a peer that already left is not an error.
        match stream.shutdown(Shutdown::Write) {
            Err(e) if e.kind() != io::ErrorKind::NotConnected => Err(e),
            _ => Ok(()),
        }
    };
    if let Err(e) = run()
        && !shared.stopping()
    {
        shared.record_pipe_error(&format!("audio pipe: {e}"));
    }
    shared.audio_closed.store(true, Ordering::SeqCst);
}

/// Waits for one connection until `deadline`, giving up early when ffmpeg
/// has exited or the encoder is being torn down.
fn accept_until(
    listener: &TcpListener,
    deadline: Instant,
    shared: &Shared,
) -> io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted sockets inherit non-blocking mode on BSD/macOS.
                stream.set_nonblocking(false)?;
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
        if shared.stopping() || shared.stdout_closed.load(Ordering::SeqCst) {
            return Err(io::Error::other(
                "ffmpeg exited before opening the audio input",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "ffmpeg did not connect to the audio input",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Writes `samples` as little-endian `f32`, replacing non-finite values with
/// silence so a NaN from upstream cannot reach the AAC encoder.
fn write_samples(out: &mut impl Write, samples: &[f32], bytes: &mut Vec<u8>) -> io::Result<()> {
    bytes.clear();
    bytes.reserve(samples.len() * 4);
    for s in samples {
        let s = if s.is_finite() { *s } else { 0.0 };
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    out.write_all(bytes)
}

/// Writes one zero sample per unpaid dropped sample.
fn repay_audio(out: &mut impl Write, shared: &Shared, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut owed = shared.audio_debt.swap(0, Ordering::SeqCst) as usize;
    let zeros = [0.0f32; SILENCE_CHUNK];
    while owed > 0 {
        let n = owed.min(SILENCE_CHUNK);
        write_samples(out, &zeros[..n], bytes)?;
        shared
            .audio_silence_samples
            .fetch_add(n as u64, Ordering::Relaxed);
        shared
            .audio_samples_written
            .fetch_add(n as u64, Ordering::Relaxed);
        owed -= n;
    }
    Ok(())
}

/// Calls `f` with each line of `input` (lossily decoded) until end of file.
fn for_each_line(input: impl Read, mut f: impl FnMut(&str)) {
    let mut reader = BufReader::new(input);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => f(&String::from_utf8_lossy(&line)),
        }
    }
}

/// Parses ffmpeg's `-progress` stream from stdout into [`Shared::progress`].
///
/// When ffmpeg reports no bitrate (the tee case) and a recording exists,
/// the bitrate is the recording's average so far.
pub fn progress_reader(stdout: impl Read, shared: Arc<Shared>, record_path: Option<PathBuf>) {
    let mut parser = ProgressParser::new();
    for_each_line(stdout, |line| {
        if let Some(mut report) = parser.feed(line) {
            if report.bitrate_kbps.is_none()
                && let (Some(path), Some(t)) = (&record_path, report.out_time)
                && let Ok(meta) = std::fs::metadata(path)
            {
                report.bitrate_kbps = average_kbps(meta.len(), t);
            }
            *lock(&shared.progress) = report;
        }
    });
    shared.stdout_closed.store(true, Ordering::SeqCst);
}

/// Keeps the newest non-empty stderr line, with the stream key redacted.
pub fn stderr_reader(stderr: impl Read, shared: Arc<Shared>, rtmp_url: Option<String>) {
    for_each_line(stderr, |line| {
        for piece in line.split('\r').map(str::trim).filter(|p| !p.is_empty()) {
            shared.record_error(&redact(piece, rtmp_url.as_deref()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    /// A writer that fails after `ok` successful writes.
    struct FailAfter {
        /// Writes left before failing.
        ok: usize,
        /// Bytes accepted so far.
        written: Vec<u8>,
    }

    impl Write for FailAfter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.ok == 0 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.ok -= 1;
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Runs the video writer over `frames` with `debt` dropped frames owed
    /// up front, returning the bytes written and the shared state.
    fn run_video(frames: &[&[u8]], debt: u64, stopping: bool) -> (Vec<u8>, Arc<Shared>) {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = sync_channel(frames.len().max(1));
        for f in frames {
            shared.video_queued.fetch_add(1, Ordering::SeqCst);
            tx.send(f.to_vec()).unwrap();
        }
        drop(tx);
        shared.video_debt.store(debt, Ordering::SeqCst);
        shared.stop.store(stopping, Ordering::SeqCst);
        let mut out = Vec::new();
        video_writer(rx, &mut out, shared.clone());
        (out, shared)
    }

    #[test]
    fn video_writer_repays_drops_with_the_previous_frame() {
        let (out, shared) = run_video(&[&[1, 1], &[2, 2]], 2, false);
        // Nothing to repeat before frame 1; the debt is paid before frame 2.
        assert_eq!(out, [1, 1, 1, 1, 1, 1, 2, 2]);
        assert_eq!(shared.frames_written.load(Ordering::SeqCst), 4);
        assert_eq!(shared.frames_repeated.load(Ordering::SeqCst), 2);
        assert_eq!(shared.video_queued.load(Ordering::SeqCst), 0);
        assert!(shared.video_closed.load(Ordering::SeqCst));
        assert_eq!(shared.last_error(), None);
    }

    #[test]
    fn video_writer_settles_debt_at_close_unless_stopping() {
        let (out, _) = run_video(&[&[7]], 2, false);
        assert_eq!(out, [7, 7, 7]);
        let (out, shared) = run_video(&[&[7]], 2, true);
        assert_eq!(out, [7]);
        assert_eq!(shared.frames_repeated.load(Ordering::SeqCst), 0);
        // No frame at all: nothing written, nothing to repeat.
        let (out, _) = run_video(&[], 3, false);
        assert!(out.is_empty());
    }

    #[test]
    fn video_writer_records_pipe_errors() {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = sync_channel(4);
        shared.video_queued.store(2, Ordering::SeqCst);
        tx.send(vec![1u8]).unwrap();
        tx.send(vec![2u8]).unwrap();
        let sink = FailAfter {
            ok: 1,
            written: Vec::new(),
        };
        video_writer(rx, sink, shared.clone());
        assert!(shared.last_error().unwrap().starts_with("video pipe:"));
        assert!(shared.video_closed.load(Ordering::SeqCst));
        // The receiver is gone, so further sends fail.
        assert!(tx.try_send(vec![3]).is_err());
    }

    #[test]
    fn samples_are_little_endian_and_sanitised() {
        let mut out = Vec::new();
        let mut bytes = Vec::new();
        write_samples(&mut out, &[1.0, f32::NAN, -0.5, f32::INFINITY], &mut bytes).unwrap();
        let back: Vec<f32> = out
            .chunks(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(back, [1.0, 0.0, -0.5, 0.0]);
    }

    #[test]
    fn audio_debt_becomes_silence() {
        let shared = Shared::default();
        shared
            .audio_debt
            .store(SILENCE_CHUNK as u64 + 3, Ordering::SeqCst);
        let mut out = Vec::new();
        repay_audio(&mut out, &shared, &mut Vec::new()).unwrap();
        assert_eq!(out.len(), (SILENCE_CHUNK + 3) * 4);
        assert!(out.iter().all(|&b| b == 0));
        assert_eq!(
            shared.audio_silence_samples.load(Ordering::SeqCst),
            SILENCE_CHUNK as u64 + 3
        );
        assert_eq!(shared.audio_debt.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn debt_saturates_at_cap() {
        let d = AtomicU64::new(0);
        add_debt(&d, 3, 5);
        add_debt(&d, 3, 5);
        assert_eq!(d.load(Ordering::SeqCst), 5);
        add_debt(&d, u64::MAX, u64::MAX);
        assert_eq!(d.load(Ordering::SeqCst), u64::MAX);
    }

    #[test]
    fn audio_writer_streams_over_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Shared::default());
        let (tx, rx) = sync_channel(4);
        tx.send(vec![0.25f32, -0.25]).unwrap();
        drop(tx);
        let s = shared.clone();
        let writer = thread::spawn(move || audio_writer(rx, listener, s, Duration::from_secs(5)));
        let mut client = TcpStream::connect(addr).unwrap();
        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap();
        writer.join().unwrap();
        assert_eq!(got.len(), 8);
        assert_eq!(f32::from_le_bytes(got[..4].try_into().unwrap()), 0.25);
        assert!(shared.audio_connected.load(Ordering::SeqCst));
        assert_eq!(shared.audio_samples_written.load(Ordering::SeqCst), 2);
        assert_eq!(shared.last_error(), None);
    }

    #[test]
    fn audio_accept_times_out_or_aborts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let shared = Arc::new(Shared::default());
        let (_tx, rx) = sync_channel::<Vec<f32>>(1);
        audio_writer(rx, listener, shared.clone(), Duration::from_millis(20));
        assert!(shared.last_error().unwrap().contains("did not connect"));
        assert!(shared.audio_closed.load(Ordering::SeqCst));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let shared = Arc::new(Shared::default());
        shared.stdout_closed.store(true, Ordering::SeqCst);
        let (_tx, rx) = sync_channel::<Vec<f32>>(1);
        audio_writer(rx, listener, shared.clone(), Duration::from_secs(30));
        assert!(shared.last_error().unwrap().contains("exited before"));
    }

    #[test]
    fn readers_publish_progress_and_redacted_errors() {
        let shared = Arc::new(Shared::default());
        let progress = b"frame=5\nfps=30\nout_time_us=166666\nprogress=end\n";
        progress_reader(&progress[..], shared.clone(), None);
        let p = shared.progress();
        assert_eq!(p.frame, 5);
        assert!(p.ended);
        assert_eq!(p.bitrate_kbps, None);
        assert!(shared.stdout_closed.load(Ordering::SeqCst));

        let url = "rtmp://host/live/super-secret";
        let stderr = b"first\r\n[fifo] Error opening rtmp://host/live/super-secret: refused\n\n";
        stderr_reader(&stderr[..], shared.clone(), Some(url.into()));
        let err = shared.last_error().unwrap();
        assert_eq!(
            err,
            "[fifo] Error opening rtmp://host/live/<redacted>: refused"
        );
    }

    #[test]
    fn pipe_errors_never_hide_ffmpeg_errors() {
        let shared = Shared::default();
        shared.record_pipe_error("video pipe: broken");
        assert_eq!(shared.last_error().as_deref(), Some("video pipe: broken"));
        shared.record_error("ffmpeg: the cause");
        shared.record_pipe_error("audio pipe: broken");
        assert_eq!(shared.last_error().as_deref(), Some("ffmpeg: the cause"));
    }

    #[test]
    fn errors_are_truncated_and_buffers_pooled() {
        let shared = Shared::default();
        shared.record_error(&"x".repeat(10 * MAX_ERROR_CHARS));
        assert_eq!(shared.last_error().unwrap().len(), MAX_ERROR_CHARS);
        assert!(shared.take_buffer().is_empty());
        for _ in 0..POOL_FRAMES + 3 {
            shared.recycle(vec![0u8; 4]);
        }
        assert_eq!(lock(&shared.pool).len(), POOL_FRAMES);
        assert_eq!(shared.take_buffer().len(), 4);
    }
}
