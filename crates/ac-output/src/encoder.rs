//! The program encoder: one ffmpeg child and its lifecycle.
//!
//! # Lifecycle
//!
//! [`ProgramEncoder::start`] validates the config, refuses to overwrite an
//! existing recording, resolves the H.264 encoder from `ffmpeg -encoders`,
//! binds the loopback audio listener (when audio is configured), spawns
//! ffmpeg and starts four threads (video writer, audio writer, progress
//! reader, stderr reader). The handle is assembled before the threads start,
//! so a failure at any later step drops it and kills ffmpeg.
//!
//! The engine then calls [`ProgramEncoder::push_video`] exactly once per
//! program tick and [`ProgramEncoder::push_audio`] with the matching audio;
//! for ticks it had to skip it owes repeats with
//! [`ProgramEncoder::repeat_frames`], so the frame count stays on its clock.
//! Neither blocks: a full queue drops the input, counts it in
//! [`EncoderStats`] and returns [`Push::Dropped`]; the writer repays the gap
//! with a repeated frame or silence so timing is preserved. When audio is
//! configured the caller must keep pushing it (silence if nothing else),
//! because ffmpeg interleaves by timestamp and waits for the slower stream.
//!
//! [`ProgramEncoder::finish`] closes both inputs (the writers drain their
//! queues and settle debts first), waits for ffmpeg to flush and exit, and
//! kills it if the timeout passes; the recording stays playable either way
//! because it is fragmented. Dropping a running encoder kills ffmpeg
//! immediately. In every case the child is reaped and all threads joined
//! before the call returns, so no process outlives the handle.

use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ac_media::{AudioBlock, VideoFrame};

use crate::OutputError;
use crate::args::build_ffmpeg_args;
use crate::config::{AudioFormat, Encoder, EncoderChoice, OutputConfig, choose_working_encoder};
use crate::progress::Progress;
use crate::transport::{
    self, AUDIO_ACCEPT_TIMEOUT, AUDIO_QUEUE_BLOCKS, MAX_DEBT_SECONDS, Shared, VIDEO_QUEUE_FRAMES,
};

/// How long [`ProgramEncoder::finish`] waits for ffmpeg before killing it.
pub const DEFAULT_FINISH_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between exit checks while finishing.
const EXIT_POLL: Duration = Duration::from_millis(10);

/// What happened to a pushed frame or audio buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push {
    /// Queued for ffmpeg.
    Queued,
    /// The queue was full; the input was dropped, counted, and will be
    /// replaced by a repeated frame or silence.
    Dropped,
}

/// A snapshot of the encoder's health (spec M14: encoder status, drops,
/// visible errors).
#[derive(Debug, Clone, PartialEq)]
pub struct EncoderStats {
    /// Encoder in use.
    pub encoder: EncoderChoice,
    /// Valid frames pushed.
    pub frames_in: u64,
    /// Frames not queued because the queue was full or ffmpeg had stopped.
    pub frames_dropped: u64,
    /// Repeats of the previous frame written in place of dropped frames.
    pub frames_repeated: u64,
    /// Frames written to ffmpeg, repeats included.
    pub frames_written: u64,
    /// Valid samples pushed (all channels).
    pub audio_samples_in: u64,
    /// Samples not queued.
    pub audio_samples_dropped: u64,
    /// Zero samples written in place of dropped samples.
    pub audio_silence_samples: u64,
    /// Samples written to ffmpeg, silence included.
    pub audio_samples_written: u64,
    /// True once ffmpeg has opened the audio input.
    pub audio_connected: bool,
    /// True while the ffmpeg process is running.
    pub ffmpeg_alive: bool,
    /// ffmpeg's exit code once it has exited (`None` if killed by a signal).
    pub exit_code: Option<i32>,
    /// Newest error line from ffmpeg or a pipe, stream key redacted. It is
    /// not cleared when the condition passes (for example when a dropped
    /// RTMP ingest reconnects), so read it together with the counters.
    pub last_error: Option<String>,
    /// Newest `-progress` report: encoded frames, fps, bitrate, speed.
    pub progress: Progress,
}

