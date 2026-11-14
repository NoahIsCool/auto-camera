//! The close guard: during a show, closing the window or stopping the
//! output asks first.
//!
//! # Theory
//!
//! Closing the window shuts the engine down, which ends the stream and
//! finishes the recording; the Stop button does the same to the output
//! alone. During a service neither can be undone (spec C.3 asks for two
//! hours without a restart), and both are one stray click away: the
//! window's close button, the system's close shortcut, Stop where Start
//! was. So while the output is live ([`output_live`]: starting or running,
//! with the engine alive) a close request is cancelled
//! (`ViewportCommand::CancelClose`) and becomes a [`Question`], and so does
//! Stop; only the operator's confirmation goes ahead. With no live output
//! there is nothing to lose, and both act at once. That also holds for a
//! question still open when the output ends (the stream fails, the
//! recording finishes, the engine dies): it is dropped, since its words
//! ("the output is live") are no longer true, and the request it held does
//! not act later on its own. The window never closes by itself because a
//! stream failed; the operator closes again, and that acts at once.
//!
//! A termination signal (Ctrl-C, `SIGTERM`, `SIGHUP`; [`crate::signals`])
//! is not a stray click: it comes from a terminal or a service manager that
//! expects the app to quit. The window closes itself through the same
//! close request, and the guard lets every close request through once a
//! signal came, without asking.
//!
//! [`CloseGuard`] is a plain state machine: each frame it is told the
//! facts (a close was requested, the output is live, a signal came, the
//! operator answered) and says what to do; [`show`] draws its question as
//! a modal dialog. Neither talks to the engine or the window directly.
//!
//! # Invariants
//!
//! * While the output is live and no signal came, a close request goes
//!   through only after the operator confirmed closing.
//! * At most one question is open. A close request replaces an open Stop
//!   question (closing stops the output too); Stop never replaces a close
//!   question.
//! * A question is open only while the output is live: either is dropped
//!   once it is not (there is nothing left to lose), unanswered, so a
//!   dropped close request never closes the window later. A signal drops
//!   either too, since the window is closing anyway.
//! * Once closing was confirmed, every later close request goes through:
//!   the window's own `Close` command comes back as a close request on the
//!   next frame, and must not be asked about again.
//! * Cancelling (the safe answer, also Escape and a click beside the
//!   dialog) changes nothing.

use ac_engine::{EngineCommand, EngineSnapshot, OutputState};
use eframe::egui::{self, Color32, RichText};

/// The safe answer's button label.
pub const KEEP_RUNNING: &str = "Keep running";

/// Width of the dialog's text, in points.
const DIALOG_WIDTH: f32 = 340.0;

/// Colour of the confirming button's text.
const DANGER_RED: Color32 = super::theme::RED_TEXT;

/// True while the output has something to lose: the engine runs and the
/// output is starting or running. A finishing output (`Stopping`) is
/// already ending, and shutting down waits for it.
pub fn output_live(snap: &EngineSnapshot) -> bool {
    snap.running
        && matches!(
            snap.output.state,
            OutputState::Starting | OutputState::Running
        )
}

/// What the operator is asked to confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Question {
    /// Close the window, which stops the output.
    Close,
    /// Stop the output.
    Stop,
}

impl Question {
    /// The dialog's heading.
    pub fn title(self) -> &'static str {
        match self {
            Self::Close => "Close the window?",
            Self::Stop => "Stop the output?",
        }
    }

    /// The dialog's explanation.
    pub fn text(self) -> &'static str {
        match self {
            Self::Close => {
                "The output is live. Closing the window ends the stream \
                 and finishes the recording."
            }
            Self::Stop => "This ends the stream and finishes the recording.",
        }
    }

    /// The confirming button's label.
    pub fn confirm(self) -> &'static str {
        match self {
            Self::Close => "Stop output and close",
            Self::Stop => "Stop output",
        }
    }
}

/// The operator's answer to a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead.
    Confirm,
    /// Keep running.
    Cancel,
}

