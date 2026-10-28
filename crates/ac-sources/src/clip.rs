//! A file decoded frame by frame on demand, for offline tools.
//!
//! [`FileSource`](crate::FileSource) plays a file as a live camera would:
//! its workers pace the frames on a clock and publish only the newest, so a
//! reader that is slower or faster than that clock sees frames dropped or
//! repeated. That is right for a studio and wrong for an offline tool that
//! steps the engine on simulated time (the eval runner), which needs every
//! frame, in order, when it asks, and the same frames on every run.
//! [`ClipReader`] is that tool's source: one ffmpeg child decodes the video
//! with a file source's command line (size, colour matrix, constant rate,
//! [`Segment`]), and [`ClipReader::next_frame`] reads exactly one frame
//! from its pipe, waiting until ffmpeg has decoded it. There is no thread
//! and no clock; ffmpeg runs ahead of the reader by one pipe buffer at most.
//!
//! Frames are numbered as a file source numbers them: sequence numbers from
//! 0 and `pts = seq / fps` at the probed rate, starting with the frame at
//! the segment's start ([`crate::segment`]). The same file therefore gives
//! the same picture for the same sequence number and PTS here and in a live
//! file source, so a detection log recorded on one replays on the other. A
//! looping clip restarts its decoder at the end and keeps counting, as a
//! looping source does. Audio is not decoded.
//!
//! The clip ends (`Ok(None)`) when the decoder's output ends and it exits
//! successfully. A decoder that exits with a failure, or that decodes no
//! frame in a pass, is an error. A partial frame at the very end is
//! dropped. Dropping the reader kills and reaps its decoder, so no ffmpeg
//! outlives it.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::Path;

use ac_core::SourceId;
use ac_media::{MediaError, SourceInfo, VideoFrame};

use crate::config::SourceConfig;
use crate::ffmpeg::{command, output_size, video_args};
use crate::frame_rate::FrameRate;
use crate::probe::{FFMPEG_ENV, FfmpegTools, probe};
use crate::process::{Children, Spawned, read_full};
use crate::segment::Segment;

/// How a clip is read.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipOptions {
    /// Scale frames down to at most this width (aspect kept, sides even).
    pub max_width: Option<u32>,
    /// The part of the file to read (default: all of it).
    pub segment: Segment,
    /// Start again at the end instead of ending.
    pub loop_playback: bool,
    /// The ffmpeg executables.
    pub tools: FfmpegTools,
}

impl Default for ClipOptions {
    fn default() -> Self {
        Self {
            max_width: None,
            segment: Segment::WHOLE,
            loop_playback: false,
            tools: FfmpegTools::from_env(),
        }
    }
}

/// A file read one frame at a time; see the module docs.
pub struct ClipReader {
    /// Static description; `audio` is always `None`.
    info: SourceInfo,
    /// Decoded frame rate; defines PTS.
    rate: FrameRate,
    /// Bytes per decoded frame.
    frame_len: usize,
    /// The ffmpeg executables.
    tools: FfmpegTools,
    /// The decoder's arguments, for each pass.
    args: Vec<OsString>,
    /// Whether to start a new pass at the end.
    looping: bool,
    /// Every running decoder; closed on drop.
    children: Children,
    /// The decoder of the current pass; `None` once the clip has ended.
    decoder: Option<Spawned>,
    /// Sequence number of the next frame.
    seq: u64,
    /// Sequence number of the current pass's first frame.
    pass_start: u64,
}

impl fmt::Debug for ClipReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipReader")
            .field("info", &self.info)
            .field("seq", &self.seq)
            .field("ended", &self.decoder.is_none())
            .finish_non_exhaustive()
    }
}

impl ClipReader {
    /// Probes `path` and starts decoding it as source `id`.
    pub fn open(id: SourceId, path: &Path, options: &ClipOptions) -> Result<Self, MediaError> {
        let path = path
            .canonicalize()
            .map_err(|e| MediaError::Open(format!("cannot open {}: {e}", path.display())))?;
        let probe = probe(&options.tools, &path)?;
        let (width, height) = output_size(probe.video.display_size(), options.max_width)?;
        let rate = probe.video.frame_rate;
        let args = options
            .segment
            .apply(video_args(&path, &probe.video, (width, height), rate));
        let mut reader = Self {
            info: SourceInfo {
                id,
                width,
                height,
                fps: rate.as_f64(),
                audio: None,
            },
            rate,
            frame_len: width as usize * height as usize * 4,
            tools: options.tools.clone(),
            args,
            looping: options.loop_playback,
            children: Children::new(),
            decoder: None,
            seq: 0,
            pass_start: 0,
        };
        reader.decoder = Some(reader.spawn()?);
        Ok(reader)
    }

