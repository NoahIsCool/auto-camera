//! Video sources: local files decoded by ffmpeg, and a synthetic test pattern.
//!
//! Every source implements [`ac_media::VideoSource`]. It decodes on its own
//! worker threads, publishes frames into a latest-frame mailbox and queues
//! audio in a small bounded queue, so the engine never waits on a source and a
//! slow reader never builds a backlog.
//!
//! * [`FileSource`] plays a local video file (and optionally its audio) through
//!   the `ffmpeg` command-line tool running as child processes. Nothing links
//!   against FFmpeg's libraries, so the crate builds anywhere and runs wherever
//!   an `ffmpeg` binary is installed, on macOS and Linux alike. It can play a
//!   [`segment`] of the file as if it were a clip of its own.
//! * [`ClipReader`] decodes a file frame by frame on demand, with no clock and
//!   no thread, for offline tools that step the engine on simulated time
//!   ([`clip`]).
//! * [`SyntheticSource`] renders a deterministic moving test pattern in pure
//!   Rust and exposes the exact ground-truth box of every moving subject, for
//!   demos and for testing everything downstream.
//!
//! # Timing
//!
//! Every frame carries the source id, a sequence number that increases by one
//! per delivered frame, and a PTS computed from that number and an exact
//! rational [`FrameRate`] (`pts = seq / fps`). PTS therefore never drifts or
//! repeats, including across loops of a file, and audio PTS is counted in
//! samples on the same timeline. Delivery is paced on a [`Clock`]: real
//! playback uses [`WallClock`]; tests and offline tools use [`VirtualClock`],
//! which runs the identical code unpaced.
//!
//! # Configuration
//!
//! [`SourceConfig`] is the serializable description of a source and
//! [`open_source`] turns one into a running `Box<dyn VideoSource>`.

pub mod audio;
pub mod clip;
pub mod clock;
pub mod config;
pub mod ffmpeg;
pub mod file;
pub mod frame_rate;
mod pool;
pub mod probe;
mod process;
pub mod segment;
mod status;
pub mod synthetic;

pub use audio::BLOCK_MILLIS;
pub use clip::{ClipOptions, ClipReader};
pub use clock::{Clock, MAX_SLEEP_SLICE, VirtualClock, WallClock};
pub use config::{SourceConfig, open_source, open_source_with_clock};
pub use ffmpeg::output_size;
pub use file::{DEFAULT_AUDIO_QUEUE_BLOCKS, FileOptions, FileSource};
pub use frame_rate::FrameRate;
pub use probe::{
    AudioStreamInfo, FFMPEG_ENV, FFPROBE_ENV, FfmpegTools, ProbeInfo, VideoStreamInfo, parse_probe,
    probe,
};
pub use synthetic::{GroundTruth, SyntheticPattern, SyntheticSource};
