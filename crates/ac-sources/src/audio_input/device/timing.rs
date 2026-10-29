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
//! # Jumps
//!
//! A device's blocks are contiguous: block `n + 1` is captured where block
//! `n` ended, `frames / rate` later at the nominal rate, give or take the
//! drift (parts per million of a block: microseconds). So each block is
//! expected at the end of the one before, plus whatever the hand-off
//! dropped between them ([`super::handoff`]). A block captured more than
//! the tolerance (half the block before it, at least [`MIN_TOLERANCE`])
//! after it was expected is a *jump ahead*: audio the device lost (an
//! overload, a restarted stream), or a host time that is wrong. One
//! captured more than the tolerance before it was expected is a *jump
//! back*: the device's timestamps went back.
//!
//! # Excursions
//!
//! A jump need not last. A host time that is off for a block or a few and
//! then back where it was is a glitch of the timestamps, not of the audio:
//! nothing was lost, and the blocks after it are where they always were.
//! Taken one at a time, its two jumps would make it lasting whichever way
//! it went (the jump ahead kept as a gap in PTS, the jump back absorbed by
//! moving the map), leaving the PTS ahead of the audio for good, a step
//! that program audio would fill with silence. So the map is symmetric
//! within a short window: a jump opens an *excursion*, which lasts at most
//! [`GLITCH_WINDOW`] of the stream's audio, from the block that jumped.
//!
//! * Every block of the excursion is measured against the old timeline,
//!   the block before the jump carried on at the nominal rate: its *net
//!   offset* is where its host time is, less where that timeline has it
//!   (the sum of the blocks' own jumps and jitter since, so a jump back
//!   cancels a jump ahead, and the other way round).
//! * The map follows the net offset, not each jump. Ahead, the map is the
//!   old one and the PTS run ahead with the host times: the gap is kept.
//!   Back, the map is the old one moved forward by the offset, so the
//!   block lands where the old timeline has it and PTS never go back.
//! * A block back within the tolerance of the old timeline closes the
//!   excursion as a **glitch**: the old map is put back, so the PTS are on
//!   the old timeline again (held at the newest PTS until it catches up,
//!   as PTS never go back), and one glitch is reported: no audio missing.
//! * An excursion still open once the window has passed lasts: the map
//!   stays as its net offset left it, and it is reported once, by that
//!   offset, at the block that jumped, on the first block after the
//!   window. Ahead it is **a gap**, kept in PTS as time with no audio;
//!   back it is **a rewind**, the map moved forward for good.
//!
//! A real loss or a real rewind on its own is thus stamped at once exactly
//! as before; only its report waits for the window. No audio waits: each
//! block is stamped as it comes, so the block of a jump ahead that turns
//! out to be a glitch has already gone out with its PTS ahead, a step that
//! the block coming back steps back from. The window is long beside a
//! glitch of a few blocks and short beside what drift does to the old
//! timeline (50 µs at 100 ppm, far inside the tolerance); an excursion
//! longer than it is taken as lasting, and one still open when the stream
//! ends is not reported (the stream's end is).
//!
//! **An overflow** (the hand-off dropped blocks) is reported at once,
//! since the hand-off counted it: the blocks' sequence numbers are skipped
//! and their time is a gap in PTS.
//!
//! # Invariants
//!
//! * Sequence numbers increase by one per block, skipping one per dropped
//!   block; PTS never decrease.
//! * Every excursion is reported once: as one glitch, or as the gap or the
//!   rewind it lasted as.
//! * The map moves only within an excursion (and for a first block
//!   captured before the open, above): one that closes leaves it as it was
//!   before the jump, one that lasts as its net offset left it.
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

/// Longest excursion, in the stream's audio from the block that jumped,
/// that can still come back as a glitch; see the module docs.
pub const GLITCH_WINDOW: Duration = Duration::from_millis(500);

