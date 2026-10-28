//! The director's side of a tick: program book-keeping, running the
//! [`Director`](ac_director::Director) and acting on its decision, and the
//! operator's hold on preview.
//!
//! The director only ever sees [`ShotCandidate`]s built from the catalog,
//! and acts through the same switcher calls as the operator, so every
//! availability and transition rule applies to it too. Per mode:
//!
//! - **Off**: nothing is staged.
//! - **Suggest**: a suggestion is put on preview only when it *changes*
//!   (edge-triggered), and never over the operator's hold (below).
//! - **Auto**: the suggestion is staged every tick and takes are executed;
//!   while the operator's switching pauses auto-takes
//!   ([`Takeover::Pause`](ac_director::Takeover::Pause)) it stages as in
//!   Suggest mode instead, hold included.
//!
//! # The operator's hold
//!
//! A shot the operator stages ([`EngineCommand::SetPreview`]) locks
//! preview against the director: in Suggest mode (and in a paused Auto)
//! the director never stages over it, however its suggestion changes,
//! until the operator takes the shot or the hold is cleared. Each
//! suggestion passed over this way spends its edge, as if staged, and is
//! counted with the newest kept for the snapshot
//! ([`DirectorView::held_back`](crate::snapshot::DirectorView::held_back)),
//! so the window can say what the director would have staged. The hold
//! covers a shot staged during a transition too: it waits in the
//! switcher's queue and lands on preview at the end, where it stays.
//!
//! The hold ends ([`EngineCore::refresh_hold`]) when its shot is on
//! program (taken, by any means), when neither the preview bus nor the
//! queue holds it any more (a Cut now, a take of the director in Auto mode
//! or an accepted suggestion replaced it), and on
//! [`EngineCommand::ReleasePreview`], a skip of the held shot or a mode
//! change. Then the director's edge is reset, so it stages its current
//! suggestion on its next tick. A non-paused Auto stages over a hold
//! (the director wins there, by the takeover setting or because the
//! operator chose Auto), which ends it.
//!
//! Staging is skipped during a transition. The switcher would queue it for
//! the transition's end, where it would replace a shot the operator staged
//! meanwhile; the director stages once the transition has ended instead:
//! on the tick that ends it, or on the next tick when a command (the T-bar
//! or Cut) ends it.
//!
//! [`EngineCommand::SetPreview`]: crate::command::EngineCommand::SetPreview
//! [`EngineCommand::ReleasePreview`]: crate::command::EngineCommand::ReleasePreview

use std::time::Duration;

use ac_core::ShotId;
use ac_director::{DirectorInput, DirectorMode, ShotCandidate};
use ac_switcher::TransitionKind;

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

    /// Ends the operator's hold on preview once its shot is on program or
    /// no longer staged (see the module docs).
    pub(super) fn refresh_hold(&mut self) {
        let Some(held) = &self.preview_hold else {
            return;
        };
        let staged = self.switcher.preview() == Some(held)
            || self.switcher.queued().is_some_and(|q| &q.shot == held);
        if self.switcher.program() == Some(held) || !staged {
            self.release_hold();
        }
    }

    /// Ends the operator's hold, so the director stages its suggestion on
    /// its next tick.
    pub(super) fn release_hold(&mut self) {
        if self.preview_hold.take().is_some() {
            self.last_staged = None;
        }
    }

    /// Runs the director and acts on its decision for the current mode.
    pub(super) fn run_director(&mut self) {
        self.refresh_hold();
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
        let paused = self.director.paused(self.now).is_some();
        match self.director.config().mode {
            DirectorMode::Off => self.last_staged = None,
            DirectorMode::Auto if !paused => {
                if let Some(s) = &decision.suggestion
                    && self.stage(s, busy)
                {
                    // So a drop to Suggest does not restage it.
                    self.last_staged = Some(s.clone());
                }
                if let Some(take) = &decision.take {
                    self.director_takes += 1;
                    self.take(
                        &take.shot,
                        take.transition == ac_director::TransitionHint::Cut,
                    );
                }
            }
            DirectorMode::Suggest | DirectorMode::Auto => {
                self.suggest(decision.suggestion.as_ref(), busy);
            }
        }
        self.decision = decision;
        self.refresh_hold();
    }

    /// Suggest-mode staging of `suggestion`: only when it changed, and
    /// never over the operator's hold (see the module docs).
    fn suggest(&mut self, suggestion: Option<&ShotId>, busy: bool) {
        if suggestion == self.last_staged.as_ref() {
            return;
        }
        let Some(s) = suggestion else {
            self.last_staged = None;
            return;
        };
        if let Some(held) = &self.preview_hold {
            if held != s {
                self.held_back += 1;
                self.last_held_back = Some(s.clone());
            }
            self.last_staged = Some(s.clone());
        } else if self.stage(s, busy) {
            self.last_staged = Some(s.clone());
        }
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
