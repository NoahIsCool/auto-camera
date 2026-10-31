//! A recent reference for timestamp phase, independent of the last buffer's
//! jitter. Phase is host time less nominal elapsed audio (including dropped
//! frames); its median follows slow clock drift without replacing raw PTS.
//! Excursions freeze the reference. Closed glitches rejoin the history,
//! keeping its noise distribution unbiased; lasting changes replace it
//! with their final level, so a short ramp cannot become a new baseline.

use std::collections::VecDeque;
use std::time::Duration;

use super::{MIN_TOLERANCE, nanos};

#[cfg(test)]
mod tests;

/// Reference history in nanoseconds of audio.
const WINDOW: i128 = 2_000_000_000;
/// Bounds memory even for tiny callback buffers.
const CAPACITY: usize = 1024;

/// A callback's phase and its position on the nominal audio timeline.
#[derive(Debug, Clone, Copy)]
struct Reading {
    /// Nominal elapsed audio in nanoseconds.
    at: i128,
    /// Host time minus nominal elapsed audio, in nanoseconds.
    phase: i128,
}

/// Frozen classification thresholds learned from calm callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Noise {
    /// Distance from the reference required to open or close an excursion.
    pub(super) jump: i128,
    /// Spread allowed between readings at a lasting rewind's level.
    pub(super) level: i128,
}

impl Default for Noise {
    fn default() -> Self {
        Self {
            jump: nanos(MIN_TOLERANCE),
            level: nanos(MIN_TOLERANCE) / 4,
        }
    }
}

/// A bounded phase history, with provisional readings held separately.
#[derive(Debug, Clone, Default)]
pub(super) struct Anchor {
    /// Confirmed, calm phase readings.
    calm: VecDeque<Reading>,
    /// Readings since the current excursion opened.
    pending: VecDeque<Reading>,
    /// Elapsed audio at the next block's start, dropped frames included.
    pub(super) elapsed: i128,
}

impl Anchor {
    /// Offset from the recent median, and conservative jitter thresholds;
    /// falls back to the adjacent-block offset before a reference exists.
    /// Median absolute deviation estimates jitter without a minority of
    /// glitch callbacks setting its scale. Margins cover unsampled tails and slow
    /// clock drift; large confirmed spikes are excluded by [`Self::close`].
    pub(super) fn measure(&self, host: i128, adjacent: i128) -> (i128, Noise) {
        if self.calm.len() < 3 {
            return (adjacent, Noise::default());
        }
        let mut sorted: Vec<_> = self.calm.iter().map(|r| r.phase).collect();
        sorted.sort_unstable();
        let mid = sorted[sorted.len() / 2];
        for phase in &mut sorted {
            *phase = (*phase - mid).abs();
        }
        sorted.sort_unstable();
        let deviation = sorted[sorted.len() / 2];
        let noise = Noise {
            jump: nanos(MIN_TOLERANCE).max(deviation * 4 + 250_000),
            level: (nanos(MIN_TOLERANCE) / 4).max(deviation * 5 + 50_000),
        };
        (host - self.elapsed - mid, noise)
    }

    /// Takes one independent callback reading after classification. A
    /// closed glitch rejoins the noise history; calm readings age by audio.
    pub(super) fn observe(&mut self, host: i128, provisional: bool) {
        let reading = Reading {
            at: self.elapsed,
            phase: host - self.elapsed,
        };
        let into = if provisional {
            &mut self.pending
        } else {
            // A returned excursion is weaker evidence than the confirmed
            // reference. It must not become half the startup history and
            // select its own phase median or deviation threshold.
            if self.pending.len() * 2 > self.calm.len() {
                self.pending.clear();
            }
            self.calm.append(&mut self.pending);
            &mut self.calm
        };
        into.push_back(reading);
        while into.len() > CAPACITY || into.front().is_some_and(|r| reading.at - r.at > WINDOW) {
            into.pop_front();
        }
    }

    /// A confirmed spike far beyond the established noise envelope is not
    /// noise evidence. Smaller closed excursions rejoin the history so
    /// excluding negative jitter tails cannot bias its center upward.
    pub(super) fn close(&mut self, widest: i128, noise: Noise) {
        if widest > noise.jump * 4 {
            self.discard();
        }
    }

    /// A scattered excursion that outlived its window teaches no new level.
    pub(super) fn discard(&mut self) {
        self.pending.clear();
    }

    /// A lasting excursion establishes only its final epoch as the reference;
    /// a preceding rewind in a nested rewind/gap must not inflate its noise.
    pub(super) fn confirm(&mut self, lasting: Duration) {
        let since = self.elapsed - nanos(lasting);
        self.pending.retain(|r| r.at >= since);
        if !self.pending.is_empty() {
            self.calm = std::mem::take(&mut self.pending);
        }
    }
}
