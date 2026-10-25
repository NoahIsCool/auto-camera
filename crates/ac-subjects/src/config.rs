//! Store tuning and its validation.
//!
//! Every threshold that decides identity lives in [`StoreConfig`]. The
//! defaults follow the rebuild specification and the proof of concept's
//! measured settings: a soft cap of 8 subjects per camera (hard cap 16),
//! detector confidence 0.35, association at IoU 0.3, cross-query aliasing
//! above IoU 0.8, "lost" after one second without a confident observation and
//! retirement after 30 seconds lost. An alias frames shots once it has been
//! seen in 2 detection passes that observed its subject, and stops after 8
//! such passes without it ([`crate::labels`]).
//!
//! A config is checked once, when a [`SubjectStore`](crate::SubjectStore) is
//! built, so the hot paths never see NaN thresholds or a zero capacity.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::event::StoreError;

/// Largest number of subjects one store may ever hold, whatever the config.
pub const HARD_CAP: usize = 16;

/// Tuning for a [`SubjectStore`](crate::SubjectStore).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StoreConfig {
    /// Soft cap on live subjects (tracking, lost, hidden and pinned alike), in
    /// `1..=`[`HARD_CAP`].
    pub max_subjects: usize,
    /// Detections scoring below this are ignored, in `0.0..=1.0`.
    pub min_score: f32,
    /// Smallest IoU at which a detection can continue an existing subject of
    /// the same query, in `(0.0, 1.0]`.
    pub match_iou: f64,
    /// A detection overlapping a subject of another query *above* this IoU is
    /// the same entity (an alias), never a new subject, in
    /// `match_iou..1.0`. The same threshold marks two subjects as duplicates
    /// of each other. Requiring `alias_iou >= match_iou` guarantees that an
    /// alias is never weaker evidence than an ordinary match.
    pub alias_iou: f64,
    /// When a matched detection overlaps the subject's current box by less
    /// than this IoU, the fast tracker has drifted and is restarted from the
    /// detection, in `0.0..=1.0`. A tracker that reported held-back
    /// observations and no confident one since the previous detection is
    /// restarted whatever the overlap, unless the tracker's report on the
    /// detection's frame had the match held back overlapping the detection,
    /// but no more than the subject's box ([`crate::record`]).
    pub reseed_iou: f64,
    /// A tracking subject becomes lost after this long without a confident
    /// observation. Must be positive.
    pub lost_after: Duration,
    /// A lost subject that nothing protects is retired after this long lost.
    pub retire_after: Duration,
    /// Track observations below this confidence never move a box, in
    /// `0.0..=1.0`. Those the tracker held back
    /// ([`TrackObservation::held`](crate::TrackObservation::held)) count as
    /// coasting evidence: a tracker that reports such observations and no
    /// confident one between two detections is restarted by the second
    /// ([`crate::record`]). The others are no such evidence; every usable
    /// one is kept as the tracker's report on its frame.
    pub track_min_confidence: f32,
    /// An alias frames shots only after it has been seen in this many
    /// detection passes that observed its subject ([`crate::labels`]), at
    /// least 1. It continues the subject from the first sighting either
    /// way; this only gates shots. One pass is the noise floor of
    /// people-first matching (one stray box can hand a guitar to whoever
    /// stands in front of it), and two is the smallest evidence that
    /// repeats, at the cost of showing a real alias's shots one pass later.
    pub alias_confirm_passes: u8,
    /// An alias stops framing shots after this many detection passes in a row
    /// that observed its subject without it, and needs
    /// `alias_confirm_passes` new sightings to frame them again; at least 1.
    /// Passes, not seconds, so that the rule does not depend on the
    /// detector's cadence: 8 observing passes are about 8 s of a 1 Hz OWLv2
    /// and about 30 s of a shared SAM sidecar that reaches each camera every
    /// 4 s, long enough to ride out a turned guitar or a few missed boxes.
    pub alias_stale_passes: u8,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            max_subjects: 8,
            min_score: 0.35,
            match_iou: 0.3,
            alias_iou: 0.8,
            reseed_iou: 0.5,
            lost_after: Duration::from_secs(1),
            retire_after: Duration::from_secs(30),
            track_min_confidence: 0.5,
            alias_confirm_passes: 2,
            alias_stale_passes: 8,
        }
    }
}

