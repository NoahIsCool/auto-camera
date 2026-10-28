//! The window's Save show: a button that asks the engine for the show, and
//! the saver that writes the answer to the show file off the window's
//! thread ([`crate::show`]).
//!
//! Pressing Save show returns `ExportShow` like any other command
//! ([`button`]); the app hands the show in the reply to its [`ShowSaver`],
//! which starts a [`SaveJob`] and, on a later frame, turns its result into
//! the status bar's notice: where the show was saved, or why it was not.
//! Saves never overlap. A press while one runs keeps the newest show and
//! saves it once the running save ends, so the file ends up with the show
//! of the last press, and closing the window finishes both first. A window
//! without a target (no home folder for the data dir) says why when Save
//! show is pressed, and saves nothing.

use ac_engine::{EngineCommand, Show};
use eframe::egui::Ui;

use crate::show::{SaveJob, ShowTarget};

/// The button's label.
pub const SAVE_SHOW: &str = "Save show";

/// What the button's hover says it does.
const SAVE_HOVER: &str = "Write the sources, queries, director, transition and output \
                          destinations to the show file, which --config opens again";

/// Why a window opened without a target cannot save.
const NO_TARGET: &str = "this window has no show file to save to";

/// The Show section: the Save show button, and the command it sends.
pub fn button(ui: &mut Ui) -> Option<EngineCommand> {
    ui.heading("Show");
    let pressed = ui.button(SAVE_SHOW).on_hover_text(SAVE_HOVER).clicked();
    pressed.then_some(EngineCommand::ExportShow)
}

/// Saves the shows the engine exports, one at a time; see the module docs.
#[derive(Debug)]
pub struct ShowSaver {
    /// Where shows are saved, or why they cannot be.
    target: Result<ShowTarget, String>,
    /// The save in progress.
    running: Option<SaveJob>,
    /// The newest show exported while a save ran, saved next.
    next: Option<Show>,
}

impl Default for ShowSaver {
    /// A saver without a target.
    fn default() -> Self {
        Self::new(Err(NO_TARGET.into()))
    }
}

impl ShowSaver {
    /// A saver writing to `target`, or saying why it cannot.
    pub fn new(target: Result<ShowTarget, String>) -> Self {
        Self {
            target,
            running: None,
            next: None,
        }
    }

    /// Saves `show`, or queues it behind the save in progress; the notice
    /// when it cannot start.
    pub fn save(&mut self, show: Show) -> Option<(String, bool)> {
        let target = match &self.target {
            Ok(target) => target.clone(),
            Err(why) => return Some(failed(why)),
        };
        if self.running.is_some() {
            self.next = Some(show);
            return None;
        }
        match SaveJob::start(show, target) {
            Ok(job) => {
                self.running = Some(job);
                None
            }
            Err(why) => Some(failed(&why)),
        }
    }

    /// The notice of a save that ended since the last call, if one did,
    /// starting the queued one.
    pub fn poll(&mut self) -> Vec<(String, bool)> {
        let Some(result) = self.running.as_mut().and_then(SaveJob::try_finish) else {
            return Vec::new();
        };
        self.running = None;
        let mut out = vec![notice(result)];
        if let Some(show) = self.next.take() {
            out.extend(self.save(show));
        }
        out
    }
}

impl Drop for ShowSaver {
    /// Finishes the running save and the queued one before the window
    /// goes, noting their results on stderr, since no window shows them.
    fn drop(&mut self) {
        let mut results = Vec::new();
        if let Some(job) = self.running.take() {
            results.push(job.wait());
        }
        if let (Some(show), Ok(target)) = (self.next.take(), &self.target) {
            results.push(SaveJob::start(show, target.clone()).and_then(SaveJob::wait));
        }
        for result in results {
            crate::awake::log(&notice(result).0);
        }
    }
}

/// The notice for a save that ended with `result`.
fn notice(result: Result<std::path::PathBuf, String>) -> (String, bool) {
    match result {
        Ok(path) => (format!("show saved to {}", path.display()), false),
        Err(why) => failed(&why),
    }
}

/// The notice for a save that failed, `why`.
fn failed(why: &str) -> (String, bool) {
    (format!("cannot save the show: {why}"), true)
}

#[cfg(test)]
mod tests;
