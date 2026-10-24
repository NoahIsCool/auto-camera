//! Shot labels: which of the queries a subject answers make its shots.
//!
//! A subject answers its primary query and its aliases. Aliasing is about
//! identity ([`crate::associate`], phase 4): a box of another prompt that is
//! the same entity continues the subject instead of admitting a twin, and it
//! must do so from the first sighting, or the second prompt's box in a late
//! pass becomes a duplicate subject. Shots are another matter. Every query a
//! subject answers frames that query's shots (ADR 0003), and a single stray
//! pass would then frame them for good: people-first matching once handed a
//! guitar-and-legs box to an audience member standing in front of the
//! guitarist, the box aliased "guitarist" onto them, and nothing ever took
//! it away, so the shot list kept a "Guitarist 7 Close" of the audience.
//!
//! So identity stays as it is, and each answered query keeps a
//! [`LabelEvidence`] that decides only whether it frames shots. Evidence is
//! counted in **observing passes**: detection batches that observed the
//! subject (matched, aliased, recovered or admitted it). A query is *seen*
//! in such a pass when one of the pass's detections of that query reached
//! the subject: matched it, aliased onto it, landed on it as a duplicate box
//! or admitted it. For every query the subject answers, each observing pass
//! does
//!
//! ```text
//! seen:      sightings = min(sightings + 1, confirm)    missed = 0
//! not seen:  missed    = missed + 1 (saturating)        if missed >= stale { sightings = 0 }
//! frames     = sightings >= confirm
//! ```
//!
//! with `confirm` = [`StoreConfig::alias_confirm_passes`] and `stale` =
//! [`StoreConfig::alias_stale_passes`]. A label therefore frames shots once
//! it has been seen in `confirm` observing passes with fewer than `stale`
//! passes without it between any two of them, and stops after `stale`
//! observing passes in a row without it; it then needs `confirm` new
//! sightings to frame again. The primary query frames shots whatever its
//! evidence says ([`SubjectRecord::shot_queries`]): which query names a
//! subject is the admission rule's and the operator's business, not this
//! module's.
//!
//! Counting passes rather than seconds keeps the rule independent of the
//! detector's cadence: 8 observing passes are about 8 s of a 1 Hz OWLv2 and
//! about 30 s of a shared SAM sidecar that reaches each camera every 4 s,
//! and either way a label survives the same number of missed looks. Only
//! passes that observed the subject count, so a lost subject keeps its
//! labels however long it is gone, and the pass that recovers it costs each
//! query it did not see one miss, not the length of the gap.
//!
//! # Invariants
//!
//! * `sightings <= confirm`, and `missed >= stale` implies `sightings == 0`,
//!   so a label that frames has missed fewer than `stale` passes.
//! * `frames == (sightings >= confirm)` after every count.
//! * Two boxes of one query in one pass are one sighting: a pass counts at
//!   most once per subject and query.
//! * Evidence changes only in a detection pass that observes the subject
//!   and in [`SubjectStore::remove_query`](crate::SubjectStore::remove_query),
//!   which drops the removed query's entry. Ticks, losses, operator flags and
//!   reacquires never touch it, and it never changes which subject a
//!   detection continues.
//! * After a pass that observed the subject, there is exactly one entry per
//!   query it answers. It is pure and deterministic: replaying the same calls
//!   gives the same evidence.

use std::collections::BTreeSet;

use ac_core::QueryId;

use crate::config::StoreConfig;
use crate::record::SubjectRecord;

/// The evidence that a subject answers one query, counted in observing
/// passes; see the [module documentation](crate::labels).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LabelEvidence {
    /// Observing passes that saw the query on the subject, at most
    /// [`StoreConfig::alias_confirm_passes`]; back to zero once the query has
    /// been missed [`StoreConfig::alias_stale_passes`] passes in a row.
    pub sightings: u8,
    /// Observing passes in a row without the query since its last sighting
    /// (saturating).
    pub missed: u8,
    /// The label frames shots: `sightings` reached
    /// [`StoreConfig::alias_confirm_passes`]. Kept by the count, so that
    /// [`SubjectRecord::shot_queries`] needs no config.
    pub frames: bool,
}

impl LabelEvidence {
    /// Counts one observing pass that did or did not see the query.
    pub(crate) fn count(&mut self, seen: bool, config: &StoreConfig) {
        if seen {
            self.sightings = self
                .sightings
                .saturating_add(1)
                .min(config.alias_confirm_passes);
            self.missed = 0;
        } else {
            self.missed = self.missed.saturating_add(1);
            if self.missed >= config.alias_stale_passes {
                self.sightings = 0;
            }
        }
        self.frames = self.sightings >= config.alias_confirm_passes;
    }
}

impl SubjectRecord {
    /// Counts one observing pass in the evidence of every query the subject
    /// answers, `seen` being the queries the pass saw on it, and drops the
    /// entries of queries it no longer answers.
    pub(crate) fn count_labels(&mut self, seen: Option<&BTreeSet<QueryId>>, config: &StoreConfig) {
        let Self {
            subject,
            aliases,
            labels,
            ..
        } = self;
        labels.retain(|q, _| *q == subject.query || aliases.contains(q));
        for q in std::iter::once(&subject.query).chain(aliases.iter()) {
            let saw = seen.is_some_and(|s| s.contains(q));
            labels.entry(q.clone()).or_default().count(saw, config);
        }
    }
}

#[cfg(test)]
mod tests;
