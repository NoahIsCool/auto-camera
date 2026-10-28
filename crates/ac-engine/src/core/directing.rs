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
//! score ([`ac_director::candidate`]): how many levels the priority of its
//! subject's primary query stands above the *floor*, the lowest priority
//! among the primary queries of the subjects that have shots.
//!
//! - **The subject's primary query, not its aliases.** Priority belongs to
//!   the subject, so all its shots carry one level, whichever query's
//!   preset made each of them. Aliases do not raise it: the store adds an
//!   alias on one overlapping box and keeps it until the subject retires,
//!   so one stray low-score "pastor" box on a band member would otherwise
//!   put the band member level with the pastor for the rest of the show,
//!   the "wrong subject" failure priority is there to help with. The cost
//!   is the other way round and milder: a subject found first through a
//!   low-priority prompt stays at that prompt's level when a higher one
//!   also finds it, and so loses a bonus rather than wins one wrongly.
//!   When the operator removes the primary query and the store promotes an
//!   alias, the level follows the new primary query.
//! - **The floor counts queries that have subjects.** Wides, and orphans
//!   (subjects whose primary query was removed, kept while on air), stand
//!   at the floor, level 0. A query that finds no one, such as one just
//!   added, does not move the floor, so it cannot shift every other
//!   prompt's shots against the wides. The floor moves when subjects of a
//!   lower-priority query appear or go, lost ones included until they
//!   retire.
//!
//! Counting from the floor makes only differences matter: with every query
//! at one priority, or only one query's subjects on screen, every level is
//! 0 and the director scores exactly as it did before it weighed
//! priorities.

use std::time::Duration;

use ac_core::ShotId;
use ac_director::{DirectorInput, DirectorMode, ShotCandidate};
use ac_switcher::TransitionKind;

use super::EngineCore;
use crate::catalog::ShotEntry;

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
        let entries = self.catalog.entries();
        let priorities: Vec<Option<u8>> =
            entries.iter().map(|e| self.subject_priority(e)).collect();
        let floor = priorities.iter().flatten().min().copied().unwrap_or(0);
        entries
            .iter()
            .zip(priorities)
            .map(|(e, priority)| ShotCandidate {
                id: e.shot.id.clone(),
                source: self.sources[e.source].id.clone(),
                kind: (&e.shot.target).into(),
                crop: e.crop,
                quality: e.quality,
                available: e.available,
                recently_used_at: e.last_on_program,
                priority: priority.map_or(0, |p| p.saturating_sub(floor)),
            })
            .collect()
    }

    /// The priority of the primary query of shot `e`'s subject; `None` for
    /// a wide, and for an orphan, whose primary query is no longer live.
    fn subject_priority(&self, e: &ShotEntry) -> Option<u8> {
        let record = self.sources[e.source].vision.store().record(e.subject?)?;
        self.priorities.get(&record.subject.query).copied()
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
