//! Commands into the engine and their validated results.
//!
//! The engine thread is the only owner of switcher, subject, director and
//! output state. Everything else (the operator UI, a headless runner, tests,
//! later a control surface) changes that state by sending an
//! [`EngineCommand`]; the engine applies commands in arrival order at the
//! start of a tick, atomically with respect to rendering, and answers each
//! with a [`CommandReply`] or a [`CommandError`] that says exactly why it was
//! refused. A refused command changes nothing.

use std::path::PathBuf;

use ac_core::{Preset, QueryId, Rect, ShotId, SourceId, SubjectId};
use ac_director::{ContentProfile, DirectorMode};
use ac_subjects::StoreError;
use ac_switcher::{SwitchError, Transition};
use thiserror::Error;

/// A request to change engine state.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineCommand {
    /// Stage `shot` on preview.
    SetPreview {
        /// The shot.
        shot: ShotId,
    },
    /// Swap preview and program at once.
    Cut,
    /// Run the selected transition from program to preview.
    Auto,
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
    /// Add a prompt query; its id is a slug of `name`.
    AddQuery {
        /// Operator-facing name.
        name: String,
        /// Detector prompt.
        prompt: String,
        /// Framings for every matched subject.
        presets: Vec<Preset>,
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
    /// Stage the director's suggestion on preview and run Auto to it.
    AcceptSuggestion,
    /// Start the program encoder. Destinations default to the config's.
    StartOutput {
        /// RTMP URL with stream key.
        rtmp_url: Option<String>,
        /// MP4 recording path (must not exist yet).
        record_path: Option<PathBuf>,
    },
    /// Finish the program encoder (the recording is finalized).
    StopOutput,
    /// Stop everything and end the engine thread.
    Shutdown,
}

impl EngineCommand {
    /// Short name of the command, for logs and errors.
    pub fn name(&self) -> &'static str {
        match self {
            EngineCommand::SetPreview { .. } => "set_preview",
            EngineCommand::Cut => "cut",
            EngineCommand::Auto => "auto",
            EngineCommand::SetTBar { .. } => "set_tbar",
            EngineCommand::SetTransition { .. } => "set_transition",
            EngineCommand::AddQuery { .. } => "add_query",
            EngineCommand::RemoveQuery { .. } => "remove_query",
            EngineCommand::Pin { .. } => "pin",
            EngineCommand::Hide { .. } => "hide",
            EngineCommand::Reacquire { .. } => "reacquire",
            EngineCommand::SetDirectorMode { .. } => "set_director_mode",
            EngineCommand::SetDirectorProfile { .. } => "set_director_profile",
            EngineCommand::AcceptSuggestion => "accept_suggestion",
            EngineCommand::StartOutput { .. } => "start_output",
            EngineCommand::StopOutput => "stop_output",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_distinct_name() {
        let commands = [
            EngineCommand::SetPreview {
                shot: ShotId("a".into()),
            },
            EngineCommand::Cut,
            EngineCommand::Auto,
            EngineCommand::SetTBar { position: 0.5 },
            EngineCommand::SetTransition {
                transition: Transition::default(),
            },
            EngineCommand::AddQuery {
                name: "a".into(),
                prompt: "b".into(),
                presets: vec![],
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
            EngineCommand::AcceptSuggestion,
            EngineCommand::StartOutput {
                rtmp_url: None,
                record_path: None,
            },
            EngineCommand::StopOutput,
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
    }
}
