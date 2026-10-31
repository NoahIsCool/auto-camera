//! An excursion of a device's host times: a jump not reported yet, which
//! may still come back; the run of blocks back that may yet last as a
//! rewind, steady or scattered; and an excursion back that a jump ahead
//! ended, which may still turn out to be a rewind, the first step of a
//! glitch's way back, or an excursion back with a glitch ahead within it;
//! see [`super`] for the theory.
//!
//! # Invariants
//!
//! * Offsets are net offsets from the excursion's old timeline, in
//!   nanoseconds, positive ahead; a [`Rewound`] keeps its own from the
//!   timeline before the rewind, so the two add up.
//! * `back` describes only the newest run beyond the frozen jitter range
//!   (at least [`super::MIN_TOLERANCE`]); it restarts when a block is not back.
//!   It counts readings of the host time, so a buffer cut into blocks counts
//!   once ([`Captured::cut`](super::Captured::cut)).
//! * Nothing here moves the map or reads a clock: the stamper does, from
//!   what these report.

use super::{Discontinuity, Noise, REWIND_BLOCKS, duration};

/// Consecutive blocks back beyond the excursion's frozen jitter range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BackRun {
    /// Readings of the host time in it (blocks not cut from a longer
    /// buffer).
    readings: u64,
    /// Net offset of the first block of its newest stretch at one level,
    /// in nanoseconds.
    level: i128,
    /// Readings in a row, including the newest, within the learned
    /// tolerance of `level`.
    steady: u64,
}

/// A jump not reported yet, which may still come back; see the module
/// docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Excursion {
    /// The map before the jump, in nanoseconds, put back if it comes back.
    pub(super) origin: i128,
    /// The block that jumped.
    pub(super) seq: u64,
    /// The newest block's net offset from the old timeline, in
    /// nanoseconds: positive ahead.
    pub(super) offset: i128,
    /// Calm-stream jitter thresholds, frozen when this excursion opens.
    pub(super) noise: Noise,
    /// The largest net offset so far, either way, in nanoseconds.
    pub(super) widest: i128,
    /// The tolerance it is reported by, in nanoseconds: half the block
    /// before the jump, at least [`super::MIN_TOLERANCE`].
    pub(super) tolerance: i128,
    /// The block after the newest one that was beyond the tolerance.
    pub(super) beyond: u64,
    /// The newest block's run back beyond the frozen jitter range, if any.
    back: Option<BackRun>,
    /// Sample frames from the start of the block that jumped to the start
    /// of the next block expected, dropped ones included.
    pub(super) frames: u64,
    /// True for the connection's first excursion, opened by a second block
    /// that jumped from the first: its map follows its offset either way,
    /// and it ends as the first block's error when it does not come back.
    pub(super) first: bool,
    /// The steady excursion back that this excursion's jump ahead ended,
    /// not decided yet.
    pub(super) rewound: Option<Rewound>,
}

impl Excursion {
    /// The excursion that block `seq` opens by jumping `off` nanoseconds
    /// from the map `origin`, reported by `tolerance`.
    pub(super) fn new(origin: i128, seq: u64, off: i128, tolerance: i128) -> Self {
        Self {
            origin,
            seq,
            offset: off,
            noise: Noise::default(),
            widest: off.abs(),
            tolerance,
            beyond: if off.abs() > tolerance { seq + 1 } else { seq },
            back: (off < 0).then_some(BackRun {
                readings: 1,
                level: off,
                steady: 1,
            }),
            frames: 0,
            first: false,
            rewound: None,
        }
    }

    /// The connection's first excursion, opened by block `seq` (the second
    /// one) jumping `off` nanoseconds from the first block's map `origin`.
    pub(super) fn first(origin: i128, seq: u64, off: i128, tolerance: i128) -> Self {
        Self {
            first: true,
            ..Self::new(origin, seq, off, tolerance)
        }
    }

    /// Takes in block `seq`, whose net offset is now `offset` and which
    /// did not end the excursion, a `reading` of the host time of its own
    /// unless it was cut from a longer buffer: the widest offset, the
    /// tolerance, and the run of blocks back and its level.
    pub(super) fn widen(&mut self, seq: u64, reading: bool) {
        let jump = self.noise.jump;
        self.widest = self.widest.max(self.offset.abs());
        if self.offset.abs() > self.tolerance {
            self.beyond = seq + 1;
        }
        if self.offset < -jump {
            let run = self.back.get_or_insert(BackRun {
                readings: 0,
                level: self.offset,
                steady: 0,
            });
            let read = u64::from(reading);
            run.readings += read;
            if (self.offset - run.level).abs() > self.noise.level {
                (run.level, run.steady) = (self.offset, read);
            } else {
                run.steady += read;
            }
        } else {
            self.back = None;
        }
    }