/// Something wrong with a device's stream that it keeps delivering
/// through; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discontinuity {
    /// Block `seq` was captured `by` later than the audio before it ended,
    /// and the stream stayed there: that much audio is missing.
    Gap {
        /// The block after the gap.
        seq: u64,
        /// How late it was.
        by: Duration,
    },
    /// Block `seq` was captured `by` before the audio before it ended, and
    /// the stream stayed there: the device's timestamps went back.
    Rewind {
        /// The block.
        seq: u64,
        /// How early it was.
        by: Duration,
    },
    /// The host times of `blocks` blocks from block `seq` on were up to
    /// `by` off the stream's timeline, and the block after them was back on
    /// it: the timestamps glitched, no audio is missing.
    Glitch {
        /// The first block that was off.
        seq: u64,
        /// How many blocks were off.
        blocks: u64,
        /// The furthest they were off, either way.
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
            Discontinuity::Glitch { seq, blocks, by } => write!(
                f,
                "host time of {blocks} block(s) from block {seq} was up to {} off, then came \
                 back: no audio missing",
                ms(by)
            ),
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

/// A jump not reported yet, which may still come back; see the module
/// docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Excursion {
    /// The map before the jump, in nanoseconds, put back if it comes back.
    origin: i128,
    /// The block that jumped.
    seq: u64,
    /// The newest block's net offset from the old timeline, in
    /// nanoseconds: positive ahead.
    offset: i128,
    /// The largest net offset so far, either way, in nanoseconds.
    widest: i128,
    /// Sample frames from the start of the block that jumped to the start
    /// of the next block expected, dropped ones included.
    frames: u64,
}

impl Excursion {
    /// The excursion reported as a jump that lasted.
    fn lasting(&self) -> Discontinuity {
        if self.offset > 0 {
            Discontinuity::Gap {
                seq: self.seq,
                by: duration(self.offset),
            }
        } else {
            Discontinuity::Rewind {
                seq: self.seq,
                by: duration(-self.offset),
            }
        }
    }
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
    /// The excursion under way, if a jump is not reported yet.
    excursion: Option<Excursion>,
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
            excursion: None,
            last_pts: Duration::ZERO,
        }
    }

    /// Sequence number the next block will get, unless blocks before it
    /// were dropped.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// The block `captured` holds, and what is reported with it, in this
    /// order: an excursion whose window has passed (a gap or a rewind, at
    /// the block that jumped), the blocks dropped just before this one (an
    /// overflow), and an excursion this block came back from (a glitch);
    /// see the module docs.
    pub fn stamp(&mut self, captured: &Captured) -> (AudioBlock, Vec<Discontinuity>) {
        let mut found = Vec::new();
        let rate = self.sample_rate;
        self.next_seq = self.next_seq.saturating_add(captured.lost_blocks);
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        if let Some(mut e) = self.excursion.take() {
            e.frames = e.frames.saturating_add(captured.lost_frames);
            if frames_duration(e.frames, rate) > GLITCH_WINDOW {
                // It lasted; the map stays where its offset left it.
                found.push(e.lasting());
            } else {
                self.excursion = Some(e);
            }
        }
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
                self.follow(seq, host - due, nanos(expected.tolerance), &mut found);
            }
        }
        let pts = duration(host - self.origin).max(self.last_pts);
        self.last_pts = pts;
        let channels = usize::from(self.channels.max(1));
        let frames = (captured.samples.len() / channels) as u64;
        let length = frames_duration(frames, rate);
        if let Some(e) = &mut self.excursion {
            e.frames = e.frames.saturating_add(frames);
        }
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

    /// Sets the map for block `seq`, captured `off` nanoseconds from where
    /// it was expected (within `tolerance` either way is no jump): opens
    /// an excursion on a jump, else follows the one under way, closing it
    /// as a glitch in `found` when the block is back; see the module docs.
    fn follow(&mut self, seq: u64, off: i128, tolerance: i128, found: &mut Vec<Discontinuity>) {
        let Some(mut e) = self.excursion.take() else {
            if off.abs() > tolerance {
                self.excursion = Some(Excursion {
                    origin: self.origin,
                    seq,
                    offset: off,
                    widest: off.abs(),
                    frames: 0,
                });
                // Back: the block lands where it was expected.
                self.origin += off.min(0);
            }
            return;
        };
        e.offset += off;
        if e.offset.abs() <= tolerance {
            found.push(Discontinuity::Glitch {
                seq: e.seq,
                blocks: seq.saturating_sub(e.seq),
                by: duration(e.widest),
            });
            self.origin = e.origin;
        } else {
            e.widest = e.widest.max(e.offset.abs());
            self.origin = e.origin + e.offset.min(0);
            self.excursion = Some(e);
        }
    }
}
