//! A video source that plays a local file through the ffmpeg command line.
//!
//! Opening a file probes it with ffprobe, then starts one ffmpeg child for
//! video (raw RGBA frames) and, when wanted and present, one for audio (raw
//! `f32` samples). Each child is read by its own worker thread:
//!
//! * The **video worker** reads whole frames, stamps them with `seq` (counting
//!   every frame delivered, across loops) and `pts = seq / fps`, waits until
//!   each is due on the source's timeline and publishes it into the
//!   [`LatestFrame`] mailbox. The first published frame moves the source from
//!   `Starting` to `Running`; end of input (without looping), an error or
//!   [`VideoSource::stop`] moves it to `Stopped`.
//! * The **audio worker** turns samples into ~10 ms [`AudioBlock`]s on the same
//!   timeline, never ahead of the video already published, and queues them for
//!   [`VideoSource::drain_audio`] (see [`crate::audio`]). When a file's audio
//!   outlasts its video, the tail can still be drained after the state turns
//!   `Stopped`.
//!
//! Looping restarts the decoders at end of file. PTS keeps counting from where
//! the previous pass ended, so it increases monotonically across loops, and the
//! next pass's decoder is started in advance so the restart has no gap. Each
//! pass's audio is trimmed or padded to the video's pass, so A/V stay locked
//! however long the file loops.
//!
//! Shutdown: [`VideoSource::stop`] raises the stop flag, kills every child (which
//! unblocks any worker stuck reading a pipe), joins the workers and reaps the
//! children. Dropping a source stops it, so no ffmpeg process outlives its
//! source.

use std::ffi::OsString;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use ac_core::SourceId;
use ac_media::{
    AudioBlock, LatestFrame, MediaError, SourceInfo, SourceState, VideoFrame, VideoSource,
};

use crate::audio::{AudioQueue, AudioWorker, SampleClock};
use crate::clock::{Clock, Timeline, WallClock};
use crate::ffmpeg::{audio_args, command, output_size, video_args};
use crate::frame_rate::FrameRate;
use crate::pool::FramePool;
use crate::probe::{FFMPEG_ENV, FfmpegTools, ProbeInfo, probe};
use crate::process::{Children, Respawner, Spawned, read_full};
use crate::status::{CloseOnDrop, Status, VideoProgress};

/// Default capacity of the audio queue: 256 blocks, about 2.5 s.
pub const DEFAULT_AUDIO_QUEUE_BLOCKS: usize = 256;

/// How a file is played.
#[derive(Debug, Clone, PartialEq)]
pub struct FileOptions {
    /// Restart at end of file instead of stopping.
    pub loop_playback: bool,
    /// Scale frames down to at most this width (aspect kept, sides even).
    pub max_width: Option<u32>,
    /// Decode the file's first audio stream, if it has one.
    pub audio: bool,
    /// Most audio blocks queued between drains (at least one); older blocks
    /// are dropped.
    pub audio_queue_blocks: usize,
    /// The ffmpeg executables.
    pub tools: FfmpegTools,
}

impl Default for FileOptions {
    fn default() -> Self {
        Self {
            loop_playback: false,
            max_width: None,
            audio: true,
            audio_queue_blocks: DEFAULT_AUDIO_QUEUE_BLOCKS,
            tools: FfmpegTools::from_env(),
        }
    }
}

/// State shared by a [`FileSource`] and its workers.
pub(crate) struct Shared {
    /// Raised once by `stop`; workers exit when they see it.
    pub(crate) stop: AtomicBool,
    /// Health and the stopping error.
    pub(crate) status: Status,
    /// Pacing for both workers.
    pub(crate) timeline: Timeline,
    /// The newest frame.
    pub(crate) latest: LatestFrame,
    /// Decoded audio awaiting `drain_audio`.
    pub(crate) audio: AudioQueue,
    /// Every running ffmpeg child.
    pub(crate) children: Children,
    /// How far the video worker has got, for the audio worker.
    pub(crate) progress: VideoProgress,
}

impl Shared {
    /// True once a stop was requested.
    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Asks every worker to exit and unblocks them.
    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.progress.close();
        self.children.close();
    }
}