/// A running program encode. See the module docs for the lifecycle.
pub struct ProgramEncoder {
    /// The validated configuration.
    config: OutputConfig,
    /// The resolved encoder.
    encoder: EncoderChoice,
    /// The ffmpeg process; locked only for non-blocking status checks.
    child: Mutex<Child>,
    /// ffmpeg's process id.
    pid: u32,
    /// Counters and snapshots shared with the threads.
    shared: Arc<Shared>,
    /// Video queue; `None` once closed.
    video_tx: Option<SyncSender<Vec<u8>>>,
    /// Audio queue; `None` without audio or once closed.
    audio_tx: Option<SyncSender<Vec<f32>>>,
    /// Writer and reader threads, joined on shutdown.
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for ProgramEncoder {
    /// Identity only: the config (stream key redacted), encoder and pid.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramEncoder")
            .field("config", &self.config)
            .field("encoder", &self.encoder)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// Maps a spawn failure, naming the executable when it is missing.
fn spawn_error(ffmpeg: &Path, e: std::io::Error) -> OutputError {
    if e.kind() == std::io::ErrorKind::NotFound {
        OutputError::FfmpegNotFound(ffmpeg.to_path_buf())
    } else {
        OutputError::Io(e)
    }
}

/// Resolves `requested` by asking `ffmpeg -encoders` what it has.
///
/// [`Encoder::Auto`] picks VideoToolbox only on macOS, and only when a
/// one-frame test encode with it succeeds ([`encoder_works`]); see
/// [`choose_working_encoder`].
pub fn detect_encoder(ffmpeg: &Path, requested: Encoder) -> Result<EncoderChoice, OutputError> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| spawn_error(ffmpeg, e))?;
    let listing = String::from_utf8_lossy(&out.stdout);
    choose_working_encoder(requested, &listing, cfg!(target_os = "macos"), || {
        encoder_works(ffmpeg, EncoderChoice::VideoToolbox)
    })
}