/// What to do with a close request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Let the window close.
    Close,
    /// Keep it open: send `ViewportCommand::CancelClose`.
    KeepOpen,
}

/// Asks before closing or stopping a live output; see the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloseGuard {
    /// The open question, if any.
    asking: Option<Question>,
    /// The operator confirmed closing.
    close_confirmed: bool,
}

impl CloseGuard {
    /// The open question, if any.
    pub fn question(&self) -> Option<Question> {
        self.asking
    }

    /// Takes in this frame's facts before anything else: whether the output
    /// is `live` and whether a termination signal came (`quitting`). Either
    /// fact drops the open question unanswered: without a live output there
    /// is nothing to ask about, and with a signal the window is closing.
    pub fn observe(&mut self, live: bool, quitting: bool) {
        if quitting || !live {
            self.asking = None;
        }
    }

    /// A close request arrived; asks about it unless it may go through.
    pub fn close_requested(&mut self, live: bool, quitting: bool) -> Verdict {
        if quitting || self.close_confirmed || !live {
            return Verdict::Close;
        }
        self.asking = Some(Question::Close);
        Verdict::KeepOpen
    }

    /// The Stop button was clicked: asks while the output is live,
    /// otherwise the command to send at once.
    pub fn stop_requested(&mut self, live: bool) -> Option<EngineCommand> {
        if !live {
            return Some(EngineCommand::StopOutput);
        }
        self.asking.get_or_insert(Question::Stop);
        None
    }

    /// The operator answered the open question; the question they
    /// confirmed, which the caller then carries out.
    pub fn answer(&mut self, answer: Answer) -> Option<Question> {
        let question = self.asking.take()?;
        match answer {
            Answer::Cancel => None,
            Answer::Confirm => {
                self.close_confirmed |= question == Question::Close;
                Some(question)
            }
        }
    }
}

