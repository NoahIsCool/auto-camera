//! Everything the operator window draws and reads, one snapshot at a time.
//!
//! [`Console`] owns the window's own state (textures, the T-bar, the forms,
//! the close guard). Given the newest snapshot it draws one frame (status
//! bar, side panels, monitors, multiview, and the guard's question when
//! one is open) and returns the commands the operator asked for. It never
//! talks to the engine: [`super::app::OperatorApp`] feeds it snapshots and
//! posts its commands. So the whole window is tested here, headless,
//! through its accessibility tree, on snapshots built by a real engine
//! core.
//!
//! # Keyboard focus
//!
//! The shortcuts ([`super::keys`]) belong to the switcher, but a focused
//! widget owns the keyboard: Space in a prompt is a space, Enter in the
//! duration field ends the edit. So while any widget has focus, or the
//! guard's question is open, the shortcuts are paused and the status bar
//! says [`super::keys::PAUSED`]. A text field or number takes focus when
//! clicked, any widget by Tab; clicking anywhere else gives it up. Escape
//! always does: the console drops the focus whichever widget holds it.
//!
//! Focus is read once per frame, after Escape and before any widget sees
//! that frame's keys. So the key that ends an edit (Enter) is never also a
//! take, and a frame that presses Escape already takes its other keys as
//! shortcuts.
//!
//! # Closing
//!
//! A close request (the window's close button, or the window's own
//! `Close` after a termination signal or a confirmed question) goes to the
//! [`CloseGuard`] before anything is drawn; while the output is live it is
//! cancelled and becomes a question, drawn over the window at the end of
//! the frame. A confirmed close sends `Close`; a confirmed Stop sends
//! `StopOutput`. A question whose output ends first goes away unanswered,
//! and the window stays open. Termination signals reach the console as
//! `quitting`, and with it every close goes through.

use ac_core::ShotId;
use ac_engine::{EngineCommand, EngineSnapshot, PromptReader, StudioConfig};
use eframe::egui::{self, Ui};

use super::close_guard::{self, CloseGuard, Question, Verdict};
use super::keys;
use super::multiview;
use super::panels::{self, OutputForm};
use super::query_form::QueryForm;
use super::switching::{self, SwitchState};
use super::textures::Textures;

/// The window's own state and frame; see the module docs.
#[derive(Debug)]
pub struct Console {
    /// Monitor and tile textures.
    textures: Textures,
    /// T-bar and transition selection.
    switch: SwitchState,
    /// The add-query form.
    query_form: QueryForm,
    /// How prompts are read (the lexicon and what the detector sees),
    /// built from the same config as the engine's, so the form reads
    /// prompts exactly as the engine will.
    prompts: PromptReader,
    /// The output form.
    output_form: OutputForm,
    /// Asks before closing or stopping a live output.
    guard: CloseGuard,
    /// Program aspect ratio.
    aspect: f32,
}

impl Console {
    /// The console of a studio started from `config`.
    pub fn new(config: &StudioConfig) -> Self {
        Self {
            textures: Textures::default(),
            switch: SwitchState::new(config.switcher.to_transition()),
            query_form: QueryForm::default(),
            prompts: PromptReader::of(config),
            output_form: OutputForm::new(
                config.output.rtmp_url.as_deref(),
                config.output.record_path.as_deref(),
            ),
            guard: CloseGuard::default(),
            aspect: config.output.width as f32 / config.output.height.max(1) as f32,
        }
    }

    /// Draws one frame of `snap` into `ui`, with `notice` in the status bar;
    /// returns the commands the operator asked for. `quitting` says a
    /// termination signal came, so a close request goes through unasked.
    pub fn frame(
        &mut self,
        ui: &mut Ui,
        snap: &EngineSnapshot,
        notice: Option<(&str, bool)>,
        quitting: bool,
    ) -> Vec<EngineCommand> {
        let ctx = ui.ctx().clone();
        let events = ctx.input(|i| i.events.clone());
        if keys::drops_focus(&events) {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        let live = close_guard::output_live(snap);
        self.guard.observe(live, quitting);
        if ctx.input(|i| i.viewport().close_requested())
            && self.guard.close_requested(live, quitting) == Verdict::KeepOpen
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let paused = ctx.egui_wants_keyboard_input() || self.guard.question().is_some();
        let ids: Vec<ShotId> = snap.shots.iter().map(|s| s.id.clone()).collect();
        let mut commands: Vec<EngineCommand> = keys::actions(&events, paused)
            .into_iter()
            .filter_map(|a| keys::command(a, &ids))
            .collect();
        egui::Panel::bottom("status").show(ui, |ui| {
            panels::status_bar(ui, snap, paused, notice);
        });
        egui::Panel::right("side")
            .default_size(380.0)
            .resizable(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    commands.extend(panels::director(ui, snap));
                    ui.separator();
                    commands.extend(panels::queries(
                        ui,
                        snap,
                        &mut self.query_form,
                        &self.prompts,
                    ));
                    ui.separator();
                    commands.extend(panels::output(
                        ui,
                        snap,
                        &mut self.output_form,
                        &mut self.guard,
                    ));
                });
            });
        egui::CentralPanel::default().show(ui, |ui| {
            let height = (ui.available_height() * 0.5).clamp(160.0, 640.0);
            commands.extend(switching::show(
                ui,
                snap,
                &mut self.switch,
                &mut self.textures,
                self.aspect,
                height,
            ));
            ui.separator();
            commands.extend(multiview::show(ui, snap, &mut self.textures, self.aspect));
        });
        if let Some(question) = self.guard.question()
            && let Some(answer) = close_guard::show(&ctx, question)
        {
            match self.guard.answer(answer) {
                Some(Question::Close) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Some(Question::Stop) => commands.push(EngineCommand::StopOutput),
                None => {}
            }
        }
        self.textures.retain(|name| {
            name.starts_with("monitor/")
                || name
                    .strip_prefix("tile/")
                    .is_some_and(|id| ids.iter().any(|s| s.0 == id))
        });
        commands
    }
}

#[cfg(test)]
mod tests;