/// A local video file played as a live source; see [`crate::file`].
pub struct FileSource {
    /// Static description.
    info: SourceInfo,
    /// What ffprobe found in the file.
    probe: ProbeInfo,
    /// State shared with the workers.
    shared: Arc<Shared>,
    /// Video and audio workers, joined by `stop`.
    workers: Vec<JoinHandle<()>>,
}

impl fmt::Debug for FileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSource")
            .field("info", &self.info)
            .field("state", &self.shared.status.get())
            .finish_non_exhaustive()
    }
}

impl FileSource {
    /// Opens `path` and starts playing it in real time.
    pub fn open(id: SourceId, path: &Path, options: FileOptions) -> Result<Self, MediaError> {
        Self::open_with_clock(id, path, options, Arc::new(WallClock::new()))
    }

    /// Opens `path` and starts playing it paced by `clock`; with a
    /// [`crate::VirtualClock`] it plays as fast as ffmpeg decodes.
    pub fn open_with_clock(
        id: SourceId,
        path: &Path,
        options: FileOptions,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, MediaError> {
        let path = path
            .canonicalize()
            .map_err(|e| MediaError::Open(format!("cannot open {}: {e}", path.display())))?;
        let probe = probe(&options.tools, &path)?;
        let (width, height) = output_size(probe.video.display_size(), options.max_width)?;
        let rate = probe.video.frame_rate;
        let audio = probe.audio.filter(|_| options.audio);
        let info = SourceInfo {
            id: id.clone(),
            width,
            height,
            fps: rate.as_f64(),
            audio: audio.map(|a| (a.sample_rate, a.channels)),
        };
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            status: Status::new(),
            timeline: Timeline::new(clock),
            latest: LatestFrame::new(),
            audio: AudioQueue::new(options.audio_queue_blocks),
            children: Children::new(),
            progress: VideoProgress::new(),
        });
        let mut source = Self {
            info,
            probe: probe.clone(),
            shared,
            workers: Vec::new(),
        };
        let looping = options.loop_playback;

        let args = video_args(&path, &probe.video, (width, height), rate);
        let decoder = source.respawner(&options.tools, args, looping, "video")?;
        let video = VideoWorker {
            source: id.clone(),
            size: (width, height),
            rate,
            looping,
            decoder,
        };
        source.spawn_worker(format!("ac-video-{}", id.0), move |s| video.run(s))?;

        if let Some(stream) = audio {
            let args = audio_args(&path, &stream);
            let decoder = source.respawner(&options.tools, args, looping, "audio")?;
            let worker = AudioWorker {
                source: id.clone(),
                clock: SampleClock::new(stream.sample_rate, stream.channels),
                looping,
                decoder,
            };
            source.spawn_worker(format!("ac-audio-{}", id.0), move |s| worker.run(s))?;
        }
        Ok(source)
    }

    /// Starts the first decoder for `args` and wraps it for later passes.
    fn respawner(
        &self,
        tools: &FfmpegTools,
        args: Vec<OsString>,
        looping: bool,
        what: &str,
    ) -> Result<Respawner, MediaError> {
        let tools = tools.clone();
        let first: Option<Spawned> = self
            .shared
            .children
            .spawn(&mut command(&tools, &args))
            .map_err(|e| {
                MediaError::Open(format!(
                    "cannot start {} for {what} (install ffmpeg or set {}): {e}",
                    tools.ffmpeg.display(),
                    FFMPEG_ENV
                ))
            })?;
        let first = first.ok_or_else(|| MediaError::Open("source stopped while opening".into()))?;
        Ok(Respawner::new(first, looping, move || {
            command(&tools, &args)
        }))
    }

    /// Runs `work` on a named thread; it is joined by `stop`.
    fn spawn_worker(
        &mut self,
        name: String,
        work: impl FnOnce(&Shared) + Send + 'static,
    ) -> Result<(), MediaError> {
        let shared = self.shared.clone();
        let handle = std::thread::Builder::new()
            .name(name)
            .spawn(move || work(&shared))?;
        self.workers.push(handle);
        Ok(())
    }

    /// What ffprobe found in the file.
    pub fn probe_info(&self) -> &ProbeInfo {
        &self.probe
    }

    /// Audio blocks dropped because `drain_audio` was not called often enough.
    pub fn audio_drops(&self) -> u64 {
        self.shared.audio.dropped()
    }

    /// The error that stopped the source, if it stopped because of one.
    pub fn error(&self) -> Option<String> {
        self.shared.status.error()
    }

    /// Process ids of the running ffmpeg children (empty once stopped).
    pub fn child_pids(&self) -> Vec<u32> {
        self.shared.children.pids()
    }
}

