//! Presentation buffers: which frame of a source each tick shows, picked by
//! PTS.
//!
//! # The problem
//!
//! The engine thread reads every source when a tick starts. When a source's
//! frames become available at the same instants as the ticks (a 30 fps
//! source and a 30 Hz tick, both paced on one clock), scheduler and delivery
//! jitter decide tick by tick whether the newest frame is already there: the
//! source repeats a frame, then skips one, again and again. A source at a
//! slightly different rate (29.97 into 30) drifts into that collision once
//! every 33 s and stutters while it lasts. The
//! [`PhaseLock`](crate::schedule::PhaseLock) cures this for one source, the
//! *phase reference* (the program audio source, else the first), by moving
//! the ticks midway between its frames; the ticks cannot also sit midway
//! between every other source's frames.
//!
//! # A target on the source's own timeline
//!
//! Every other source gets a [`PresentationBuffer`]. The runtime offers it
//! the source's newest frame whenever the engine thread wakes, at every tick
//! and between ticks (every 10 ms at most, on the engine's clock, while the
//! thread waits), with the clock reading at which it collected the frame,
//! and asks it at every tick for the frame to show. The buffer answers with the held frame whose PTS
//! is nearest the tick's *target*, `due + offset`: `due` is the clock reading
//! at which the tick was due, and `offset` is the buffer's *lock*,
//!
//! ```text
//! offset = max over frames not shown yet (pts − collected_at) − period
//! ```
//!
//! measured when the buffer locks (see the rules below). Sources are paced on the engine's clock, so within one
//! connection the frame with PTS `p` becomes available at clock reading
//! `p − c` for a fixed `c`, plus jitter. A frame is collected at or after
//! that, so the largest `pts − collected_at` is the closest estimate of `c`,
//! and the target runs one frame period behind the newest frame that can
//! have arrived. The frame nearest the target therefore takes over half a
//! period after the lock's estimate of its arrival: the same half-period
//! margin the phase lock gives the reference, here by moving the target
//! instead of the ticks. Since `due` follows the clock exactly and the lock
//! is fixed, the choice depends only on PTS, not on when a frame happened
//! to arrive or when the engine thread woke up:
//!
//! * a source at the tick rate advances exactly one frame per tick, however
//!   it jitters, as long as no frame is more than half a period late;
//! * a 29.97 fps source into a 30 fps program repeats exactly one frame per
//!   1,001 ticks, a 60 fps one shows every other frame and a 15 fps one each
//!   frame twice.
//!
//! The price is latency: such a source is shown one to two frame periods
//! behind its newest frame, while the reference shows its newest (aligning
//! the sources' latencies is per-camera calibration, spec V8, not done
//! here). The target follows the due time rather than the master clock's
//! media time so that the lock stays valid when the phase lock moves the
//! ticks: a moved tick is due later, and its target moves with it.
//!
//! # Rules
//!
//! * **Never backwards.** The presented sequence number never decreases:
//!   while the frame nearest the target is older than the one shown, the
//!   one shown is repeated.
//! * **Epochs.** A frame of a newer epoch (the source reconnected; see
//!   `ac_media`) drops every held frame and the lock, since its PTS describe
//!   a new connection. A frame of an older epoch than the newest held, from
//!   another source, or not newer than the newest held is ignored. A frame
//!   whose PTS does not increase although its sequence number does is a
//!   break in the timeline, handled like a new epoch.
//! * **Locking.** A buffer locks once it holds two frames of the current
//!   connection: the period is measured from the PTS and sequence numbers of
//!   the oldest and newest, so missed frames do not distort it. Until then
//!   it presents the newest frame, as the engine did before. It locks again
//!   on its next new frame when the wanted frame (the one nearest the
//!   target) had not arrived on [`RELOCK_AFTER`] ticks in a row, which means
//!   the source got later than the lock allows (a single late frame is only
//!   repeated, then skipped); and as soon as a new frame arrives while the
//!   target is more than `CAPACITY − 1` periods behind the newest frame,
//!   since the frames it wants are gone from the ring. A source that went
//!   stale locks again when it resumes.
//! * **Stale.** When the newest frame was collected more than
//!   [`SOURCE_STALE_AFTER`] before the tick was due, the source has stalled:
//!   the buffer presents [`Presentation::Stale`], no frame, until a new one
//!   arrives. The core uses the same bound to call a source without new
//!   frames unavailable ([`SourceView::available`](crate::SourceView::available)
//!   false), and keeps its last
//!   picture where it is shown, since a freeze is better than black on air;
//!   but the buffer does not hand that frame out as current, tick after
//!   tick, for as long as the stall lasts.
//!
//! # The one exception to latest-frame mailboxes
//!
//! CLAUDE.md asks for latest-frame mailboxes and never a queue of frames.
//! Picking by PTS needs the frame *before* the newest, which a latest-frame
//! mailbox has replaced by the time the tick looks, so each buffer holds a
//! ring of up to [`CAPACITY`] frames: deliberately the one exception to
//! taking only the newest frame from a source (the encoder's bounded queues
//! are on the way out). It keeps the rule's purpose. The ring has
//! a fixed capacity and a new frame evicts the oldest, so a slow engine never
//! builds a backlog and is never shown a frame more than a few periods old;
//! the pixels are shared with the mailbox (`Arc`), not copied; and the ring
//! is owned by the engine thread alone, so it needs no lock. Four frames
//! cover the one to two periods between the target and the newest frame,
//! plus a period of jitter.
//!
//! The buffer is pure: it reads no clock, and time enters as clock readings
//! passed in, as for [`TickSchedule::poll`](crate::schedule::TickSchedule::poll).

