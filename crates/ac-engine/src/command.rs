//! Commands into the engine and their validated results.
//!
//! The engine thread is the only owner of switcher, subject, director and
//! output state. Everything else (the operator UI, a headless runner, tests,
//! later a control surface) changes that state by sending an
//! [`EngineCommand`]; the engine applies commands in arrival order at the
//! start of a tick, atomically with respect to rendering, and answers each
//! with a [`CommandReply`] or a [`CommandError`] that says exactly why it was
//! refused. A refused command changes nothing.
//!
//! # Fencing
//!
//! A take must put on air the shot the operator looked at, but the engine
//! may restage preview between the snapshot the operator saw and the
//! command's arrival (the director stages its suggestion every tick in Auto
//! mode). So [`EngineCommand::Cut`] and [`EngineCommand::Auto`] carry the
//! preview their sender saw, and are refused with
//! [`CommandError::PreviewChanged`] when the preview bus shows anything
//! else (spec C.1: fencing against stale commands). Accepting a suggestion
//! is fenced the same way: [`EngineCommand::AcceptSuggestion`] names the
//! suggestion its sender saw and is refused with
//! [`CommandError::SuggestionChanged`] when the director suggests anything
//! else by then. Cut now ([`EngineCommand::CutTo`]) names its shot, so it
//! needs no fence. The T-bar is not fenced: its first move starts a
//! transition to whatever preview holds.

use std::path::PathBuf;

use ac_core::{Preset, QueryId, Rect, ShotId, SourceId, SubjectId};
use ac_director::{ContentProfile, DirectorMode, Handover};
use ac_subjects::StoreError;
use ac_switcher::{SwitchError, Transition};
use thiserror::Error;

use crate::show::Show;

/// A request to change engine state.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineCommand {
    /// Stage `shot` on preview; during a transition, when it ends.
    SetPreview {
        /// The shot.
        shot: ShotId,
    },
    /// Swap preview and program at once; during a transition, cut it short,
    /// landing the shot it brings in. Refused unless preview shows
    /// `preview` (see the module docs).
    Cut {
        /// The preview the sender saw: the shot staged, or the shot a
        /// running transition brings in; `None` for an empty preview.
        preview: Option<ShotId>,
    },
    /// Cut `shot` straight to program ("Cut now"), whatever is staged and
    /// whether or not a transition runs, in one step.
    CutTo {
        /// The shot.
        shot: ShotId,
    },
    /// Run the selected transition from program to preview; during a
    /// transition, queue a take of the shot staged meanwhile for its end.
    /// Refused unless preview shows `preview`, as [`Self::Cut`].
    Auto {
        /// The preview the sender saw, as for [`Self::Cut`].
        preview: Option<ShotId>,
    },
    /// Move the T-bar to `position` in `0.0..=1.0`.
    SetTBar {
        /// Bar position.
        position: f64,
    },
    /// Select the transition used by Auto and the T-bar.
    SetTransition {
        /// The transition.
        transition: Transition,
    },
    /// Add a prompt query; its id is a slug of `name`. The prompt is read
    /// once, with the studio's lexicon and what its detector sees
    /// ([`crate::queries`]).
    AddQuery {
        /// Operator-facing name.
        name: String,
        /// The operator's prompt, such as "speaker upper body".
        prompt: String,
        /// Framings for every matched subject. `Some` always wins over the
        /// prompt's shot words; `None` takes one preset per shot size they
        /// name, or medium and close when they name none.
        presets: Option<Vec<Preset>>,
        /// Send the prompt to the detector as typed, without reading shot
        /// or role words.
        literal: bool,
    },
    /// Remove a query and the subjects only it matched.
    RemoveQuery {
        /// The query.
        query: QueryId,
    },
    /// Pin or unpin a subject (pinned subjects are never retired).
    Pin {
        /// The subject's source.
        source: SourceId,
        /// The subject.
        subject: SubjectId,
        /// New pin state.
        pinned: bool,
    },
    /// Hide or show a subject's shots.
    Hide {
        /// The subject's source.
        source: SourceId,
        /// The subject.
        subject: SubjectId,
        /// New hidden state.
        hidden: bool,
    },
    /// Operator correction: the subject is at `bbox` on the newest frame.
    Reacquire {
        /// The subject's source.
        source: SourceId,
        /// The subject, which keeps its id.
        subject: SubjectId,
        /// Its box in normalized source coordinates.
        bbox: Rect,
    },
    /// Change the auto-director's mode.
    SetDirectorMode {
        /// The mode.
        mode: DirectorMode,
    },
    /// Change the auto-director's content profile (resets its timing).
    SetDirectorProfile {
        /// The profile.
        profile: ContentProfile,
    },
    /// Stage the director's suggestion on preview and run Auto to it
    /// (queued like [`EngineCommand::Auto`] during a transition). Refused
    /// unless the director still suggests `shot` (see the module docs).
    AcceptSuggestion {
        /// The suggestion the sender saw.
        shot: ShotId,
    },
    /// Leave `shot` out of the director's suggestions for its skip window
    /// (`director.skip_s`); a hold the operator has on preview with it ends.
    Skip {
        /// The shot, normally the suggestion the sender saw.
        shot: ShotId,
    },
    /// End the operator's hold on preview, so the director stages its
    /// suggestion again ([`crate::core`] has the rules).
    ReleasePreview,
    /// Start the program encoder. Destinations default to the config's.
    StartOutput {
        /// RTMP URL with stream key.
        rtmp_url: Option<String>,
        /// MP4 recording path (must not exist yet).
        record_path: Option<PathBuf>,
    },
    /// Finish the program encoder (the recording is finalized).
    StopOutput,
    /// Export the show: the sources, queries, director mode and profile,
    /// transition and default recording path as they stand, never the
    /// stream key ([`crate::show`]). Changes nothing; the reply is
    /// [`CommandReply::Show`], which the sender saves.
    ExportShow,
    /// Stop everything and end the engine thread.
    Shutdown,
}

