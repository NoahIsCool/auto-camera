//! Applying operator commands to the core.
//!
//! Every command is validated before anything changes, so a refused command
//! leaves the core exactly as it was. Switcher commands use the catalog's
//! health guard: a shot that does not exist or whose source has no fresh
//! frames is never taken to program. Output and shutdown commands belong to
//! the runtime, which owns the encoder and the threads.
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
use crate::config::{QuerySettings, slug};
use crate::queries::build_query;

impl EngineCore {
    /// Applies `cmd`; see the module docs.
    pub fn apply(&mut self, cmd: &EngineCommand) -> Result<CommandReply, CommandError> {
        use EngineCommand as C;
        self.update_protection();
        match cmd {
            C::SetPreview { shot } => {
                if self.catalog.get(shot).is_none() {
                    return Err(CommandError::UnknownShot(shot.clone()));
                }
                self.switcher.set_preview(shot.clone())?;
            }
            C::Cut => {
                let catalog = &self.catalog;
                self.switcher.cut(|id| catalog.is_available(id))?;
            }
            C::Auto => {
                let catalog = &self.catalog;
                self.switcher.auto(|id| catalog.is_available(id))?;
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
            }
            C::SetTransition { transition } => self.switcher.set_transition(*transition)?,
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
            }
            C::Pin {
                source,
                subject,
                pinned,
            } => {
                let i = self.require_source(source)?;
                self.sources[i].vision.set_pinned(*subject, *pinned)?;
            }
            C::Hide {
                source,
                subject,
                hidden,
            } => {
                let i = self.require_source(source)?;
                self.sources[i].vision.set_hidden(*subject, *hidden)?;
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
            }
            C::SetDirectorMode { mode } => {
                self.director.set_mode(*mode);
                self.last_staged = None;
            }
            C::SetDirectorProfile { profile } => {
                let config = DirectorConfig::new(self.director.config().mode, *profile);
                self.director
                    .set_config(config)
                    .map_err(|e| CommandError::Invalid(e.to_string()))?;
            }
            C::AcceptSuggestion => {
                let shot = self
                    .decision
                    .suggestion
                    .clone()
                    .ok_or(CommandError::NoSuggestion)?;
                self.accept(shot)?;
            }
            C::StartOutput { .. } | C::StopOutput | C::Shutdown => {
                return Err(CommandError::Invalid(format!(
                    "{} is handled by the engine runtime",
                    cmd.name()
                )));
            }
        }
        self.update_protection();
        self.note_program();
        Ok(CommandReply::Done)
    }

    /// Reads the prompt once with the studio's reader (its lexicon and
    /// what its detector sees), settles the presets ([`crate::queries`])
    /// and adds the query, at the default priority.
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
        self.queries.push(query);
        self.priorities
            .insert(id.clone(), QuerySettings::DEFAULT_PRIORITY);
        Ok(CommandReply::QueryAdded { id })
    }

    /// Stages `shot` on preview and runs Auto to it.
    fn accept(&mut self, shot: ShotId) -> Result<(), CommandError> {
        if !self.catalog.is_available(&shot) {
            return Err(CommandError::Switch(ac_switcher::SwitchError::Unavailable(
                shot,
            )));
        }
        if self.switcher.preview() != Some(&shot) {
            self.switcher.set_preview(shot)?;
        }
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