    /// Opens the file of a file source's `config` as source `id`, with its
    /// size limit, segment and looping; any other kind of source is an
    /// error.
    pub fn from_config(id: SourceId, config: &SourceConfig) -> Result<Self, MediaError> {
        let SourceConfig::File {
            path,
            loop_playback,
            max_width,
            start_s,
            duration_s,
            ..
        } = config
        else {
            return Err(MediaError::Open(
                "only a file source is read as a clip".into(),
            ));
        };
        let options = ClipOptions {
            max_width: *max_width,
            segment: Segment::from_seconds(*start_s, *duration_s)?,
            loop_playback: *loop_playback,
            ..ClipOptions::default()
        };
        Self::open(id, path, &options)
    }

    /// Static description; a clip has no audio.
    pub fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// The decoded frame rate, which defines PTS.
    pub fn frame_rate(&self) -> FrameRate {
        self.rate
    }

    /// The next frame, `Ok(None)` once the clip has ended; see the module
    /// docs. Waits until ffmpeg has decoded it.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>, MediaError> {
        loop {
            let Some(decoder) = &mut self.decoder else {
                return Ok(None);
            };
            let mut data = vec![0u8; self.frame_len];
            let n = read_full(&mut decoder.stdout, &mut data)?;
            if n == self.frame_len {
                let pts = self.rate.pts(self.seq);
                let (w, h) = (self.info.width, self.info.height);
                let frame =
                    VideoFrame::new(self.info.id.clone(), self.seq, pts, w, h, data.into())?;
                self.seq += 1;
                return Ok(Some(frame));
            }
            let key = decoder.key;
            self.decoder = None;
            let exit = self.children.finish(key);
            if self.seq == self.pass_start {
                let exit = exit.map_or("killed".to_string(), |s| s.to_string());
                return Err(io::Error::other(format!("ffmpeg decoded no frames ({exit})")).into());
            }
            if let Some(status) = exit.filter(|s| !s.success()) {
                return Err(
                    io::Error::other(format!("the video decoder failed ({status})")).into(),
                );
            }
            if !self.looping {
                return Ok(None);
            }
            self.pass_start = self.seq;
            self.decoder = Some(self.spawn()?);
        }
    }

    /// Starts a decoder for one pass over the clip.
    fn spawn(&self) -> Result<Spawned, MediaError> {
        let spawned = self
            .children
            .spawn(&mut command(&self.tools, &self.args))
            .map_err(|e| {
                MediaError::Open(format!(
                    "cannot start {} (install ffmpeg or set {FFMPEG_ENV}): {e}",
                    self.tools.ffmpeg.display()
                ))
            })?;
        // The registry is closed only when the reader is dropped.
        spawned.ok_or_else(|| MediaError::Open("the clip reader is closed".into()))
    }

    /// Process ids of the running ffmpeg children (none once ended).
    pub fn child_pids(&self) -> Vec<u32> {
        self.children.pids()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_file_source_is_read_as_a_clip() {
        let synthetic = SourceConfig::Synthetic {
            width: 64,
            height: 36,
            fps: 30.0,
            subjects: 1,
        };
        let err = ClipReader::from_config(SourceId("s".into()), &synthetic).unwrap_err();
        assert!(err.to_string().contains("only a file source"), "{err}");
    }

    #[test]
    fn a_missing_file_or_a_bad_segment_is_an_open_error() {
        let file = |start_s| SourceConfig::File {
            path: "/nonexistent/auto-camera/clip.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: true,
            start_s,
            duration_s: None,
        };
        let err = ClipReader::from_config(SourceId("f".into()), &file(None)).unwrap_err();
        assert!(err.to_string().contains("clip.mp4"), "{err}");
        let err = ClipReader::from_config(SourceId("f".into()), &file(Some(-1.0))).unwrap_err();
        assert!(err.to_string().contains("start_s"), "{err}");
    }

    #[test]
    fn options_default_to_the_whole_file_once() {
        let o = ClipOptions::default();
        assert_eq!(
            (o.max_width, o.segment, o.loop_playback),
            (None, Segment::WHOLE, false)
        );
    }
}