impl EngineCommand {
    /// Short name of the command, for logs and errors.
    pub fn name(&self) -> &'static str {
        match self {
            EngineCommand::SetPreview { .. } => "set_preview",
            EngineCommand::Cut { .. } => "cut",
            EngineCommand::CutTo { .. } => "cut_to",
            EngineCommand::Auto { .. } => "auto",
            EngineCommand::SetTBar { .. } => "set_tbar",
            EngineCommand::SetTransition { .. } => "set_transition",
            EngineCommand::AddQuery { .. } => "add_query",
            EngineCommand::RemoveQuery { .. } => "remove_query",
            EngineCommand::Pin { .. } => "pin",
            EngineCommand::Hide { .. } => "hide",
            EngineCommand::Reacquire { .. } => "reacquire",
            EngineCommand::SetDirectorMode { .. } => "set_director_mode",
            EngineCommand::SetDirectorProfile { .. } => "set_director_profile",
            EngineCommand::AcceptSuggestion { .. } => "accept_suggestion",
            EngineCommand::Skip { .. } => "skip",
            EngineCommand::ReleasePreview => "release_preview",
            EngineCommand::StartOutput { .. } => "start_output",
            EngineCommand::StopOutput => "stop_output",
            EngineCommand::ExportShow => "export_show",
            EngineCommand::Shutdown => "shutdown",
        }
    }
}

/// What an accepted command did.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandReply {
    /// Applied.
    Done,
    /// A query was added under this id.
    QueryAdded {
        /// The new query's id.
        id: QueryId,
    },
    /// The encoder is starting; progress shows in the snapshot.
    OutputStarting,
    /// The encoder is finishing; the final stats show in the snapshot.
    OutputStopping,
    /// Applied, and as the operator's own switching while the director ran
    /// in Auto mode, it took over as `director.takeover` says.
    TookOver(Handover),
    /// The show as it stands, for [`EngineCommand::ExportShow`] (boxed:
    /// the reply stays small).
    Show(Box<Show>),
}

