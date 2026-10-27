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
//! Staging is skipped during a transition. The switcher would queue it for
//! the transition's end, where it would replace a shot the operator staged
//! meanwhile; the director stages once the transition has ended instead:
//! on the tick that ends it, or on the next tick when a command (the T-bar
//! or Cut) ends it. A shot the operator staged during the transition lands
//! on preview at that end, before the director runs, and the program
//! change there often changes the suggestion too. In Suggest mode the
//! landing counts as the operator staging over the suggestion
//! ([`EngineCore::note_landing`]): the director's next suggestion is marked
//! staged without being staged, so the edge is spent on the operator's
//! choice and the director stages again only when its suggestion changes
//! after that, as after a shot staged by hand. (Auto mode stages every
//! tick regardless.)

use std::time::Duration;

use ac_core::ShotId;
use ac_director::{DirectorInput, DirectorMode, ShotCandidate};
use ac_switcher::{Queued, TransitionKind};

use super::EngineCore;

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

    /// Notes whether `queued`, the press waiting before a switcher call,
    /// was a shot the operator staged that the call landed on preview at the
    /// end of its transition, with no take of it running (see the module
    /// docs).
    pub(super) fn note_landing(&mut self, queued: Option<Queued>) {
        let landed = queued.is_some_and(|q| {
            self.switcher.queued().is_none()
                && !self.switcher.in_transition()
                && self.switcher.preview() == Some(&q.shot)
        });
        self.operator_staged |= landed;
    }

    /// Runs the director and acts on its decision for the current mode.
    pub(super) fn run_director(&mut self) {
        let operator_staged = std::mem::take(&mut self.operator_staged);
        let candidates: Vec<ShotCandidate> = self
            .catalog
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
            })
            .collect();
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
                if operator_staged {
                    self.last_staged = decision.suggestion.clone();
                } else if decision.suggestion != self.last_staged {
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

    /// Puts `shot` on preview unless it is there; false, changing nothing,
    /// while `busy` with a transition (see the module docs).
    pub(super) fn stage(&mut self, shot: &ShotId, busy: bool) -> bool {
        if self.switcher.preview() == Some(shot) {
            return true;
        }
        if busy {
            return false;
        }
        self.switcher.set_preview(shot.clone());
        true
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
