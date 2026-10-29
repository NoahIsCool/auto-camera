//! An audio-only file played as a live feed through the ffmpeg command
//! line: the rehearsal stand-in for a soundboard's feed.
//!
//! Opening probes the file's first audio stream with ffprobe
//! ([`crate::probe::probe_audio`]) and starts one ffmpeg child that decodes
//! it to interleaved `f32` at the stream's own rate and channel layout,
//! with no option that could resample or remix it, so a PCM file's samples
//! arrive exactly (16- and 24-bit integers convert to `f32` without loss).
//! One worker thread reads the child's output:
//!
//! * it cuts it into blocks of about [`crate::BLOCK_MILLIS`] and stamps
//!   each from a running sample count ([`crate::audio`]'s `SampleClock`): a
//!   sequence number per block and an exact PTS, block `n + 1` starting at
//!   the sample where block `n` ended;
//! * it releases each block when its *last* sample is due on the clock, as
//!   a capture device delivers a buffer once it is full, into an
//!   [`AudioRing`] that drops its oldest block when nobody drains it. The
//!   first block moves the input from `Starting` to `Running`.
//!
//! Looping restarts the decoder at end of file; the decoder for the next
//! pass was started in advance, so the restart has no gap. The sample count
//! goes on, so PTS stays gap-free and strictly increasing however long the
//! file loops, and the sequence numbers continue.
//!
//! A decoder's output ending is end of input only if the decoder then exits
//! successfully; one that was killed or crashed lost the input, and the
//! input stops with that error, so a supervisor opens it again
//! ([`crate::audio_input::supervised`]). A pass that decodes no audio at all
//! is an error too: an empty file would otherwise loop without a sound.
//!
//! Shutdown: [`AudioInput::stop`] raises the stop flag, kills every child
//! (which unblocks the worker's read), joins the worker and reaps the
//! children; dropping the input stops it. The worker kills its decoder
//! before it drops the decoder's stdout, so a stop leaves no "Broken pipe"
//! complaint in the log.

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_core::SourceId;
use ac_media::{AudioBlock, AudioInput, AudioInputInfo, AudioRing, MediaError, SourceState};

use crate::audio::{SampleClock, decode_f32le};
use crate::clock::{Clock, Timeline, WallClock};
use crate::ffmpeg::{audio_only_args, command};
use crate::file::DEFAULT_AUDIO_QUEUE_BLOCKS;
use crate::probe::{AudioStreamInfo, FFMPEG_ENV, FfmpegTools, probe_audio};
use crate::process::{Children, END_GRACE, Respawner, Spawned, read_full};
use crate::status::Status;

/// How an audio-only file is played.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFileOptions {
    /// Restart at end of file instead of stopping.
    pub loop_playback: bool,
    /// Most blocks queued between drains (at least one); older blocks are
    /// dropped.
    pub queue_blocks: usize,
    /// The ffmpeg executables.
    pub tools: FfmpegTools,
}

impl Default for AudioFileOptions {
    fn default() -> Self {
        Self {
            loop_playback: false,
            queue_blocks: DEFAULT_AUDIO_QUEUE_BLOCKS,
            tools: FfmpegTools::from_env(),
        }
    }
}

/// State shared by an [`AudioFileInput`] and its worker.
struct Shared {
    /// Raised once by `stop`; the worker exits when it sees it.
    stop: AtomicBool,
    /// Health and the stopping error.
    status: Status,
    /// Pacing.
    timeline: Timeline,
    /// Blocks awaiting `drain`.
    ring: AudioRing,
    /// Every running ffmpeg child.
    children: Children,
}

impl Shared {
    /// True once a stop was requested.
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Asks the worker to exit and unblocks it.
    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.children.close();
    }
}

/// An audio-only file played as a live input; see the module docs.
pub struct AudioFileInput {
    /// What it delivers.
    info: AudioInputInfo,
    /// The stream ffprobe found.
    stream: AudioStreamInfo,
    /// State shared with the worker.
    shared: Arc<Shared>,
    /// The worker, joined by `stop`.
    worker: Option<JoinHandle<()>>,
}

impl fmt::Debug for AudioFileInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioFileInput")
            .field("info", &self.info)
            .field("state", &self.shared.status.get())
            .finish_non_exhaustive()
    }
}

impl AudioFileInput {
    /// Opens `path` and starts playing it in real time.
    pub fn open(id: SourceId, path: &Path, options: AudioFileOptions) -> Result<Self, MediaError> {
        Self::open_with_clock(id, path, options, Arc::new(WallClock::new()))
    }

    /// Opens `path` and starts playing it paced by `clock`; with a
    /// [`crate::VirtualClock`] it plays as fast as ffmpeg decodes.
    pub fn open_with_clock(
        id: SourceId,
        path: &Path,
        options: AudioFileOptions,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, MediaError> {
        let path = path
            .canonicalize()
            .map_err(|e| MediaError::Open(format!("cannot open {}: {e}", path.display())))?;
        let stream = probe_audio(&options.tools, &path)?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            status: Status::new(),
            timeline: Timeline::new(clock),
            ring: AudioRing::new(options.queue_blocks),
            children: Children::new(),
        });
        let tools = options.tools.clone();
        let args = audio_only_args(&path, &stream);
        let first = shared
            .children
            .spawn(&mut command(&tools, &args))
            .map_err(|e| {
                MediaError::Open(format!(
                    "cannot start {} (install ffmpeg or set {FFMPEG_ENV}): {e}",
                    tools.ffmpeg.display()
                ))
            })?
            .ok_or_else(|| MediaError::Open("input stopped while opening".into()))?;
        let decoder = Respawner::new(first, options.loop_playback, move || command(&tools, &args));
        let worker = Worker {
            source: id.clone(),
            clock: SampleClock::new(stream.sample_rate, stream.channels),
            channels: usize::from(stream.channels),
            looping: options.loop_playback,
            decoder,
        };
        let state = shared.clone();
        let handle = std::thread::Builder::new()
            .name(format!("ac-audio-in-{}", id.0))
            .spawn(move || worker.run(&state));
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                shared.request_stop();
                return Err(e.into());
            }
        };
        Ok(Self {
            info: AudioInputInfo {
                id,
                format: Some((stream.sample_rate, stream.channels)),
            },
            stream,
            shared,
            worker: Some(handle),
        })
    }

    /// The stream ffprobe found.
    pub fn stream(&self) -> &AudioStreamInfo {
        &self.stream
    }

    /// Process ids of the running ffmpeg children (empty once stopped).
    pub fn child_pids(&self) -> Vec<u32> {
        self.shared.children.pids()
    }
}