/// Why a command was refused.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum CommandError {
    /// No shot has this id.
    #[error("unknown shot {0:?}")]
    UnknownShot(ShotId),
    /// No source has this id.
    #[error("unknown source {0:?}")]
    UnknownSource(SourceId),
    /// No query has this id.
    #[error("unknown query {0:?}")]
    UnknownQuery(QueryId),
    /// A query with this id exists already.
    #[error("query {0:?} already exists")]
    DuplicateQuery(QueryId),
    /// A command argument is invalid; the message says which.
    #[error("invalid command: {0}")]
    Invalid(String),
    /// The switcher refused.
    #[error("switcher: {0}")]
    Switch(#[from] SwitchError),
    /// The subject store refused.
    #[error("subjects: {0}")]
    Store(#[from] StoreError),
    /// The director has no suggestion to accept.
    #[error("the director has no suggestion")]
    NoSuggestion,
    /// The director's suggestion changed after the sender saw it, so it was
    /// not accepted (see the module docs).
    #[error("the suggestion changed to {suggestion:?} since you saw {seen:?}: nothing was taken")]
    SuggestionChanged {
        /// The suggestion the sender saw.
        seen: ShotId,
        /// The suggestion now.
        suggestion: ShotId,
    },
    /// Preview changed after the sender saw it, so the take was refused
    /// (see the module docs).
    #[error(
        "preview changed to {} since you saw {}: nothing was taken",
        bus(.preview),
        bus(.seen)
    )]
    PreviewChanged {
        /// The preview the sender saw.
        seen: Option<ShotId>,
        /// The preview it shows now.
        preview: Option<ShotId>,
    },
    /// The output is already running or starting.
    #[error("output is already {0}")]
    OutputBusy(&'static str),
    /// The output is not running.
    #[error("output is not running")]
    OutputIdle,
    /// The output could not be configured.
    #[error("output: {0}")]
    Output(String),
    /// The engine has shut down or did not answer in time.
    #[error("engine unavailable: {0}")]
    Unavailable(&'static str),
}

/// A preview bus in an error message: the shot, or "nothing".
fn bus(shot: &Option<ShotId>) -> String {
    match shot {
        Some(shot) => format!("{shot:?}"),
        None => "nothing".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_distinct_name() {
        let commands = [
            EngineCommand::SetPreview {
                shot: ShotId("a".into()),
            },
            EngineCommand::Cut { preview: None },
            EngineCommand::CutTo {
                shot: ShotId("a".into()),
            },
            EngineCommand::Auto { preview: None },
            EngineCommand::SetTBar { position: 0.5 },
            EngineCommand::SetTransition {
                transition: Transition::default(),
            },
            EngineCommand::AddQuery {
                name: "a".into(),
                prompt: "b".into(),
                presets: None,
                literal: false,
            },
            EngineCommand::RemoveQuery {
                query: QueryId("a".into()),
            },
            EngineCommand::Pin {
                source: SourceId("s".into()),
                subject: SubjectId(1),
                pinned: true,
            },
            EngineCommand::Hide {
                source: SourceId("s".into()),
                subject: SubjectId(1),
                hidden: true,
            },
            EngineCommand::Reacquire {
                source: SourceId("s".into()),
                subject: SubjectId(1),
                bbox: Rect::FULL,
            },
            EngineCommand::SetDirectorMode {
                mode: DirectorMode::Auto,
            },
            EngineCommand::SetDirectorProfile {
                profile: ContentProfile::Music,
            },
            EngineCommand::AcceptSuggestion {
                shot: ShotId("a".into()),
            },
            EngineCommand::Skip {
                shot: ShotId("a".into()),
            },
            EngineCommand::ReleasePreview,
            EngineCommand::StartOutput {
                rtmp_url: None,
                record_path: None,
            },
            EngineCommand::StopOutput,
            EngineCommand::ExportShow,
            EngineCommand::Shutdown,
        ];
        let names: std::collections::HashSet<_> = commands.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), commands.len());
    }

    #[test]
    fn errors_render_readably() {
        let e = CommandError::from(SwitchError::NoPreview);
        assert_eq!(e.to_string(), "switcher: no shot on preview");
        assert!(
            CommandError::UnknownShot(ShotId("x".into()))
                .to_string()
                .contains('x')
        );
        let changed = CommandError::PreviewChanged {
            seen: Some(ShotId("mine".into())),
            preview: None,
        };
        assert_eq!(
            changed.to_string(),
            "preview changed to nothing since you saw ShotId(\"mine\"): nothing was taken"
        );
        let changed = CommandError::SuggestionChanged {
            seen: ShotId("a".into()),
            suggestion: ShotId("b".into()),
        };
        assert_eq!(
            changed.to_string(),
            "the suggestion changed to ShotId(\"b\") since you saw ShotId(\"a\"): nothing was taken"
        );
    }
}
