//! The operator window's application state and frame loop.
//!
//! Each frame [`OperatorApp`] takes the newest engine snapshot (an `Arc`,
//! never blocking the engine), has the [`Console`] draw it (shortcuts,
//! panels, monitors, multiview, the status bar's notice), and posts every
//! command the operator asked for. Commands are posted, not sent: their
//! replies are polled on later frames ([`OperatorApp::poll`]), so a slow
//! command never freezes the window, and the notices they deserve go to
//! the console with the next frame ([`super::notices`]). Once no reply is
//! outstanding, the app tells the console from which snapshot tick on
//! everything it sent shows ([`Console::answered`]): the engine applies
//! commands at the start of a tick and publishes that tick's snapshot at
//! its end, so the snapshot read after the last reply may still predate
//! them, but the next tick's cannot. The window repaints at about the
//! monitor rate while the engine runs.
//!
//! The config's load warnings (a missing `schema` key) open the window as
//! a notice in the status bar ([`OperatorApp::with_warnings`]). The show
//! in the reply to Save show goes to the [`ShowSaver`], which writes it off
//! this thread and raises its notice once written ([`super::save`]).
//!
//! An output session the engine cannot hold awake (no `caffeinate` on this
//! platform, or one that cannot start or ends early) is noted on stderr and
//! in the log once, from the output view ([`crate::awake::SessionNotes`]).
//!
//! Closing the window shuts the engine down, which finishes a running
//! recording before the process exits; while the output is live the
//! console asks first ([`super::close_guard`]). A termination signal
//! (Ctrl-C in the terminal, `SIGTERM`, `SIGHUP`; [`crate::signals`]) closes
//! the window the same way, without asking.

use std::time::Duration;

use ac_engine::{CommandReply, Engine, EngineCommand, EngineHandle, PendingReply, StudioConfig};
use eframe::egui;

use super::console::Console;
use super::notices::{notice_for, warnings_notice};
use super::save::ShowSaver;
use crate::awake::{self, SessionNotes};
use crate::show::ShowTarget;
use crate::signals::QuitSignal;

/// Repaint interval while running (about 30 Hz, the monitor rate).
const REPAINT: Duration = Duration::from_millis(30);

/// The operator window; see the module docs.
pub struct OperatorApp {
    /// The engine, until the window closes.
    engine: Option<Engine>,
    /// Commands and snapshots.
    handle: EngineHandle,
    /// What the window draws and reads.
    console: Console,
    /// Replies not yet arrived, with their command names.
    pending: Vec<(&'static str, PendingReply)>,
    /// Termination signals; when one has come the window closes.
    quit: QuitSignal,
    /// Notes on output sessions that are not held awake.
    awake: SessionNotes,
    /// Where notes go: stderr and the log ([`awake::log`]); tests collect
    /// them.
    log: Box<dyn FnMut(&str)>,
    /// Writes the shows Save show exports.
    saver: ShowSaver,
}

impl std::fmt::Debug for OperatorApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperatorApp")
            .field("pending", &self.pending.len())
            .field("console", &self.console)
            .finish_non_exhaustive()
    }
}

impl OperatorApp {
    /// An app driving `engine`, which was started from `config`.
    pub fn new(engine: Engine, config: &StudioConfig) -> Self {
        let handle = engine.handle();
        Self {
            console: Console::new(config, &handle.snapshot()),
            handle,
            engine: Some(engine),
            pending: Vec::new(),
            quit: QuitSignal::new(),
            awake: SessionNotes::default(),
            log: Box::new(awake::log),
            saver: ShowSaver::default(),
        }
    }

    /// The same app, saving shows to `target` (or saying why it cannot).
    pub fn with_show_target(mut self, target: Result<ShowTarget, String>) -> Self {
        self.saver = ShowSaver::new(target);
        self
    }

    /// The same app, closing its window once `quit` is requested.
    pub fn with_quit(mut self, quit: QuitSignal) -> Self {
        self.quit = quit;
        self
    }

    /// The same app, opening with the config's load `warnings` in the
    /// status bar (for as long as any notice stays).
    pub fn with_warnings(mut self, warnings: &[String]) -> Self {
        if let Some((text, error)) = warnings_notice(warnings) {
            self.console.raise_on_open(text, error);
        }
        self
    }

    /// Whether the operator pressed Sources since the last call.
    pub fn take_sources(&mut self) -> bool {
        self.console.take_sources()
    }

    /// Turns the shortcuts off while the source picker is open over the
    /// console, or on again.
    pub fn block(&mut self, blocked: bool) {
        self.console.block(blocked);
    }

    /// Posts `command`; its reply is collected by [`Self::poll`].
    fn post(&mut self, command: EngineCommand) {
        let name = command.name();
        self.pending.push((name, self.handle.post(command)));
    }

    /// Collects arrived replies; returns the notices they deserve, oldest
    /// first.
    fn poll(&mut self) -> Vec<(String, bool)> {
        let mut raised = Vec::new();
        self.pending.retain(|(name, reply)| match reply.try_get() {
            Some(Ok(CommandReply::Show(show))) => {
                raised.extend(self.saver.save(*show));
                false
            }
            Some(r) => {
                raised.extend(notice_for(name, &r));
                false
            }
            None => true,
        });
        raised.extend(self.saver.poll());
        raised
    }

    /// Stops the engine (idempotent).
    pub fn shutdown(&mut self) {
        if let Some(engine) = self.engine.take()
            && let Err(e) = engine.shutdown()
        {
            eprintln!("auto-camera: engine shutdown: {e}");
            tracing::error!("engine shutdown: {e}");
        }
    }

    /// Draws one frame into `ui`; asks the window to close once a
    /// termination signal came (closing shuts the engine down).
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let quitting = self.quit.requested();
        if quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let raised = self.poll();
        // Read after the replies arrived (see the module docs).
        let snap = self.handle.snapshot();
        if self.pending.is_empty() {
            self.console.answered(snap.tick + 1);
        }
        if let Some(note) = self.awake.note(&snap.output.keep_awake) {
            (self.log)(&note);
        }
        for command in self.console.frame(ui, &snap, raised, quitting) {
            self.post(command);
        }
        ctx.request_repaint_after(REPAINT);
    }
}

impl eframe::App for OperatorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}

impl Drop for OperatorApp {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests;
