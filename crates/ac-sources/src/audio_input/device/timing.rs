//! A device's blocks on the input's timeline: host capture times mapped to
//! PTS, sequence numbers, and the discontinuities a device's stream has.
//!
//! # Host time and the engine's clock
//!
//! CoreAudio stamps every input buffer with the host time at which its
//! first sample frame was captured, on the Mac's host clock
//! (`mach_absolute_time`). On macOS the engine's [`crate::WallClock`]
//! (Rust's `Instant`) reads that same host clock, so a fixed offset maps
//! one onto the other exactly, with no drift between them. A connection
//! fixes the offset when it opens: it reads the host clock through its
//! stream and the pacing clock back to back, and maps the pacing clock's
//! reading when the open began to PTS 0 (the *origin*), as every input's
//! PTS count from its connection's start (a supervisor then moves them
//! onto its own timeline). A block's PTS is therefore the time its first
//! frame was captured, measured on the engine's clock. A device whose
//! sample clock runs fast or slow shows it as PTS that advance a little
//! slower or faster than its sample count: the drift that program audio
//! has to absorb, measured rather than hidden.
//!
//! The first buffer of a device that was already running can have been
//! captured a few milliseconds before the open began; the origin is then
//! moved back to it, so PTS start at 0 and never come out negative.
//!
//! # Discontinuities
//!
//! A device's blocks are contiguous: block `n + 1` is captured where block
//! `n` ended, `frames / rate` later at the nominal rate, give or take the
//! drift (parts per million of a block: microseconds). So each block is
//! expected at the end of the one before, plus whatever the hand-off
//! dropped between them ([`super::handoff`]), and the stamper reports
//! ([`Discontinuity`]):
//!
//! * **an overflow** when the hand-off dropped blocks: their sequence
//!   numbers are skipped and their time is a gap in PTS;
//! * **a gap** when a block was captured more than the tolerance (half the
//!   block before it, at least [`MIN_TOLERANCE`]) after it was expected:
//!   audio the device lost (an overload, a restarted stream). The gap is
//!   kept in PTS, as time with no audio;
//! * **a rewind** when a block was captured more than the tolerance before
//!   it was expected: the device's timestamps went back. PTS never go back,
//!   so the map is moved forward to put that block at the expected time,
//!   and stays moved.
//!
//! # Invariants
//!
//! * Sequence numbers increase by one per block, skipping one per dropped
//!   block; PTS never decrease.
//! * The stamper is pure: it does no I/O and reads no clock, so every rule
//!   above is tested with made-up host times.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ac_core::SourceId;
use ac_media::AudioBlock;

use super::handoff::{Captured, frames_duration};

/// Least distance from the expected capture time that is a jump, however
/// short the blocks: well above the jitter of CoreAudio's timestamps.
pub const MIN_TOLERANCE: Duration = Duration::from_millis(1);

/// Something wrong with a device's stream that it keeps delivering
/// through; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discontinuity {
    /// Block `seq` was captured `by` later than the audio before it ended:
    /// that much audio is missing.
    Gap {
        /// The block after the gap.
        seq: u64,
        /// How late it was.
        by: Duration,
    },
    /// Block `seq` was captured `by` before the audio before it ended: the
    /// device's timestamps went back.
    Rewind {
        /// The block.
        seq: u64,
        /// How early it was.
        by: Duration,
    },
    /// `blocks` blocks of `frames` sample frames (`lasting` in all) were
    /// dropped just before block `seq`, because the input's worker fell
    /// behind its device's callback.
    Overflow {
        /// The block after them.
        seq: u64,
        /// Blocks dropped.
        blocks: u64,
        /// Their sample frames.
        frames: u64,
        /// Their duration at the nominal rate.
        lasting: Duration,
    },
    /// The device reported an overload: its I/O cycle ran late, so its
    /// audio may have a gap.
    Overload,
}

