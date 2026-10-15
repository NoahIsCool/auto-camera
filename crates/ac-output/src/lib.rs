//! Program output: one H.264/AAC encode of the program feed, fanned out to an
//! RTMP stream and a local MP4 recording (spec M11).
//!
//! # Design
//!
//! * **The ffmpeg CLI as a child process.** Nothing is linked; the encoder
//!   runs in its own address space, so an encoder crash cannot take the engine
//!   down, and any ffmpeg build on the `PATH` (or at [`OutputConfig::ffmpeg`])
//!   works.
//! * **Constant frame rate by construction.** Video goes to ffmpeg's stdin as
//!   raw RGBA with a fixed `-framerate`, so frame `n` has presentation time
//!   `n / fps`. The engine pushes exactly one frame per program tick, which
//!   makes the tick counter the program clock: no wall clock, no arrival-time
//!   jitter and no ffmpeg-side frame-rate resampling.
//! * **Audio on its own pipe.** Interleaved `f32le` samples go over a
//!   loopback TCP connection that ffmpeg opens as its second input; sample
//!   `n` has time `n / sample_rate`. Both timelines are counts, so audio and
//!   video stay in sync exactly as long as the caller pushes them in step.
//!   TCP rather than a FIFO because opening a FIFO blocks the writer, forever
//!   if ffmpeg dies first, and only a raw `open(2)` flag (`unsafe` here) could
//!   avoid that; the private `transport` module has the full argument.
//! * **Bounded, non-blocking submission.** [`ProgramEncoder::push_video`] and
//!   [`ProgramEncoder::push_audio`] never block the engine tick. When a queue
//!   is full the input is dropped and counted, and the writer later repeats
//!   the previous frame (or writes silence) once per dropped frame (or
//!   sample), so the counts and therefore A/V sync survive an encoder hiccup.
//!   See [`encoder`].
//! * **One encode, two outputs.** The tee muxer feeds a fragmented MP4 (crash
//!   safe: every keyframe closes a self-contained fragment) and an FLV/RTMP
//!   stream that is isolated behind ffmpeg's `fifo` muxer with
//!   `onfail=ignore`, so a slow, failing or unreachable ingest never stalls or
//!   stops the recording. See [`args`].
//! * **Broadcast-legal picture.** Full-range RGBA is converted to limited-range
//!   BT.709 `yuv420p` and tagged as such in the container and the H.264 VUI;
//!   no B-frames; a fixed GOP of `keyframe_seconds * fps` frames.
//! * **No orphans.** [`ProgramEncoder::finish`] closes both inputs, waits with
//!   a timeout and then kills; dropping an unfinished encoder kills ffmpeg.
//!   Either way the child is reaped before the call returns.
//!
//! # Limitations
//!
//! * The RTMP URL, stream key included, is an ffmpeg argument and therefore
//!   visible to local users through the process list. Logs and `Debug`
//!   output redact it.
//! * ffmpeg cannot report a bitrate for tee output; the reported bitrate is
//!   the recording's average, and there is none when only streaming.
//! * The frame rate is a whole number (no 29.97).

pub mod args;
pub mod config;
pub mod encoder;
pub mod progress;
mod transport;

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

pub use args::build_ffmpeg_args;
pub use config::{
    AudioFormat, Encoder, EncoderChoice, OutputConfig, choose_encoder, choose_working_encoder,
};
pub use encoder::{
    DEFAULT_FINISH_TIMEOUT, EncoderStats, ProgramEncoder, Push, detect_encoder, encoder_works,
};
pub use progress::Progress;

/// Errors from configuring, running or stopping the program encoder.
#[derive(Debug, Error)]
pub enum OutputError {
    /// The configuration is unusable; the message names the field.
    #[error("invalid output config: {0}")]
    InvalidConfig(String),
    /// The ffmpeg executable could not be found or started.
    #[error("ffmpeg not found at {0:?}")]
    FfmpegNotFound(PathBuf),
    /// The requested H.264 encoder is not built into this ffmpeg.
    #[error("encoder {0} is not available in this ffmpeg build")]
    EncoderUnavailable(String),
    /// A video frame does not have `width * height * 4` bytes.
    #[error("video frame has {actual} bytes, expected {expected}")]
    FrameSize {
        /// Bytes required by the configured size.
        expected: usize,
        /// Bytes supplied.
        actual: usize,
    },
    /// A video frame has different dimensions from the output.
    #[error("video frame is {actual:?}, output is {expected:?}")]
    FrameDimensions {
        /// Configured `(width, height)`.
        expected: (u32, u32),
        /// Supplied `(width, height)`.
        actual: (u32, u32),
    },
    /// Audio was pushed to an encoder configured without audio.
    #[error("the encoder was started without audio")]
    AudioNotConfigured,
    /// An audio buffer is not a whole number of sample frames.
    #[error("{len} samples is not a multiple of {channels} channels")]
    AudioLength {
        /// Samples supplied.
        len: usize,
        /// Configured channel count.
        channels: u16,
    },
    /// An audio block's format differs from the configured input format.
    #[error("audio block is {actual:?}, encoder expects {expected:?}")]
    AudioFormatMismatch {
        /// Configured format.
        expected: AudioFormat,
        /// Supplied format.
        actual: AudioFormat,
    },
    /// ffmpeg has exited or closed the input, so nothing more can be encoded.
    #[error("the encoder has stopped")]
    Stopped,
    /// ffmpeg exited unsuccessfully.
    #[error("ffmpeg exited with {status}: {}", last_error.as_deref().unwrap_or("no error output"))]
    Failed {
        /// Exit status as reported by the OS.
        status: String,
        /// Last line ffmpeg wrote to stderr, with stream keys redacted.
        last_error: Option<String>,
    },
    /// ffmpeg did not exit within the finish timeout and was killed.
    #[error("ffmpeg did not finish within {0:?} and was killed")]
    Timeout(Duration),
    /// An operating-system error while starting or talking to ffmpeg.
    #[error("output I/O: {0}")]
    Io(#[from] std::io::Error),
}