    /// True when the run of blocks back is steady enough to be a rewind
    /// that lasted: [`REWIND_BLOCKS`] readings in a row at one level; see
    /// the module docs.
    pub(super) fn steady(&self) -> bool {
        self.back.is_some_and(|run| run.steady >= REWIND_BLOCKS)
    }

    /// True while it cannot be decided yet once its window has passed: it
    /// is back, but for fewer than [`REWIND_BLOCKS`] readings, too few to
    /// tell a rewind from scatter (see the module docs). Never for the
    /// first excursion.
    pub(super) fn undecided(&self) -> bool {
        !self.first && self.back.is_some_and(|run| run.readings < REWIND_BLOCKS)
    }

    /// What is reported when block `seq` is back on the old timeline: a
    /// glitch, when the excursion went beyond its tolerance.
    pub(super) fn closed(&self, seq: u64) -> Option<Discontinuity> {
        (self.widest > self.tolerance).then(|| Discontinuity::Glitch {
            seq: self.seq,
            blocks: seq.saturating_sub(self.seq),
            by: duration(self.widest),
        })
    }

    /// What is reported when it lasted, steady when back: a rewind, a gap
    /// beyond its tolerance or, within it, a glitch when it went beyond
    /// it.
    pub(super) fn lasting(&self) -> Option<Discontinuity> {
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

/// An excursion back, steady, that a jump ahead not landing on its old
/// timeline ended, kept in the excursion that jump opened until that one
/// shows what it was: a rewind that lasted (that excursion lasts), the
/// first step of a glitch's way back (a block lands on the old timeline),
/// or a glitch ahead within it (a block lands back on it, and it goes on);
/// see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rewound {
    /// The map before it, in nanoseconds, put back if it was a glitch.
    pub(super) origin: i128,
    /// Its first block.
    pub(super) seq: u64,
    /// How far back it was, in nanoseconds (above zero).
    pub(super) by: i128,
    /// The largest net offset from its old timeline so far, either way,
    /// in nanoseconds.
    pub(super) widest: i128,
    /// The tolerance it is reported by, in nanoseconds.
    pub(super) tolerance: i128,
    /// The block after its newest one beyond the tolerance.
    beyond: u64,
    /// Sample frames from its first block to the jump ahead.
    frames: u64,
    /// The rewind's original jitter thresholds.
    noise: Noise,
}

impl Rewound {
    /// Excursion `back` (steady, back by `by` nanoseconds), ended by a jump
    /// ahead to a net offset of `offset` from its old timeline.
    pub(super) fn of(back: &Excursion, by: i128, offset: i128) -> Self {
        Self {
            origin: back.origin,
            seq: back.seq,
            by,
            widest: back.widest.max(offset.abs()),
            tolerance: back.tolerance,
            beyond: back.beyond,
            frames: back.frames,
            noise: back.noise,
        }
    }

    /// The excursion back going on at block `seq` (a `reading` of its own
    /// or not), back on its level at a net offset of `offset` from its old
    /// timeline, `frames` after the jump ahead: that jump was a glitch
    /// within it.
    pub(super) fn resume(&self, seq: u64, offset: i128, frames: u64, reading: bool) -> Excursion {
        let mut back = Excursion {
            seq: self.seq,
            widest: self.widest,
            beyond: self.beyond,
            frames: self.frames.saturating_add(frames),
            noise: self.noise,
            ..Excursion::new(self.origin, self.seq, offset, self.tolerance)
        };
        back.back = None;
        back.widen(seq, reading);
        back
    }

    /// What is reported when it lasted: the rewind.
    pub(super) fn lasted(&self) -> Discontinuity {
        Discontinuity::Rewind {
            seq: self.seq,
            by: duration(self.by),
        }
    }

    /// What is reported when block `seq` is back on its old timeline: one
    /// glitch from its first block, when it went beyond its tolerance.
    pub(super) fn closed(&self, seq: u64) -> Option<Discontinuity> {
        (self.widest > self.tolerance).then(|| Discontinuity::Glitch {
            seq: self.seq,
            blocks: seq.saturating_sub(self.seq),
            by: duration(self.widest),
        })
    }
}
