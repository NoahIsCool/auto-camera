//! The threads that move data between the engine and the ffmpeg child.
//!
//! Each pipe has exactly one owner thread: the video writer owns ffmpeg's
//! stdin, the audio writer owns the audio socket, and two readers own stdout
//! (progress and mux statistics) and stderr (errors and the stream monitor);
//! the readers are in the `readers` child module. The engine talks to the
//! writers only through bounded channels; the threads publish counters and
//! snapshots into [`Shared`], whose locks are held for a copy and never
//! across I/O.
//!
//! # Errors
//!
//! [`Shared`] keeps the newest error line, and apart from it the newest one
//! that was not the stream's ([`ErrorLog`]). A reconnection of the stream
//! drops the stream's error, so a stream that recovered shows no error and
//! an older error of something else shows again.
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

use std::io::{self, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::progress::Progress;
use crate::stream::StreamMonitor;

mod readers;

pub use readers::{progress_reader, stderr_reader};

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

/// The newest error lines; see the module docs.
#[derive(Debug, Default)]
pub struct ErrorLog {
    /// The newest error line.
    newest: Option<String>,
    /// `newest` was the stream's.
    newest_is_stream: bool,
    /// The newest error line that was not the stream's.
    other: Option<String>,
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
    /// Newest error lines from ffmpeg or a writer, redacted.
    pub last_error: Mutex<ErrorLog>,
    /// Newest complete progress report.
    pub progress: Mutex<Progress>,
    /// Bitrate ffmpeg muxed over the newest window, from mux statistics.
    pub muxed_kbps: Mutex<Option<f64>>,
    /// Copy of the stderr reader's stream monitor; `None` without a stream.
    pub stream: Mutex<Option<StreamMonitor>>,
    /// Spare frame buffers.
    pub pool: Mutex<Vec<Vec<u8>>>,
}

impl Shared {
    /// Records `msg` as the newest error, truncated to a sane length.
    pub fn record_error(&self, msg: &str) {
        self.record(msg, false);
    }

    /// Records `msg`, an error of the stream, as the newest error; the
    /// stream's next reconnection clears it ([`Shared::clear_stream_error`]).
    pub fn record_stream_error(&self, msg: &str) {
        self.record(msg, true);
    }

    /// Records `msg` as the newest error, the stream's or not.
    fn record(&self, msg: &str, stream: bool) {
        let msg: String = msg.chars().take(MAX_ERROR_CHARS).collect();
        let mut log = lock(&self.last_error);
        if !stream {
            log.other = Some(msg.clone());
        }
        log.newest = Some(msg);
        log.newest_is_stream = stream;
    }

    /// Records a pipe failure unless an error is already known.
    ///
    /// A broken pipe is a symptom of ffmpeg exiting; ffmpeg's own stderr
    /// line says why, and stderr lines always win, so the symptom never
    /// hides the cause regardless of which thread notices first.
    pub fn record_pipe_error(&self, msg: &str) {
        if lock(&self.last_error).newest.is_none() {
            self.record(msg, false);
        }
    }

    /// Drops the newest error if it was the stream's, so the newest other
    /// error (if any) shows again: the stream reconnected.
    pub fn clear_stream_error(&self) {
        let mut log = lock(&self.last_error);
        if log.newest_is_stream {
            log.newest = log.other.clone();
            log.newest_is_stream = false;
        }
    }

    /// The newest error line.
    pub fn last_error(&self) -> Option<String> {
        lock(&self.last_error).newest.clone()
    }

    /// The bitrate ffmpeg muxed over the newest window, when measured.
    pub fn muxed_kbps(&self) -> Option<f64> {
        *lock(&self.muxed_kbps)
    }

    /// The newest copy of the stream monitor.
    pub fn stream(&self) -> Option<StreamMonitor> {
        *lock(&self.stream)
    }

    /// Publishes a copy of the stream monitor.
    pub fn publish_stream(&self, monitor: StreamMonitor) {
        *lock(&self.stream) = Some(monitor);
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

#[cfg(test)]
mod tests;