use std::collections::VecDeque;
use std::time::Duration;

use ac_core::SourceId;
use ac_media::VideoFrame;

use crate::core::SOURCE_STALE_AFTER;

/// Frames a [`PresentationBuffer`] holds at most; see the module docs.
pub const CAPACITY: usize = 4;

/// Ticks in a row on which the wanted frame had not arrived, after which a
/// [`PresentationBuffer`] locks again on its next new frame.
pub const RELOCK_AFTER: u32 = 3;

/// Two frames whose distances from the target differ by at most this many
/// nanoseconds are equally near, and the older is presented. PTS and tick
/// times are rounded to whole nanoseconds, so a target on the midpoint
/// between two frames would otherwise be decided by rounding, one way on
/// one tick and the other way on the next; a microsecond is far above
/// that rounding and far below any frame period.
const TIE: i128 = 1_000;

/// What a [`PresentationBuffer`] shows at one tick.
#[derive(Debug, Clone)]
pub enum Presentation {
    /// No frame of the source has arrived yet.
    Empty,
    /// A frame not presented before.
    New(VideoFrame),
    /// The frame presented last, again: the source runs slower than the
    /// program, or the frame the target wants is late.
    Repeat(VideoFrame),
    /// The source has stalled: its newest frame was collected more than
    /// [`SOURCE_STALE_AFTER`] before the tick was due. No frame is presented
    /// until a new one arrives.
    Stale,
}

impl Presentation {
    /// The frame shown, if any.
    pub fn frame(&self) -> Option<&VideoFrame> {
        match self {
            Self::New(frame) | Self::Repeat(frame) => Some(frame),
            Self::Empty | Self::Stale => None,
        }
    }

    /// The frame shown, if any, by value.
    pub fn into_frame(self) -> Option<VideoFrame> {
        match self {
            Self::New(frame) | Self::Repeat(frame) => Some(frame),
            Self::Empty | Self::Stale => None,
        }
    }
}

/// A frame and when it was collected.
#[derive(Debug, Clone)]
struct Held {
    /// The frame.
    frame: VideoFrame,
    /// Clock reading at which the runtime collected it.
    at: Duration,
}

/// Where a buffer's target sits on the source's timeline; see the module
/// docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lock {
    /// Target PTS minus the tick's due time, in nanoseconds (negative when
    /// the source's PTS run behind the clock).
    offset: i128,
    /// The source's frame period, in nanoseconds; above zero.
    period: i128,
}

/// `d` in nanoseconds, as a signed number.
fn nanos(d: Duration) -> i128 {
    i128::try_from(d.as_nanos()).unwrap_or(i128::MAX)
}

/// One source's presentation buffer; see the module docs.
#[derive(Debug, Clone)]
pub struct PresentationBuffer {
    /// The source whose frames it takes.
    source: SourceId,
    /// Frames of the current connection, oldest first, at most [`CAPACITY`];
    /// their sequence numbers and PTS increase.
    held: VecDeque<Held>,
    /// The lock, once measured.
    lock: Option<Lock>,
    /// Sequence number of the frame presented last; never decreases, and
    /// never exceeds the newest held frame's.
    last: Option<u64>,
    /// Ticks in a row on which the wanted frame had not arrived.
    late: u32,
}

impl PresentationBuffer {
    /// An empty buffer for the frames of `source`.
    pub fn new(source: SourceId) -> Self {
        Self {
            source,
            held: VecDeque::with_capacity(CAPACITY),
            lock: None,
            last: None,
            late: 0,
        }
    }

    /// The source whose frames the buffer takes.
    pub fn source(&self) -> &SourceId {
        &self.source
    }

