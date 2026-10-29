//! The program encoder: one ffmpeg child and its lifecycle.
//!
//! # Lifecycle
//!
//! [`ProgramEncoder::start`] validates the config, refuses to overwrite an
//! existing recording, resolves the H.264 encoder from `ffmpeg -encoders`,
//! asks `ffmpeg -h long` whether it can write mux statistics (when the
//! encode streams; [`mux_stats_supported`]), binds the loopback audio
//! listener (when audio is configured), spawns ffmpeg and starts four
//! threads (video writer, audio writer, progress reader, stderr reader,
//! which owns the stream monitor). The handle is assembled before the
//! threads start, so a failure at any later step drops it and kills ffmpeg.
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
//!
//! This module holds the handle, its start (with the encoder probe) and its
//! stats; the non-blocking submission (`push_*`, `repeat_frames`) is in the
//! private `push` module and finishing and teardown (`finish`, `Drop`) in
//! `finish`.

use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::OutputError;
use crate::args::{build_ffmpeg_args, rtmp_slave_index};
use crate::config::{Encoder, EncoderChoice, OutputConfig, choose_working_encoder};
use crate::progress::Progress;
use crate::stream::{StreamHealth, StreamMonitor, settle_time};
use crate::transport::{
    self, AUDIO_ACCEPT_TIMEOUT, AUDIO_QUEUE_BLOCKS, Shared, VIDEO_QUEUE_FRAMES,
};

mod finish;
mod push;

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
    /// Newest error line from ffmpeg or a pipe, stream key redacted, as
    /// `context: message`. When the stream reconnects, an error of the
    /// stream is cleared (an older error of anything else shows again);
    /// other errors stay, so read them together with the counters.
    pub last_error: Option<String>,
    /// Newest `-progress` report: encoded frames, fps, bitrate, speed.
    pub progress: Progress,
    /// The stream's connection and bitrate, when the encode streams (see
    /// the stream module); `Down` once ffmpeg has exited.
    pub stream: Option<StreamHealth>,
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

/// True when `ffmpeg` can write per-packet mux statistics
/// (`-stats_mux_pre`, ffmpeg 6.0 and newer), from which the stream's
/// bitrate is measured: its `-h long` listing names the option.
pub fn mux_stats_supported(ffmpeg: &Path) -> bool {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-h", "long"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| lists_option(&String::from_utf8_lossy(&out.stdout), "-stats_mux_pre"))
}

/// True when an ffmpeg help listing has a line for `option`, such as
/// `-stats_mux_pre      write packets stats before muxing` (ffmpeg 6) or
/// `-stats_mux_pre[:<stream_spec>]  write packets stats before muxing`
/// (7.0 and newer, which append what the option can be specified for).
fn lists_option(help: &str, option: &str) -> bool {
    help.lines()
        .filter_map(|line| line.split_whitespace().next())
        .any(|name| name.split('[').next() == Some(option))
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
        let streaming = config.rtmp_url.is_some();
        let mux_stats = streaming && mux_stats_supported(&config.ffmpeg);
        let args = build_ffmpeg_args(&config, &encoder, audio_url.as_deref(), mux_stats);
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
        let monitor = streaming.then(|| {
            let settle = settle_time(enc.config.fps, enc.config.audio.is_some());
            StreamMonitor::new(settle, rtmp_slave_index(&enc.config))
        });
        if let Some(m) = monitor {
            enc.shared.publish_stream(m);
        }
        let (s, url) = (enc.shared.clone(), enc.config.rtmp_url.clone());
        enc.threads.push(spawn("ac-output-stderr", move || {
            transport::stderr_reader(stderr, s, url, monitor)
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

    /// Locks the child handle.
    fn lock_child(&self) -> MutexGuard<'_, Child> {
        self.child.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A snapshot of counters, ffmpeg's state, its newest progress report
    /// and the stream's health at that report's media time.
    pub fn stats(&self) -> EncoderStats {
        let (ffmpeg_alive, exit_code) = match self.lock_child().try_wait() {
            Ok(None) => (true, None),
            Ok(Some(status)) => (false, status.code()),
            Err(_) => (false, None),
        };
        let s = &self.shared;
        let get = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed);
        let progress = s.progress();
        let stream = s
            .stream()
            .map(|m| m.health(progress.out_time, ffmpeg_alive, s.muxed_kbps()));
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
            progress,
            stream,
        }
    }
}

#[cfg(test)]
mod tests;