impl AudioInput for AudioFileInput {
    fn info(&self) -> &AudioInputInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        self.shared.status.get()
    }

    fn drain(&self) -> Vec<AudioBlock> {
        self.shared.ring.drain()
    }

    fn wait(&self, timeout: Duration) -> bool {
        self.shared.ring.wait(timeout)
    }

    fn dropped(&self) -> u64 {
        self.shared.ring.dropped()
    }

    fn error(&self) -> Option<String> {
        self.shared.status.error()
    }

    fn stop(&mut self) {
        self.shared.request_stop();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.shared.status.fail("the audio decoder worker panicked");
        }
        self.shared.children.close();
        self.shared.status.advance(SourceState::Stopped);
    }
}

impl Drop for AudioFileInput {
    fn drop(&mut self) {
        self.stop();
    }
}

/// How one pass over the file ended.
enum Pass {
    /// The decoder's output ended and it exited successfully (or was gone
    /// already) after `frames` sample frames.
    Ended {
        /// Sample frames emitted in the pass.
        frames: u64,
    },
    /// A stop was requested.
    Stopped,
    /// The input was lost; the message says how.
    Failed(String),
}

/// Decodes the file for the lifetime of an input.
struct Worker {
    /// Input the blocks are tagged with.
    source: SourceId,
    /// Position on the input's timeline.
    clock: SampleClock,
    /// Interleaved channels of the decoder output.
    channels: usize,
    /// Whether the file loops.
    looping: bool,
    /// Starts the decoder for each pass.
    decoder: Respawner,
}

impl Worker {
    /// Runs until end of input (without looping), an error or a stop
    /// request.
    fn run(mut self, shared: &Shared) {
        let mut bytes = vec![0u8; self.clock.block_bytes()];
        loop {
            let decoder = match self.decoder.start(&shared.children) {
                Ok(Some(d)) => d,
                Ok(None) => break,
                Err(e) => {
                    shared.status.fail(format!("cannot restart ffmpeg: {e}"));
                    break;
                }
            };
            match self.play_pass(shared, decoder, &mut bytes) {
                Pass::Ended { frames } if frames > 0 && self.looping => {}
                Pass::Ended { .. } | Pass::Stopped => break,
                Pass::Failed(message) => {
                    shared.status.fail(message);
                    break;
                }
            }
        }
        self.decoder.discard(&shared.children);
        shared.status.advance(SourceState::Stopped);
    }

    /// Plays one pass from `decoder`.
    fn play_pass(&mut self, shared: &Shared, decoder: Spawned, bytes: &mut [u8]) -> Pass {
        let Spawned { key, mut stdout } = decoder;
        let frame_bytes = self.channels * 4;
        let mut frames_out = 0u64;
        // Why the pass ended early, when it did: a stop, or an error.
        let mut early = None;
        while !shared.stopping() {
            let n = match read_full(&mut stdout, bytes) {
                Ok(n) => n,
                Err(e) => {
                    early = Some(Pass::Failed(format!("reading decoded audio: {e}")));
                    break;
                }
            };
            let frames = n / frame_bytes;
            if frames > 0 {
                if !self.emit(shared, decode_f32le(&bytes[..frames * frame_bytes])) {
                    break;
                }
                frames_out += frames as u64;
            }
            if n < bytes.len() {
                break;
            }
        }
        let early = early.or_else(|| shared.stopping().then_some(Pass::Stopped));
        // A decoder cut off is killed; one whose output ended is waited for,
        // for its exit status: briefly when it produced audio (it exits at
        // once), longer when it produced none. Either way it is gone before
        // its pipe closes (see `crate::process`).
        let exit = if early.is_some() {
            shared.children.kill(key);
            None
        } else if frames_out > 0 {
            shared.children.finish_within(key, END_GRACE)
        } else {
            shared.children.finish(key)
        };
        drop(stdout);
        if let Some(pass) = early {
            return pass;
        }
        if shared.stopping() {
            return Pass::Stopped;
        }
        if frames_out == 0 {
            let exit = exit.map_or("killed".to_string(), |s| s.to_string());
            return Pass::Failed(format!("ffmpeg decoded no audio ({exit})"));
        }
        match exit.filter(|s| !s.success()) {
            Some(status) => Pass::Failed(format!(
                "the audio decoder exited during playback ({status})"
            )),
            None => Pass::Ended { frames: frames_out },
        }
    }

    /// Waits until the block of `samples` is due, then queues it. Returns
    /// `false` when stopped.
    fn emit(&mut self, shared: &Shared, samples: Arc<[f32]>) -> bool {
        let frames = samples.len() / self.channels;
        if !shared
            .timeline
            .wait_until(self.clock.end_after(frames), &shared.stop)
        {
            return false;
        }
        shared.ring.push(self.clock.block(&self.source, samples));
        shared.status.advance(SourceState::Running);
        true
    }
}