    /// Frames held now, at most [`CAPACITY`].
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// True before the first frame arrived.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// True while the buffer has a lock (see the module docs).
    pub fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Offers the source's newest frame, collected at clock reading `at`.
    /// A frame not newer than the newest held, of an older epoch or of
    /// another source is ignored; see the module docs for the rules.
    pub fn offer(&mut self, frame: VideoFrame, at: Duration) {
        if frame.source != self.source {
            return;
        }
        if let Some(newest) = self.held.back().map(|h| &h.frame) {
            if frame.seq <= newest.seq || frame.epoch < newest.epoch {
                return;
            }
            if frame.epoch > newest.epoch || frame.pts <= newest.pts {
                self.held.clear();
                self.lock = None;
                self.late = 0;
            }
        }
        if self.held.len() == CAPACITY {
            self.held.pop_front();
        }
        self.held.push_back(Held { frame, at });
    }

    /// The frame to show at the tick due at clock reading `due`; see the
    /// module docs.
    pub fn present(&mut self, due: Duration) -> Presentation {
        let Some(newest) = self.held.back() else {
            return Presentation::Empty;
        };
        if due.saturating_sub(newest.at) > SOURCE_STALE_AFTER {
            self.lock = None;
            self.late = 0;
            return Presentation::Stale;
        }
        let pick = match self.locked(due) {
            Some(lock) => self.nearest(due, lock),
            None => self.held.len() - 1,
        };
        let frame = self.held[pick].frame.clone();
        let new = self.last.is_none_or(|last| frame.seq > last);
        self.last = Some(frame.seq);
        if new {
            Presentation::New(frame)
        } else {
            Presentation::Repeat(frame)
        }
    }

    /// The lock for the tick due at `due`: the current one, or a new one
    /// when there is none or it must be measured again (see the module
    /// docs). `None` while it cannot be measured.
    fn locked(&mut self, due: Duration) -> Option<Lock> {
        let newest = &self.held.back()?.frame;
        let fresh = self.last.is_none_or(|last| newest.seq > last);
        if let Some(lock) = self.lock {
            let behind = nanos(newest.pts) - (nanos(due) + lock.offset);
            let lost = behind > (CAPACITY as i128 - 1) * lock.period;
            if !(fresh && (lost || self.late >= RELOCK_AFTER)) {
                return Some(lock);
            }
        }
        self.lock = self.measure();
        if self.lock.is_some() {
            self.late = 0;
        }
        self.lock
    }

    /// Measures a lock from the held frames: the period from the oldest and
    /// the newest, the offset from those not presented yet. `None` with
    /// fewer than two frames.
    fn measure(&self) -> Option<Lock> {
        let (oldest, newest) = (&self.held.front()?.frame, &self.held.back()?.frame);
        let frames = i128::from(newest.seq.checked_sub(oldest.seq)?);
        let span = nanos(newest.pts) - nanos(oldest.pts);
        if frames == 0 || span <= 0 {
            return None;
        }
        let period = (span / frames).max(1);
        let lead = self
            .held
            .iter()
            .filter(|h| self.last.is_none_or(|last| h.frame.seq > last))
            .map(|h| nanos(h.frame.pts) - nanos(h.at))
            .max()?;
        Some(Lock {
            offset: lead - period,
            period,
        })
    }

    /// Index of the held frame nearest the target of the tick due at `due`
    /// under `lock`, among the frames presented last or newer (the older of
    /// two equally near, within [`TIE`]), and notes whether the wanted frame
    /// had arrived.
    fn nearest(&mut self, due: Duration, lock: Lock) -> usize {
        let target = nanos(due) + lock.offset;
        let distance = |h: &Held| (nanos(h.frame.pts) - target).abs();
        let newest = self.held.len() - 1;
        let arrived = target <= nanos(self.held[newest].frame.pts) + lock.period / 2;
        self.late = if arrived {
            0
        } else {
            self.late.saturating_add(1)
        };
        let mut pick = newest;
        for (i, h) in self.held.iter().enumerate().rev().skip(1) {
            if self.last.is_some_and(|last| h.frame.seq < last) {
                break;
            }
            if distance(h) <= distance(&self.held[pick]) + TIE {
                pick = i;
            }
        }
        pick
    }
}

/// One buffer per source, in source order, except for the phase reference
/// at index `reference`, which the [`PhaseLock`](crate::schedule::PhaseLock)
/// aligns by moving the ticks and which is read as it is (`None`).
pub fn buffers_for(
    sources: impl IntoIterator<Item = SourceId>,
    reference: usize,
) -> Vec<Option<PresentationBuffer>> {
    sources
        .into_iter()
        .enumerate()
        .map(|(i, id)| (i != reference).then(|| PresentationBuffer::new(id)))
        .collect()
}

#[cfg(test)]
mod tests;
