//! Exporting the show: what the operator built, as a [`Show`] value
//! ([`crate::show`]).
//!
//! The export only reads. Most of a show is state the core owns and the
//! operator's commands change: the queries (with their presets and
//! priorities), the director's mode and profile, and the selected
//! transition. The rest no command changes, so the core keeps it as the
//! config gave it ([`ShowBase`]): the sources (their paths resolved) and
//! the output's default destinations, which the runtime's output
//! controller also starts from.
//!
//! # Queries
//!
//! Each query is written so that laying the show over its studio file
//! builds the same query again ([`crate::config::QuerySettings::build`]):
//!
//! * its id only when it is not the [`slug`] of its name, which is the id
//!   a query without one gets;
//! * its prompt and literal flag, so the prompt is read as it was;
//! * its presets, always, a standard preset ([`preset_for`]) by its size
//!   alone: explicit presets win over the prompt's shot words, so the
//!   query keeps its framings even if the studio's lexicon changes what
//!   those words mean;
//! * its priority only when list order would not give it back. While the
//!   config ranks by list order ([`Ranking::ListOrder`]) and the order of
//!   the queries still yields every priority (as it does until a query
//!   other than the last is removed), no priority is written, so the
//!   reloaded studio ranks by list order too, a query added later
//!   included. Otherwise every query's priority is written, so each keeps
//!   its own; the reloaded studio then ranks by given priorities, and a
//!   query added after the restart gets the default rather than one below
//!   the last.

use ac_core::Preset;

use super::EngineCore;
use crate::config::{
    PresetSpec, QuerySettings, Ranking, SourceEntry, StudioConfig, preset_for, slug,
};
use crate::show::{SHOW_VERSION, Show, ShowDirector, ShowOutput, ShowSwitcher};

/// What the show says about the studio that no command changes, as the
/// config gave it; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ShowBase {
    /// The source entries, their paths resolved.
    sources: Vec<SourceEntry>,
    /// The output's default destinations.
    output: ShowOutput,
}

impl ShowBase {
    /// The parts of `config` the show holds and no command changes.
    pub(super) fn of(config: &StudioConfig) -> Self {
        Self {
            sources: config.sources.clone(),
            output: ShowOutput {
                rtmp_url: config.output.rtmp_url.clone(),
                record_path: config.output.record_path.clone(),
            },
        }
    }
}

impl EngineCore {
    /// The show as it stands: the sources, the queries as they would be
    /// written in a config (see the module docs), the director's mode and
    /// profile, the selected transition and the output's default
    /// destinations. `studio` is left for the caller to name.
    pub fn export_show(&self) -> Show {
        let director = self.director.config();
        let transition = self.switcher.transition();
        Show {
            show: SHOW_VERSION,
            studio: None,
            sources: Some(self.show_base.sources.clone()),
            queries: Some(self.exported_queries()),
            director: ShowDirector {
                mode: Some(director.mode),
                profile: Some(director.profile),
            },
            switcher: ShowSwitcher {
                transition: Some(transition.kind),
                duration: Some(transition.duration),
            },
            output: self.show_base.output.clone(),
        }
    }

    /// The live queries as config entries, in order; see the module docs.
    fn exported_queries(&self) -> Vec<QuerySettings> {
        let mut out: Vec<QuerySettings> = self
            .queries
            .iter()
            .map(|q| QuerySettings {
                id: (q.id.0 != slug(&q.name)).then(|| q.id.0.clone()),
                name: q.name.clone(),
                prompt: q.prompt.clone(),
                presets: Some(q.presets.iter().map(preset_spec).collect()),
                literal: q.parse.literal,
                priority: None,
            })
            .collect();
        let held: Vec<u8> = self
            .queries
            .iter()
            .map(|q| {
                self.priorities
                    .get(&q.id)
                    .copied()
                    .unwrap_or(QuerySettings::DEFAULT_PRIORITY)
            })
            .collect();
        let implicit =
            self.ranking == Ranking::ListOrder && Ranking::ListOrder.priorities(&out) == held;
        if !implicit {
            for (q, p) in out.iter_mut().zip(held) {
                q.priority = Some(p);
            }
        }
        out
    }
}

/// `preset` as a config writes it: by its size alone when it is that size's
/// standard preset, in full otherwise.
fn preset_spec(preset: &Preset) -> PresetSpec {
    if *preset == preset_for(preset.size) {
        PresetSpec::Size(preset.size)
    } else {
        PresetSpec::Full(preset.clone())
    }
}

#[cfg(test)]
mod tests;