/// Longest a test encode may take before it counts as failed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// True when `ffmpeg` can encode a few frames of a small test picture with
/// `choice`, using the same realtime option as a real session. It takes a
/// fraction of a second; a hung probe is killed after ten seconds.
pub fn encoder_works(ffmpeg: &Path, choice: EncoderChoice) -> bool {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "color=c=gray:s=256x144:r=30"])
        .args(["-frames:v", "3", "-c:v", choice.ffmpeg_name()]);
    if choice == EncoderChoice::VideoToolbox {
        cmd.args(["-realtime", "1"]);
    }
    let child = cmd
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(EXIT_POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Refuses a recording path whose directory is missing or that exists.
fn check_record_path(config: &OutputConfig) -> Result<(), OutputError> {
    let Some(path) = &config.record_path else {
        return Ok(());
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && !dir.is_dir()
    {
        return Err(OutputError::InvalidConfig(format!(
            "record_path directory {dir:?} does not exist"
        )));
    }
    if path.try_exists()? {
        return Err(OutputError::InvalidConfig(format!(
            "record_path {path:?} already exists; refusing to overwrite"
        )));
    }
    Ok(())
}

/// Spawns a named thread.
fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, OutputError> {
    Ok(thread::Builder::new().name(name.into()).spawn(f)?)
}

impl ProgramEncoder {
    /// Starts ffmpeg for `config`. See the module docs for the sequence.
    pub fn start(config: OutputConfig) -> Result<Self, OutputError> {
        config.validate()?;
        check_record_path(&config)?;
        let encoder = detect_encoder(&config.ffmpeg, config.encoder)?;
        let listener = match config.audio {
            Some(_) => Some(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?),
            None => None,
        };
        let audio_url = match &listener {
            Some(l) => Some(format!("tcp://{}", l.local_addr()?)),
            None => None,
        };
        let args = build_ffmpeg_args(&config, &encoder, audio_url.as_deref());
        let mut child = Command::new(&config.ffmpeg)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| spawn_error(&config.ffmpeg, e))?;
        let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (video_tx, video_rx) = sync_channel(VIDEO_QUEUE_FRAMES);
        let (audio_tx, audio_rx) = sync_channel(AUDIO_QUEUE_BLOCKS);
        let mut enc = Self {
            pid: child.id(),
            child: Mutex::new(child),
            encoder,
            shared: Arc::new(Shared::default()),
            video_tx: Some(video_tx),
            audio_tx: listener.is_some().then_some(audio_tx),
            threads: Vec::new(),
            config,
        };
        // From here on, an early return drops `enc`, which kills ffmpeg.
        let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
            return Err(OutputError::Io(std::io::Error::other(
                "ffmpeg pipes missing",
            )));
        };
        let s = enc.shared.clone();
        enc.threads.push(spawn("ac-output-video", move || {
            transport::video_writer(video_rx, stdin, s)
        })?);
        if let Some(listener) = listener {
            let s = enc.shared.clone();
            enc.threads.push(spawn("ac-output-audio", move || {
                transport::audio_writer(audio_rx, listener, s, AUDIO_ACCEPT_TIMEOUT)
            })?);
        }
        let (s, path) = (enc.shared.clone(), enc.config.record_path.clone());
        enc.threads.push(spawn("ac-output-progress", move || {
            transport::progress_reader(stdout, s, path)
        })?);
        let (s, url) = (enc.shared.clone(), enc.config.rtmp_url.clone());
        enc.threads.push(spawn("ac-output-stderr", move || {
            transport::stderr_reader(stderr, s, url)
        })?);
        Ok(enc)
    }

    /// The configuration in use.
    pub fn config(&self) -> &OutputConfig {
        &self.config
    }

    /// The encoder in use.
    pub fn encoder(&self) -> EncoderChoice {
        self.encoder
    }

    /// ffmpeg's process id, for diagnostics.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Queues one RGBA frame of exactly `width * height * 4` bytes.
    ///
    /// Call once per program tick. Never blocks; see [`Push`].
    pub fn push_video(&self, rgba: &[u8]) -> Result<Push, OutputError> {
        let expected = self.config.frame_bytes();
        if rgba.len() != expected {
            return Err(OutputError::FrameSize {
                expected,
                actual: rgba.len(),
            });
        }
        let s = &self.shared;
        s.frames_in.fetch_add(1, Ordering::Relaxed);
        let tx = match &self.video_tx {
            Some(tx) if !s.video_closed.load(Ordering::SeqCst) => tx,
            _ => {
                s.frames_dropped.fetch_add(1, Ordering::Relaxed);
                return Err(OutputError::Stopped);
            }
        };
        // Cheap early drop: skip the copy when the queue is known full.
        if s.video_queued.load(Ordering::SeqCst) >= VIDEO_QUEUE_FRAMES {
            return Ok(self.drop_video());
        }
        let mut buf = s.take_buffer();
        buf.clear();
        buf.extend_from_slice(rgba);
        // Counted before sending so the writer's decrement cannot underflow.
        s.video_queued.fetch_add(1, Ordering::SeqCst);
        match tx.try_send(buf) {
            Ok(()) => Ok(Push::Queued),
            Err(TrySendError::Full(buf)) => {
                s.video_queued.fetch_sub(1, Ordering::SeqCst);
                s.recycle(buf);
                Ok(self.drop_video())
            }
            Err(TrySendError::Disconnected(_)) => {
                s.video_queued.fetch_sub(1, Ordering::SeqCst);
                s.frames_dropped.fetch_add(1, Ordering::Relaxed);
                Err(OutputError::Stopped)
            }
        }
    }

    /// Counts a dropped frame and owes a repeat for it.
    fn drop_video(&self) -> Push {
        let s = &self.shared;
        s.frames_dropped.fetch_add(1, Ordering::Relaxed);
        let cap = MAX_DEBT_SECONDS * u64::from(self.config.fps);
        transport::add_debt(&s.video_debt, 1, cap);
        Push::Dropped
    }

    /// Owes `n` repeats of the newest frame for program ticks the caller
    /// skipped (it fell far behind its clock), so the encoded timeline keeps
    /// pace with the caller's clock instead of falling behind it for good.
    ///
    /// The video writer emits the repeats (of the frame it wrote last) just
    /// before the next frame it takes from the queue, exactly like the
    /// repeats that repay dropped frames; they count
    /// in [`EncoderStats::frames_repeated`] and `frames_written`, not as
    /// drops. The debt is capped at `MAX_DEBT_SECONDS` of video (ten seconds
    /// at the configured rate), so a long suspension is not replayed in full.
    /// Returns how many repeats were owed: `n` unless the cap or a closed
    /// encoder limited it.
    pub fn repeat_frames(&self, n: u64) -> u64 {
        let s = &self.shared;
        if n == 0 || self.video_tx.is_none() || s.video_closed.load(Ordering::SeqCst) {
            return 0;
        }
        let cap = MAX_DEBT_SECONDS * u64::from(self.config.fps);
        let before = s
            .video_debt
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |d| {
                Some(d.saturating_add(n).min(cap))
            })
            .unwrap_or_else(|d| d);
        before.saturating_add(n).min(cap).saturating_sub(before)
    }

    /// Queues a composed program frame; it must match the output size.
    pub fn push_frame(&self, frame: &VideoFrame) -> Result<Push, OutputError> {
        let expected = (self.config.width, self.config.height);
        let actual = (frame.width, frame.height);
        if expected != actual {
            return Err(OutputError::FrameDimensions { expected, actual });
        }
        self.push_video(&frame.data)
    }

    /// Queues interleaved samples in the configured [`AudioFormat`].
    ///
    /// The length must be a whole number of sample frames. Never blocks.
    pub fn push_audio(&self, samples: &[f32]) -> Result<Push, OutputError> {
        let Some(format) = self.config.audio else {
            return Err(OutputError::AudioNotConfigured);
        };
        let channels = usize::from(format.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(OutputError::AudioLength {
                len: samples.len(),
                channels: format.channels,
            });
        }
        if samples.is_empty() {
            return Ok(Push::Queued);
        }
        let s = &self.shared;
        let n = samples.len() as u64;
        s.audio_samples_in.fetch_add(n, Ordering::Relaxed);
        let tx = match &self.audio_tx {
            Some(tx) if !s.audio_closed.load(Ordering::SeqCst) => tx,
            _ => {
                s.audio_samples_dropped.fetch_add(n, Ordering::Relaxed);
                return Err(OutputError::Stopped);
            }
        };
        match tx.try_send(samples.to_vec()) {
            Ok(()) => Ok(Push::Queued),
            Err(TrySendError::Full(_)) => {
                s.audio_samples_dropped.fetch_add(n, Ordering::Relaxed);
                let per_second = u64::from(format.sample_rate) * u64::from(format.channels);
                transport::add_debt(&s.audio_debt, n, MAX_DEBT_SECONDS * per_second);
                Ok(Push::Dropped)
            }
            Err(TrySendError::Disconnected(_)) => {
                s.audio_samples_dropped.fetch_add(n, Ordering::Relaxed);
                Err(OutputError::Stopped)
            }
        }
    }

    /// Queues an audio block; its rate and channels must match the config.
    pub fn push_audio_block(&self, block: &AudioBlock) -> Result<Push, OutputError> {
        let Some(expected) = self.config.audio else {
            return Err(OutputError::AudioNotConfigured);
        };
        let actual = AudioFormat {
            sample_rate: block.sample_rate,
            channels: block.channels,
        };
        if actual != expected {
            return Err(OutputError::AudioFormatMismatch { expected, actual });
        }
        self.push_audio(&block.samples)
    }

    /// Locks the child handle.
    fn lock_child(&self) -> MutexGuard<'_, Child> {
        self.child.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A snapshot of counters, ffmpeg's state and its newest progress report.
    pub fn stats(&self) -> EncoderStats {
        let (ffmpeg_alive, exit_code) = match self.lock_child().try_wait() {
            Ok(None) => (true, None),
            Ok(Some(status)) => (false, status.code()),
            Err(_) => (false, None),
        };
        let s = &self.shared;
        let get = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed);
        EncoderStats {
            encoder: self.encoder,
            frames_in: get(&s.frames_in),
            frames_dropped: get(&s.frames_dropped),
            frames_repeated: get(&s.frames_repeated),
            frames_written: get(&s.frames_written),
            audio_samples_in: get(&s.audio_samples_in),
            audio_samples_dropped: get(&s.audio_samples_dropped),
            audio_silence_samples: get(&s.audio_silence_samples),
            audio_samples_written: get(&s.audio_samples_written),
            audio_connected: s.audio_connected.load(Ordering::SeqCst),
            ffmpeg_alive,
            exit_code,
            last_error: s.last_error(),
            progress: s.progress(),
        }
    }

    /// Finishes with [`DEFAULT_FINISH_TIMEOUT`].
    pub fn finish(self) -> Result<EncoderStats, OutputError> {
        self.finish_with_timeout(DEFAULT_FINISH_TIMEOUT)
    }

    /// Closes the inputs, waits up to `timeout` for ffmpeg to flush both
    /// outputs and exit, then kills it. Returns the final stats on a clean
    /// exit, [`OutputError::Failed`] on a non-zero exit and
    /// [`OutputError::Timeout`] if ffmpeg had to be killed.
    ///
    /// A timeout does not by itself mean the recording is damaged: the MP4
    /// is finalised before the RTMP slave is closed, and closing an RTMP
    /// slave that is still trying to reach an unreachable host can outlast
    /// any reasonable timeout (it waits for the OS connect timeout).
    pub fn finish_with_timeout(mut self, timeout: Duration) -> Result<EncoderStats, OutputError> {
        self.video_tx = None;
        self.audio_tx = None;
        let status = self.wait_until(Instant::now() + timeout);
        // Teardown from here on: broken pipes are expected, not errors.
        self.shared.stop.store(true, Ordering::SeqCst);
        if !matches!(status, Ok(Some(_))) {
            self.kill_and_reap();
        }
        self.join_threads();
        let stats = self.stats();
        match status? {
            None => Err(OutputError::Timeout(timeout)),
            Some(status) if status.success() => Ok(stats),
            Some(status) => Err(OutputError::Failed {
                status: status.to_string(),
                last_error: stats.last_error,
            }),
        }
    }

    /// Polls for ffmpeg's exit until `deadline`; `None` if still running.
    fn wait_until(&self, deadline: Instant) -> std::io::Result<Option<ExitStatus>> {
        loop {
            if let Some(status) = self.lock_child().try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(EXIT_POLL);
        }
    }

    /// Kills ffmpeg if it is still running and reaps it.
    fn kill_and_reap(&self) {
        let mut child = self.lock_child();
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
        }
        let _ = child.wait();
    }

    /// Joins every thread, recording a panic as an error.
    fn join_threads(&mut self) {
        for t in self.threads.drain(..) {
            if t.join().is_err() {
                self.shared.record_error("an ac-output thread panicked");
            }
        }
    }
}

impl Drop for ProgramEncoder {
    /// Kills ffmpeg (if still running), reaps it and joins the threads.
    /// Idempotent after [`ProgramEncoder::finish`].
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Closing the queues lets idle writers exit; killing ffmpeg breaks
        // the pipes of busy ones and ends the readers.
        self.video_tx = None;
        self.audio_tx = None;
        self.kill_and_reap();
        self.join_threads();
    }
}

#[cfg(test)]
mod tests;