impl fmt::Display for Discontinuity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        /// Milliseconds with one decimal.
        fn ms(d: &Duration) -> String {
            format!("{:.1} ms", d.as_secs_f64() * 1000.0)
        }
        match self {
            Discontinuity::Gap { seq, by } => write!(
                f,
                "{} of audio missing before block {seq} (its host time jumped ahead)",
                ms(by)
            ),
            Discontinuity::Rewind { seq, by } => {
                write!(f, "host time went back {} at block {seq}", ms(by))
            }
            Discontinuity::Overflow {
                seq,
                blocks,
                frames,
                lasting,
            } => write!(
                f,
                "{blocks} block(s) ({frames} sample frames, {}) dropped before block {seq}: \
                 the input fell behind its device",
                ms(lasting)
            ),
            Discontinuity::Overload => {
                f.write_str("the device reported an overload (its audio may have a gap)")
            }
        }
    }
}

/// Where the next block is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Expected {
    /// Host time at which the previous block ended.
    host: Duration,
    /// Its tolerance: half the previous block, at least [`MIN_TOLERANCE`].
    tolerance: Duration,
}

/// Turns a device's captured blocks into tagged [`AudioBlock`]s; see the
/// module docs.
#[derive(Debug, Clone)]
pub struct Stamper {
    /// Tagged on every block.
    source: SourceId,
    /// Nominal samples per second per channel.
    sample_rate: u32,
    /// Interleaved channels.
    channels: u16,
    /// Host time that maps to PTS 0, in nanoseconds; signed so that a
    /// rewind can move the map either way of the host clock's zero.
    origin: i128,
    /// Sequence number of the next block.
    next_seq: u64,
    /// The previous block's end, once there was one.
    expected: Option<Expected>,
    /// PTS of the previous block.
    last_pts: Duration,
}

/// `d` in nanoseconds.
fn nanos(d: Duration) -> i128 {
    i128::try_from(d.as_nanos()).unwrap_or(i128::MAX)
}

/// `n` nanoseconds as a duration, zero when negative.
fn duration(n: i128) -> Duration {
    Duration::from_nanos(u64::try_from(n.max(0)).unwrap_or(u64::MAX))
}

impl Stamper {
    /// Stamps the blocks of input `source`, whose device runs at
    /// `sample_rate` with `channels` channels, mapping host time `origin`
    /// to PTS 0.
    pub fn new(source: SourceId, sample_rate: u32, channels: u16, origin: Duration) -> Self {
        Self {
            source,
            sample_rate,
            channels,
            origin: nanos(origin),
            next_seq: 0,
            expected: None,
            last_pts: Duration::ZERO,
        }
    }

    /// Sequence number the next block will get, unless blocks before it
    /// were dropped.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// The block `captured` holds, and what was wrong before it (an
    /// overflow, then a gap or a rewind).
    pub fn stamp(&mut self, captured: &Captured) -> (AudioBlock, Vec<Discontinuity>) {
        let mut found = Vec::new();
        let rate = self.sample_rate;
        self.next_seq = self.next_seq.saturating_add(captured.lost_blocks);
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        if captured.lost_blocks > 0 {
            found.push(Discontinuity::Overflow {
                seq,
                blocks: captured.lost_blocks,
                frames: captured.lost_frames,
                lasting: frames_duration(captured.lost_frames, rate),
            });
        }
        let host = nanos(captured.host_time);
        match self.expected {
            None => self.origin = self.origin.min(host),
            Some(expected) => {
                let due = nanos(expected.host) + nanos(frames_duration(captured.lost_frames, rate));
                let off = host - due;
                if off > nanos(expected.tolerance) {
                    found.push(Discontinuity::Gap {
                        seq,
                        by: duration(off),
                    });
                } else if -off > nanos(expected.tolerance) {
                    found.push(Discontinuity::Rewind {
                        seq,
                        by: duration(-off),
                    });
                    // Moves the map forward by the rewind: the block lands
                    // where it was expected.
                    self.origin += off;
                }
            }
        }
        let pts = duration(host - self.origin).max(self.last_pts);
        self.last_pts = pts;
        let channels = usize::from(self.channels.max(1));
        let frames = (captured.samples.len() / channels) as u64;
        let length = frames_duration(frames, rate);
        self.expected = Some(Expected {
            host: captured.host_time.saturating_add(length),
            tolerance: (length / 2).max(MIN_TOLERANCE),
        });
        let block = AudioBlock {
            source: self.source.clone(),
            seq,
            pts,
            sample_rate: rate,
            channels: self.channels,
            samples: Arc::from(&captured.samples[..frames as usize * channels]),
        };
        (block, found)
    }
}
