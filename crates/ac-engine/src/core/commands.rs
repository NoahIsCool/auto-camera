//! Applying operator commands to the core.
//!
//! Every command is validated before anything changes, so a refused command
//! leaves the core exactly as it was. Switcher commands use the catalog's
//! health guard: a shot that does not exist or whose source has no fresh
//! frames is never taken to program. Output and shutdown commands belong to
//! the runtime, which owns the encoder and the threads.
//!
//! A running transition does not make switching commands fail
//! ([`ac_switcher`] has the rules, the T-bar's included): staging and Auto
//! are queued for its end, Cut cuts it short, landing its incoming shot,
//! and Cut now ([`EngineCommand::CutTo`]) puts a named shot on air in one
//! command, so no tick can render the incoming shot alone between a Cut
//! and a cut to the named shot. A queued take is accepted when it is
//! pressed and checked again when it fires, a tick or more later, where no
//! reply is waiting; a take dropped there is counted in the snapshot with
//! its reason ([`crate::snapshot::SwitcherView::last_dropped`]), and the
//! window shows it as it shows a refused command.
//!
//! Cut and Auto are fenced: they carry the preview their sender saw and are
//! refused, before anything else is checked, when the preview bus shows
//! anything else ([`crate::command`]); accepting a suggestion is refused
//! the same way when the director suggests anything else. Staging a shot
//! puts the operator's hold on preview ([`super::directing`]); Skip and
//! ReleasePreview end it, Skip also leaving the shot out of the director's
//! suggestions for its window.
//!
//! The operator's own switching (staging, Cut, Cut now, Auto, the T-bar)
//! is also a manual action for the director: once it has been applied, a
//! director in Auto mode takes over as its takeover setting says
//! ([`ac_director::Director::take_over`]), and the reply says what that
//! did ([`CommandReply::TookOver`]). A refused action takes nothing over,
//! as it changes nothing. Accepting or skipping a suggestion follows the
//! director rather than overriding it, so neither is a manual action.
//!
//! Protection (on-air subjects are never removed) is refreshed before and
//! after every command, not only at the start of a tick: a cut followed at
//! once by removing the query must keep the subject that was just cut to
//! program, and so must a removal right after the director took a shot in
//! the middle of the previous tick.

use ac_core::ShotId;
use ac_director::DirectorConfig;

use super::EngineCore;
use crate::command::{CommandError, CommandReply, EngineCommand};
use crate::config::slug;
use crate::queries::build_query;

