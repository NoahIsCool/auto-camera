//! The director's side of a tick: program book-keeping, running the
//! [`Director`](ac_director::Director) and acting on its decision.
//!
//! The director only ever sees [`ShotCandidate`]s built from the catalog,
//! and acts through the same switcher calls as the operator, so every
//! availability and transition rule applies to it too. Per mode:
//!
//! - **Off**: nothing is staged.
//! - **Suggest**: a suggestion is put on preview only when it *changes*
//!   (edge-triggered), so an operator who stages something else is not
//!   overridden every tick.
//! - **Auto**: the suggestion is staged every tick and takes are executed.
//!
//! Staging is skipped during a transition, since the switcher refuses to
//! change preview while one runs.
//!
//! # Priority
//!
//! Each candidate carries a priority level, the `p` of the director's
//! score ([`ac_director::candidate`]): how many levels the highest-priority
//! live query its subject answers, as primary query or alias, stands above
//! the lowest-priority live query. Priority belongs to the subject, not to
//! the query whose preset made a shot: a person found by both "person" and
//! "pastor" is the pastor in every size, so all their shots carry the
//! pastor's level. Wides, and subjects whose queries were all removed (an
//! orphan kept on air), stand with the lowest, at 0. Counting from the
//! lowest live query makes only differences matter: with every query at
//! one priority every level is 0 and the director scores exactly as without
//! priorities, and removing or adding a query (live, at the default
//! priority) moves the floor with it.

use std::collections::HashMap;
use std::time::Duration;

use ac_core::{QueryId, ShotId};
use ac_director::{DirectorInput, DirectorMode, ShotCandidate};
use ac_switcher::TransitionKind;

use super::EngineCore;
use crate::catalog::ShotEntry;
use crate::config::QuerySettings;

impl EngineCore {
    /// Book-keeps program changes: on-air time, last use and change count.
    pub(super) fn note_program(&mut self) {
        let current = self.switcher.program().cloned();
        if current != self.program {
            if let Some(old) = &self.program {
                self.catalog.mark_on_program(old, self.now);
            }
            self.program = current;
            self.program_since = self.now;
            self.program_changes += 1;
        }
        if let Some(p) = &self.program {
            self.catalog.mark_on_program(p, self.now);
        }
    }

    /// The director's view of the catalog: one candidate per shot, in
    /// display order, with its priority level (see the module docs).
    pub(super) fn director_candidates(&self) -> Vec<ShotCandidate> {
        let levels = self.priority_levels();
        self.catalog
            .entries()
            .iter()
            .map(|e| ShotCandidate {
                id: e.shot.id.clone(),
                source: self.sources[e.source].id.clone(),
                kind: (&e.shot.target).into(),
                crop: e.crop,
                quality: e.quality,
                available: e.available,
                recently_used_at: e.last_on_program,
                priority: self.shot_priority(e, &levels),
            })
            .collect()
    }

    /// How many levels each live query's priority stands above the lowest
    /// live query's.
    fn priority_levels(&self) -> HashMap<&QueryId, u8> {
        let priority = |id: &QueryId| {
            self.priorities
                .get(id)
                .copied()
                .unwrap_or(QuerySettings::DEFAULT_PRIORITY)
        };
        let lowest = self
            .queries
            .iter()
            .map(|q| priority(&q.id))
            .min()
            .unwrap_or_default();
        self.queries
            .iter()
            .map(|q| (&q.id, priority(&q.id) - lowest))
            .collect()
    }

    /// The level of shot `e`: that of the highest-priority live query its
    /// subject answers, 0 for a wide or a subject that answers none.
    fn shot_priority(&self, e: &ShotEntry, levels: &HashMap<&QueryId, u8>) -> u8 {
        let Some(subject) = e.subject else {
            return 0;
        };
        self.sources[e.source]
            .vision
            .store()
            .record(subject)
            .and_then(|r| r.queries().filter_map(|q| levels.get(q).copied()).max())
            .unwrap_or(0)
    }

    /// Runs the director and acts on its decision for the current mode.
    pub(super) fn run_director(&mut self) {
        let candidates = self.director_candidates();
        let input = DirectorInput {
            now: self.now,
            program: self.switcher.program(),
            preview: self.switcher.preview(),
            program_on_air: if self.program.is_some() {
                self.now.saturating_sub(self.program_since)
            } else {
                Duration::ZERO
            },
            in_transition: self.switcher.in_transition(),
            candidates: &candidates,
        };
        let decision = self.director.tick(&input);
        let busy = self.switcher.in_transition();
        match self.director.config().mode {
            DirectorMode::Off => self.last_staged = None,
            DirectorMode::Suggest => {
                if decision.suggestion != self.last_staged {
                    match &decision.suggestion {
                        Some(s) if self.stage(s, busy) => self.last_staged = Some(s.clone()),
                        Some(_) => {}
                        None => self.last_staged = None,
                    }
                }
            }
            DirectorMode::Auto => {
                if let Some(s) = &decision.suggestion {
                    self.stage(s, busy);
                }
                if let Some(take) = &decision.take {
                    self.director_takes += 1;
                    self.take(
                        &take.shot,
                        take.transition == ac_director::TransitionHint::Cut,
                    );
                }
            }
        }
        self.decision = decision;
    }

    /// Puts `shot` on preview unless it is there; false if that failed.
    pub(super) fn stage(&mut self, shot: &ShotId, busy: bool) -> bool {
        if self.switcher.preview() == Some(shot) {
            return true;
        }
        !busy && self.switcher.set_preview(shot.clone()).is_ok()
    }

    /// Takes `shot` to program: a cut, or the selected transition.
    pub(super) fn take(&mut self, shot: &ShotId, cut: bool) -> bool {
        if !self.stage(shot, self.switcher.in_transition()) {
            return false;
        }
        let catalog = &self.catalog;
        let available = |id: &ShotId| catalog.is_available(id);
        let result = if cut || self.switcher.transition().kind == TransitionKind::Cut {
            self.switcher.cut(available)
        } else {
            self.switcher.auto(available)
        };
        result.is_ok()
    }
}
