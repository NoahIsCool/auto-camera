//! A device's blocks on the input's timeline: host capture times mapped to
//! PTS, sequence numbers, and the discontinuities measured in a device's
//! stream (an overload the device reports is the input's to count, see
//! [`super::input`]).
//!
//! # Host time and the engine's clock
//!
//! CoreAudio stamps every input buffer with a host time (`mHostTime`) on
//! the Mac's host clock (`mach_absolute_time`). cpal takes the device's
//! latency off it (its IO buffer, latency and safety offset, read once
//! when the stream is built), and the input holds that latency fixed for
//! the connection ([`CaptureLatency`]), so a buffer's capture time is its
//! host time less a constant: a few to tens of milliseconds before
//! CoreAudio's stamp, the same for every buffer of a connection. On macOS
//! the engine's [`crate::WallClock`] (Rust's `Instant`) reads that same
//! host clock, so a fixed offset maps one onto the other exactly, with no
//! drift between them. A connection fixes the offset when it opens: it
//! reads the host clock through its stream and the pacing clock back to
//! back, and maps the pacing clock's reading when the open began to PTS 0
//! (the *origin*), as every input's PTS count from its connection's start
//! (a supervisor then moves them onto its own timeline). A block's PTS is
//! therefore its capture time on the engine's clock: early by the
//! connection's constant latency, which shifts where the timeline starts
//! and nothing else, since program audio is paced by its sample count and
//! its drift is measured from how the PTS advance. A device whose sample
//! clock runs fast or slow shows it as PTS that advance a little slower or
//! faster than its sample count: the drift that program audio has to
//! absorb, measured rather than hidden.
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
//! dropped between them ([`mod@super::handoff`]). A block captured more
//! than [`MIN_TOLERANCE`] from where it was expected, either way, *jumped*
//! (a millisecond is well above the jitter of CoreAudio's timestamps):
//! ahead, audio the device lost (an overload, a restarted stream) or a
//! host time that is wrong; back, the device's timestamps went back. A jump
//! is reported by a tolerance of its own, half the block before it (at
//! least [`MIN_TOLERANCE`]): a lost buffer is never shorter than a block,
//! so a jump ahead within it is not audio lost.
//!
//! The first block's host time is provisional. A device's first IO cycle
//! is the likeliest to be stamped off, and two blocks cannot tell which of
//! them is: a second block that jumped from the first is stamped as if it
//! were the first, its host time less the first block's length and what
//! was dropped between them taken for the first block's (so the origin is
//! the open's again, unless that is after it), and nothing is reported.
//! Only the first block's own PTS then carry its error; audio lost between
//! the first two blocks stays in the PTS, as any gap, but is not reported.
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
//! * A block back within [`MIN_TOLERANCE`] of the old timeline closes the
//!   excursion as a **glitch**: the old map is put back, so the PTS are on
//!   the old timeline again (held at the newest PTS until it catches up,
//!   as PTS never go back), and, when it went beyond its tolerance, one
//!   glitch is reported: no audio missing.
//! * A glitch's way back lands on the old timeline; audio lost after the
//!   timestamps went back for good does not, unless it happens to be as
//!   long as the rewind. So an excursion that has been back for
//!   [`REWIND_BLOCKS`] blocks in a row and then jumps ahead without landing
//!   there lasted: it is reported as a **rewind** at once, the map left
//!   moved forward, and the jump ahead is a new jump from there, kept in
//!   PTS like any other. Otherwise the part of the loss that the rewind
//!   cancelled would be missing from the PTS, and so from what program
//!   audio makes up.
//! * An excursion still open once the window has passed lasts: the map
//!   stays as its net offset left it, and it is reported once, at the
//!   block that jumped, on the first block after the window. Ahead beyond
//!   its tolerance it is **a gap**, kept in PTS as time with no audio
//!   (ahead within it, a glitch when it went beyond its tolerance and came
//!   back within it, else nothing); back it is **a rewind**, whatever its
//!   size, as the map moved for it.
//!
//! A real loss or a real rewind on its own is thus stamped at once exactly
//! as before; only its report waits for the window. No audio waits: each
//! block is stamped as it comes, so the block of a jump ahead that turns
//! out to be a glitch has already gone out with its PTS ahead, a step that
//! the block coming back steps back from. The window is long beside a
//! glitch of a few blocks and short beside what drift does to the old
//! timeline (50 µs at 100 ppm, far inside [`MIN_TOLERANCE`]); an excursion
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
//! * Every excursion beyond its tolerance is reported once, as one glitch
//!   or as the gap or the rewind it lasted as, and so is every rewind that
//!   lasted, whatever its size; an excursion within its tolerance that did
//!   not last back is not reported.
//! * The map moves only within an excursion, at a rewind that lasted, and
//!   for the first two blocks (a first block captured before the open, a
//!   second one that jumped from it): one that closes leaves it as it was
//!   before the jump, one that lasts as its net offset left it.
//! * The stamper is pure: it does no I/O and reads no clock, so every rule
//!   above is tested with made-up host times.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ac_core::SourceId;
use ac_media::AudioBlock;

use super::handoff::{Captured, frames_duration};

/// Distance from the expected capture time beyond which a block jumped,
/// either way, however long the blocks: well above the jitter of
/// CoreAudio's timestamps. Also the least tolerance a jump is reported by.
pub const MIN_TOLERANCE: Duration = Duration::from_millis(1);

/// Blocks in a row an excursion must have been back for a jump ahead that
/// does not land on the old timeline to end it as a rewind that lasted;
/// see the module docs.
pub const REWIND_BLOCKS: u64 = 3;

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
        }
    }
}