/// True when `v` is a finite number in `0.0..=1.0`.
fn unit(v: f64) -> bool {
    (0.0..=1.0).contains(&v)
}

impl StoreConfig {
    /// Checks every field, returning the first problem found.
    pub fn validate(&self) -> Result<(), StoreError> {
        let fail = |what: &'static str| Err(StoreError::InvalidConfig(what));
        if !(1..=HARD_CAP).contains(&self.max_subjects) {
            return fail("max_subjects must be in 1..=16");
        }
        if !unit(f64::from(self.min_score)) {
            return fail("min_score must be in 0..=1");
        }
        if !(unit(self.match_iou) && self.match_iou > 0.0) {
            return fail("match_iou must be in (0, 1]");
        }
        if !(self.alias_iou >= self.match_iou && self.alias_iou < 1.0) {
            return fail("alias_iou must be in [match_iou, 1)");
        }
        if !unit(self.reseed_iou) {
            return fail("reseed_iou must be in 0..=1");
        }
        if self.lost_after.is_zero() {
            return fail("lost_after must be positive");
        }
        if !unit(f64::from(self.track_min_confidence)) {
            return fail("track_min_confidence must be in 0..=1");
        }
        if self.alias_confirm_passes == 0 {
            return fail("alias_confirm_passes must be at least 1");
        }
        if self.alias_stale_passes == 0 {
            return fail("alias_stale_passes must be at least 1");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid_and_matches_the_spec() {
        let c = StoreConfig::default();
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(c.max_subjects, 8);
        assert_eq!(c.match_iou, 0.3);
        assert_eq!(c.alias_iou, 0.8);
        assert_eq!(c.lost_after, Duration::from_secs(1));
        assert_eq!(c.retire_after, Duration::from_secs(30));
    }

    #[test]
    fn capacity_must_be_within_the_hard_cap() {
        for max_subjects in [0, HARD_CAP + 1] {
            let c = StoreConfig {
                max_subjects,
                ..StoreConfig::default()
            };
            assert!(matches!(c.validate(), Err(StoreError::InvalidConfig(_))));
        }
        let c = StoreConfig {
            max_subjects: HARD_CAP,
            ..StoreConfig::default()
        };
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn nan_and_out_of_range_thresholds_are_rejected() {
        let d = StoreConfig::default();
        let bad = [
            StoreConfig {
                min_score: f32::NAN,
                ..d
            },
            StoreConfig {
                match_iou: 0.0,
                ..d
            },
            StoreConfig {
                match_iou: f64::NAN,
                ..d
            },
            StoreConfig {
                alias_iou: 1.0,
                ..d
            },
            StoreConfig {
                alias_iou: 0.2,
                ..d
            },
            StoreConfig {
                reseed_iou: -0.1,
                ..d
            },
            StoreConfig {
                lost_after: Duration::ZERO,
                ..d
            },
            StoreConfig {
                track_min_confidence: 1.5,
                ..d
            },
        ];
        for c in bad {
            assert!(c.validate().is_err(), "{c:?} should be rejected");
        }
    }

    #[test]
    fn alias_label_passes_default_to_two_and_eight_and_must_be_positive() {
        let d = StoreConfig::default();
        assert_eq!((d.alias_confirm_passes, d.alias_stale_passes), (2, 8));
        for c in [
            StoreConfig {
                alias_confirm_passes: 0,
                ..d
            },
            StoreConfig {
                alias_stale_passes: 0,
                ..d
            },
        ] {
            assert!(
                matches!(c.validate(), Err(StoreError::InvalidConfig(_))),
                "{c:?} should be rejected"
            );
        }
        // The smallest and the largest counts are both valid.
        for (confirm, stale) in [(1, 1), (u8::MAX, u8::MAX)] {
            let c = StoreConfig {
                alias_confirm_passes: confirm,
                alias_stale_passes: stale,
                ..d
            };
            assert_eq!(c.validate(), Ok(()));
        }
    }

    #[test]
    fn zero_retire_delay_is_allowed() {
        let c = StoreConfig {
            retire_after: Duration::ZERO,
            ..StoreConfig::default()
        };
        assert_eq!(c.validate(), Ok(()));
    }
}