/// Draws `question` as a modal dialog over the window; the operator's
/// answer on the frame they gave one. Escape and a click beside the dialog
/// cancel.
pub fn show(ctx: &egui::Context, question: Question) -> Option<Answer> {
    let modal = egui::Modal::new(egui::Id::new("close-guard")).show(ctx, |ui| {
        ui.set_max_width(DIALOG_WIDTH);
        ui.heading(question.title());
        ui.label(question.text());
        ui.add_space(8.0);
        let mut answer = None;
        ui.horizontal(|ui| {
            if ui.button(KEEP_RUNNING).clicked() {
                answer = Some(Answer::Cancel);
            }
            let confirm = RichText::new(question.confirm()).color(DANGER_RED);
            if ui.button(confirm).clicked() {
                answer = Some(Answer::Confirm);
            }
        });
        answer
    });
    modal
        .inner
        .or_else(|| modal.should_close().then_some(Answer::Cancel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::snapshot;

    #[test]
    fn only_a_starting_or_running_output_of_a_live_engine_is_live() {
        let mut snap = snapshot();
        assert!(!output_live(&snap), "idle");
        for (state, live) in [
            (OutputState::Starting, true),
            (OutputState::Running, true),
            (OutputState::Stopping, false),
            (OutputState::Finished, false),
            (OutputState::Failed("x".into()), false),
        ] {
            snap.output.state = state.clone();
            assert_eq!(output_live(&snap), live, "{state:?}");
        }
        snap.output.state = OutputState::Running;
        snap.running = false;
        assert!(!output_live(&snap), "a dead engine has nothing to lose");
    }

    #[test]
    fn without_a_live_output_closing_and_stopping_act_at_once() {
        let mut guard = CloseGuard::default();
        assert_eq!(guard.close_requested(false, false), Verdict::Close);
        assert_eq!(guard.stop_requested(false), Some(EngineCommand::StopOutput));
        assert_eq!(guard.question(), None);
    }

    #[test]
    fn a_close_request_during_output_asks_and_cancel_keeps_running() {
        let mut guard = CloseGuard::default();
        assert_eq!(guard.close_requested(true, false), Verdict::KeepOpen);
        assert_eq!(guard.question(), Some(Question::Close));
        // Unanswered, it keeps asking while the output is live.
        guard.observe(true, false);
        assert_eq!(guard.question(), Some(Question::Close));
        assert_eq!(guard.answer(Answer::Cancel), None);
        assert_eq!(guard.question(), None);
        // Cancelling confirmed nothing: the next request asks again.
        assert_eq!(guard.close_requested(true, false), Verdict::KeepOpen);
    }

    #[test]
    fn a_close_question_is_dropped_once_the_output_ends() {
        let mut guard = CloseGuard::default();
        assert_eq!(guard.close_requested(true, false), Verdict::KeepOpen);
        guard.observe(false, false);
        assert_eq!(guard.question(), None, "nothing left to lose");
        // Dropped unanswered, it confirmed nothing: a late answer does
        // nothing, and a request while live again asks again.
        assert_eq!(guard.answer(Answer::Confirm), None);
        assert_eq!(guard, CloseGuard::default());
        assert_eq!(guard.close_requested(true, false), Verdict::KeepOpen);
        // With the output over, closing again acts at once.
        guard.observe(false, false);
        assert_eq!(guard.close_requested(false, false), Verdict::Close);
        assert_eq!(guard.question(), None);
    }

    #[test]
    fn a_confirmed_close_lets_the_windows_own_request_through() {
        let mut guard = CloseGuard::default();
        guard.close_requested(true, false);
        assert_eq!(guard.answer(Answer::Confirm), Some(Question::Close));
        assert_eq!(guard.question(), None);
        assert_eq!(guard.close_requested(true, false), Verdict::Close);
        assert_eq!(guard.close_requested(true, false), Verdict::Close);
    }

    #[test]
    fn stop_asks_and_is_dropped_once_the_output_ends() {
        let mut guard = CloseGuard::default();
        assert_eq!(guard.stop_requested(true), None);
        assert_eq!(guard.question(), Some(Question::Stop));
        guard.observe(true, false);
        assert_eq!(guard.question(), Some(Question::Stop), "still live");
        assert_eq!(guard.answer(Answer::Confirm), Some(Question::Stop));
        // Confirming Stop does not confirm closing.
        assert_eq!(guard.close_requested(true, false), Verdict::KeepOpen);
        guard.answer(Answer::Cancel);
        guard.stop_requested(true);
        guard.observe(false, false);
        assert_eq!(guard.question(), None, "nothing left to stop");
    }

    #[test]
    fn a_close_request_replaces_stop_but_not_the_reverse() {
        let mut guard = CloseGuard::default();
        guard.stop_requested(true);
        assert_eq!(guard.close_requested(true, false), Verdict::KeepOpen);
        assert_eq!(guard.question(), Some(Question::Close));
        assert_eq!(guard.stop_requested(true), None);
        assert_eq!(guard.question(), Some(Question::Close));
    }

    #[test]
    fn a_termination_signal_closes_without_asking() {
        let mut guard = CloseGuard::default();
        guard.close_requested(true, false);
        guard.observe(true, true);
        assert_eq!(guard.question(), None, "the window is closing anyway");
        assert_eq!(guard.close_requested(true, true), Verdict::Close);
        assert_eq!(guard.question(), None);
    }

    #[test]
    fn answers_without_a_question_do_nothing() {
        let mut guard = CloseGuard::default();
        assert_eq!(guard.answer(Answer::Confirm), None);
        assert_eq!(guard, CloseGuard::default());
    }

    #[test]
    fn every_question_has_its_own_words() {
        for q in [Question::Close, Question::Stop] {
            assert!(q.title().ends_with('?'), "{q:?}");
            assert!(!q.text().is_empty());
            assert_ne!(q.confirm(), KEEP_RUNNING);
        }
        assert_ne!(Question::Close.confirm(), Question::Stop.confirm());
    }
}