/// Where the next block is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Expected {
    /// Host time at which the previous block ended.
    host: Duration,
    /// The tolerance a jump from it is reported by: half the previous
    /// block, at least [`MIN_TOLERANCE`].
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
    /// The tolerance it is reported by, in nanoseconds: half the block
    /// before the jump, at least [`MIN_TOLERANCE`].
    tolerance: i128,
    /// The block after the newest one that was beyond the tolerance.
    beyond: u64,
    /// Blocks in a row, the newest included, that were back by more than
    /// [`MIN_TOLERANCE`].
    back: u64,
    /// Sample frames from the start of the block that jumped to the start
    /// of the next block expected, dropped ones included.
    frames: u64,
}

impl Excursion {
    /// The excursion that block `seq` opens by jumping `off` nanoseconds
    /// from the map `origin`, reported by `tolerance`.
    fn new(origin: i128, seq: u64, off: i128, tolerance: i128) -> Self {
        Self {
            origin,
            seq,
            offset: off,
            widest: off.abs(),
            tolerance,
            beyond: if off.abs() > tolerance { seq + 1 } else { seq },
            back: u64::from(off < 0),
            frames: 0,
        }
    }

    /// What is reported when block `seq` is back on the old timeline: a
    /// glitch, when the excursion went beyond its tolerance.
    fn closed(&self, seq: u64) -> Option<Discontinuity> {
        (self.widest > self.tolerance).then(|| Discontinuity::Glitch {
            seq: self.seq,
            blocks: seq.saturating_sub(self.seq),
            by: duration(self.widest),
        })
    }

    /// What is reported when it lasted: a rewind, a gap beyond its
    /// tolerance or, within it, a glitch when it went beyond it.
    fn lasting(&self) -> Option<Discontinuity> {
        if self.offset < 0 {
            Some(Discontinuity::Rewind {
                seq: self.seq,
                by: duration(-self.offset),
            })
        } else if self.offset > self.tolerance {
            Some(Discontinuity::Gap {
                seq: self.seq,
                by: duration(self.offset),
            })
        } else {
            self.closed(self.beyond)
        }
    }
}

/// A connection's capture latency, fixed by its first buffer: what cpal
/// takes off that buffer's host time for its capture time, taken off every
/// later buffer's too, so that a change in the callbacks' length cannot
/// move the capture times (see `coreaudio`'s module docs). For the
/// device's callback: it never blocks or allocates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureLatency {
    /// The first buffer's host time less its capture time, once there was
    /// a buffer.
    fixed: Option<Duration>,
}

impl CaptureLatency {
    /// The capture time of a buffer CoreAudio stamped `host` and cpal
    /// gives as captured at `capture`: `host` less the first buffer's
    /// latency (zero for a capture time after its host time).
    pub fn capture(&mut self, host: Duration, capture: Duration) -> Duration {
        let latency = *self.fixed.get_or_insert(host.saturating_sub(capture));
        host.saturating_sub(latency)
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
    /// The origin it was made with, in nanoseconds: the host time of the
    /// open.
    opened: i128,
    /// The first block's host time, in nanoseconds, until the second
    /// block is stamped: provisional, see the module docs.
    first: Option<i128>,
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
            opened: nanos(origin),
            first: None,
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
                found.extend(e.lasting());
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
            None => {
                self.origin = self.origin.min(host);
                self.first = Some(host);
            }
            Some(expected) => {
                let due = nanos(expected.host) + nanos(frames_duration(captured.lost_frames, rate));
                let off = host - due;
                match self.first.take() {
                    // The second block jumped from the first: stamped as
                    // if it were the first (see the module docs).
                    Some(first) if off.abs() > nanos(MIN_TOLERANCE) => {
                        self.origin = self.opened.min(host - (due - first));
                    }
                    _ => self.follow(seq, off, nanos(expected.tolerance), &mut found),
                }
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
    /// it was expected (within [`MIN_TOLERANCE`] either way is no jump; a
    /// jump is reported by `tolerance`): opens an excursion on a jump, else
    /// follows the one under way, ending it in `found` as a glitch when the
    /// block is back, or as a rewind when it lasted back and the block
    /// jumped ahead; see the module docs.
    fn follow(&mut self, seq: u64, off: i128, tolerance: i128, found: &mut Vec<Discontinuity>) {
        let jump = nanos(MIN_TOLERANCE);
        let Some(mut e) = self.excursion.take() else {
            self.open(seq, off, tolerance);
            return;
        };
        let before = e.offset;
        e.offset += off;
        if before < -jump && e.back >= REWIND_BLOCKS && off > jump && e.offset.abs() > jump {
            // It lasted back: the map stays moved forward by it, and this
            // block's jump ahead is a new one from there.
            found.push(Discontinuity::Rewind {
                seq: e.seq,
                by: duration(-before),
            });
            self.open(seq, off, tolerance);
            return;
        }
        if e.offset.abs() <= jump {
            found.extend(e.closed(seq));
            self.origin = e.origin;
            return;
        }
        e.widest = e.widest.max(e.offset.abs());
        if e.offset.abs() > e.tolerance {
            e.beyond = seq + 1;
        }
        e.back = if e.offset < -jump { e.back + 1 } else { 0 };
        self.origin = e.origin + e.offset.min(0);
        self.excursion = Some(e);
    }

    /// Opens an excursion when block `seq`, captured `off` nanoseconds from
    /// where it was expected, jumped: back, the map moves forward so that
    /// the block lands where it was expected.
    fn open(&mut self, seq: u64, off: i128, tolerance: i128) {
        if off.abs() > nanos(MIN_TOLERANCE) {
            self.excursion = Some(Excursion::new(self.origin, seq, off, tolerance));
            self.origin += off.min(0);
        }
    }
}