impl EngineCore {
    /// Applies `cmd`; see the module docs.
    pub fn apply(&mut self, cmd: &EngineCommand) -> Result<CommandReply, CommandError> {
        use EngineCommand as C;
        self.update_protection();
        let manual = match cmd {
            C::SetPreview { shot } => {
                if self.catalog.get(shot).is_none() {
                    return Err(CommandError::UnknownShot(shot.clone()));
                }
                self.switcher.set_preview(shot.clone());
                self.preview_hold = Some(shot.clone());
                true
            }
            C::Cut { preview } => {
                self.fence(preview)?;
                let catalog = &self.catalog;
                self.switcher.cut(|id| catalog.is_available(id))?;
                true
            }
            C::CutTo { shot } => {
                if self.catalog.get(shot).is_none() {
                    return Err(CommandError::UnknownShot(shot.clone()));
                }
                let catalog = &self.catalog;
                self.switcher
                    .cut_to(shot.clone(), |id| catalog.is_available(id))?;
                true
            }
            C::Auto { preview } => {
                self.fence(preview)?;
                let catalog = &self.catalog;
                self.switcher.auto(|id| catalog.is_available(id))?;
                true
            }
            C::SetTBar { position } => {
                if !position.is_finite() {
                    return Err(CommandError::Invalid(format!(
                        "T-bar position {position} is not a number"
                    )));
                }
                let catalog = &self.catalog;
                self.switcher
                    .set_tbar(*position, |id| catalog.is_available(id))?;
                true
            }
            C::SetTransition { transition } => {
                self.switcher.set_transition(*transition)?;
                false
            }
            C::AddQuery {
                name,
                prompt,
                presets,
                literal,
            } => return self.add_query(name, prompt, presets.clone(), *literal),
            C::RemoveQuery { query } => {
                let Some(pos) = self.queries.iter().position(|q| &q.id == query) else {
                    return Err(CommandError::UnknownQuery(query.clone()));
                };
                // Removed first, so results already in flight are filtered.
                self.queries.remove(pos);
                self.priorities.remove(query);
                for s in &mut self.sources {
                    s.vision.remove_query(query);
                    s.discovery.matches.remove(query);
                }
                false
            }
            C::Pin {
                source,
                subject,
                pinned,
            } => {
                let i = self.require_source(source)?;
                self.sources[i].vision.set_pinned(*subject, *pinned)?;
                false
            }
            C::Hide {
                source,
                subject,
                hidden,
            } => {
                let i = self.require_source(source)?;
                self.sources[i].vision.set_hidden(*subject, *hidden)?;
                false
            }
            C::Reacquire {
                source,
                subject,
                bbox,
            } => {
                let i = self.require_source(source)?;
                let s = &mut self.sources[i];
                let frame = s.frame.clone().ok_or_else(|| {
                    CommandError::Invalid(format!("source {:?} has no frame yet", source.0))
                })?;
                s.vision.reacquire(*subject, *bbox, &frame)?;
                false
            }
            C::SetDirectorMode { mode } => {
                self.director.set_mode(*mode);
                self.preview_hold = None;
                self.last_staged = None;
                false
            }
            C::SetDirectorProfile { profile } => {
                let current = self.director.config();
                let config = DirectorConfig {
                    takeover: current.takeover,
                    pause: current.pause,
                    skip: current.skip,
                    ..DirectorConfig::new(current.mode, *profile)
                };
                self.director
                    .set_config(config)
                    .map_err(|e| CommandError::Invalid(e.to_string()))?;
                false
            }
            C::AcceptSuggestion { shot } => {
                match &self.decision.suggestion {
                    None => return Err(CommandError::NoSuggestion),
                    Some(now) if now != shot => {
                        return Err(CommandError::SuggestionChanged {
                            seen: shot.clone(),
                            suggestion: now.clone(),
                        });
                    }
                    Some(_) => self.accept(shot.clone())?,
                }
                false
            }
            C::Skip { shot } => {
                if self.catalog.get(shot).is_none() {
                    return Err(CommandError::UnknownShot(shot.clone()));
                }
                self.director.skip(shot.clone(), self.now);
                if self.preview_hold.as_ref() == Some(shot) {
                    self.release_hold();
                }
                false
            }
            C::ReleasePreview => {
                self.release_hold();
                false
            }
            C::StartOutput { .. } | C::StopOutput | C::Shutdown => {
                return Err(CommandError::Invalid(format!(
                    "{} is handled by the engine runtime",
                    cmd.name()
                )));
            }
        };
        let reply = match manual.then(|| self.director.take_over(self.now)).flatten() {
            Some(handover) => CommandReply::TookOver(handover),
            None => CommandReply::Done,
        };
        self.refresh_hold();
        self.update_protection();
        self.note_program();
        Ok(reply)
    }

    /// Refuses a take whose sender saw `seen` on preview when the preview
    /// bus shows anything else (see the module docs).
    fn fence(&self, seen: &Option<ShotId>) -> Result<(), CommandError> {
        let preview = self.switcher.preview();
        if preview == seen.as_ref() {
            Ok(())
        } else {
            Err(CommandError::PreviewChanged {
                seen: seen.clone(),
                preview: preview.cloned(),
            })
        }
    }

    /// Reads the prompt once with the studio's reader (its lexicon and
    /// what its detector sees), settles the presets ([`crate::queries`])
    /// and adds the query, ranked as if listed last
    /// ([`crate::config::Ranking::appended`]).
    fn add_query(
        &mut self,
        name: &str,
        prompt: &str,
        presets: Option<Vec<ac_core::Preset>>,
        literal: bool,
    ) -> Result<CommandReply, CommandError> {
        let id = ac_core::QueryId(slug(name));
        let query = build_query(id.clone(), name, prompt, presets, literal, &self.prompts)
            .map_err(CommandError::Invalid)?;
        if self.queries.iter().any(|q| q.id == id) {
            return Err(CommandError::DuplicateQuery(id));
        }
        let last = self.queries.last().and_then(|q| self.priorities.get(&q.id));
        let priority = self.ranking.appended(last.copied());
        self.queries.push(query);
        self.priorities.insert(id.clone(), priority);
        Ok(CommandReply::QueryAdded { id })
    }

    /// Stages `shot` on preview and runs Auto to it; during a transition,
    /// queues both for its end, unless the transition brings `shot` in
    /// already. The operator's hold on preview ends: they chose the
    /// director's shot over it.
    fn accept(&mut self, shot: ShotId) -> Result<(), CommandError> {
        if !self.catalog.is_available(&shot) {
            return Err(CommandError::Switch(ac_switcher::SwitchError::Unavailable(
                shot,
            )));
        }
        self.release_hold();
        self.switcher.set_preview(shot);
        let catalog = &self.catalog;
        self.switcher.auto(|id| catalog.is_available(id))?;
        Ok(())
    }

    /// Index of `source`, or [`CommandError::UnknownSource`].
    fn require_source(&self, source: &ac_core::SourceId) -> Result<usize, CommandError> {
        self.source_index(source)
            .ok_or_else(|| CommandError::UnknownSource(source.clone()))
    }
}