impl VideoSource for FileSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        self.shared.status.get()
    }

    fn latest(&self) -> Option<VideoFrame> {
        self.shared.latest.get()
    }

    fn drain_audio(&self) -> Vec<AudioBlock> {
        self.shared.audio.drain()
    }

    fn stop(&mut self) {
        self.shared.request_stop();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                self.shared.status.fail("a decoder worker panicked");
            }
        }
        self.shared.children.close();
        self.shared.status.advance(SourceState::Stopped);
    }
}

impl Drop for FileSource {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Decodes a file's video for the lifetime of a source.
struct VideoWorker {
    /// Source the frames are tagged with.
    source: SourceId,
    /// Decoded frame size.
    size: (u32, u32),
    /// Decoded frame rate; defines PTS.
    rate: FrameRate,
    /// Whether the file loops.
    looping: bool,
    /// Starts the decoder for each pass.
    decoder: Respawner,
}

impl VideoWorker {
    /// Runs until end of input (without looping), an error or a stop request.
    fn run(mut self, shared: &Shared) {
        let _close = CloseOnDrop(&shared.progress);
        let frame_len = self.size.0 as usize * self.size.1 as usize * 4;
        let mut pool = FramePool::new(frame_len);
        let mut seq = 0u64;
        let mut pass = 0;
        loop {
            let Spawned { key, mut stdout } = match self.decoder.start(&shared.children) {
                Ok(Some(d)) => d,
                Ok(None) => break,
                Err(e) => {
                    shared.status.fail(format!("cannot restart ffmpeg: {e}"));
                    break;
                }
            };
            let first = seq;
            let mut failure = None;
            while !shared.stopping() {
                let (data, got) = pool.fill(|buf| read_full(&mut stdout, buf));
                match got {
                    Ok(n) if n == frame_len => {}
                    Ok(_) => break,
                    Err(e) => {
                        failure = Some(format!("reading decoded video: {e}"));
                        break;
                    }
                }
                let pts = self.rate.pts(seq);
                if !shared.timeline.wait_until(pts, &shared.stop) {
                    break;
                }
                let (w, h) = self.size;
                match VideoFrame::new(self.source.clone(), seq, pts, w, h, data.clone()) {
                    Ok(frame) => shared.latest.publish(frame),
                    Err(e) => {
                        failure = Some(e.to_string());
                        break;
                    }
                }
                pool.recycle(data);
                shared.status.advance(SourceState::Running);
                seq += 1;
                shared.progress.frame_published(pass, self.rate.pts(seq));
            }
            drop(stdout);
            let produced = seq > first;
            if self.looping && produced && failure.is_none() && !shared.stopping() {
                // Announce the loop point before reaping the old decoder: the
                // audio worker may be waiting on it.
                shared.progress.pass_ended(self.rate.pts(seq));
                pass += 1;
            }
            // A decoder that produced frames is done, and killing it is
            // immediate; one that produced none is waited for, for its status.
            let exit = if produced {
                shared.children.kill(key);
                None
            } else {
                shared.children.finish(key)
            };
            if shared.stopping() {
                break;
            }
            if let Some(message) = failure {
                shared.status.fail(message);
                break;
            }
            if !produced {
                let exit = exit.map_or("killed".to_string(), |s| s.to_string());
                shared
                    .status
                    .fail(format!("ffmpeg decoded no frames ({exit})"));
                break;
            }
            if !self.looping {
                break;
            }
        }
        self.decoder.discard(&shared.children);
        shared.status.advance(SourceState::Stopped);
    }
}
