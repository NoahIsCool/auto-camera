//! The operator window's application state and frame loop.
//!
//! Each frame [`OperatorApp`] takes the newest engine snapshot (an `Arc`,
//! never blocking the engine), turns shortcuts into commands, draws the
//! panels, the monitors and the multiview, and posts every command the
//! operator asked for. Commands are posted, not sent: their replies are
//! polled on later frames ([`OperatorApp::poll`]), so a slow command never
//! freezes the window, and errors surface as a notice in the status bar.
//! The window repaints at about the monitor rate while the engine runs.
//!
//! An output session the engine cannot hold awake (no `caffeinate` on this
//! platform, or one that cannot start or ends early) is noted on stderr
//! once, from the output view ([`crate::awake::SessionNotes`]).
//!
//! Closing the window shuts the engine down, which finishes a running
//! recording before the process exits. A termination signal (Ctrl-C in
//! the terminal, `SIGTERM`, `SIGHUP`; [`crate::signals`]) closes the window
//! the same way.

use std::time::{Duration, Instant};

use ac_core::ShotId;
use ac_engine::{
    CommandError, CommandReply, Engine, EngineCommand, EngineHandle, PendingReply, PromptReader,
    StudioConfig,
};
use eframe::egui;

use super::keys;
use super::multiview;
use super::panels::{self, OutputForm};
use super::query_form::QueryForm;
use super::switching::{self, SwitchState};
use super::textures::Textures;
use crate::awake::{self, SessionNotes};
use crate::signals::QuitSignal;

/// How long a notice stays in the status bar.
const NOTICE_FOR: Duration = Duration::from_secs(8);
/// Repaint interval while running (about 30 Hz, the monitor rate).
const REPAINT: Duration = Duration::from_millis(30);

/// A message for the operator.
#[derive(Debug, Clone)]
struct Notice {
    /// Text.
    text: String,
    /// It reports a failure.
    error: bool,
    /// When it was raised.
    at: Instant,
}

/// The notice for a command's reply, if it deserves one. Routine switching
/// replies are silent; every error is shown.
pub fn notice_for(
    command: &str,
    reply: &Result<CommandReply, CommandError>,
) -> Option<(String, bool)> {
    match reply {
        Err(e) => Some((format!("{command}: {e}"), true)),
        Ok(CommandReply::QueryAdded { id }) => Some((format!("query {} added", id.0), false)),
        Ok(CommandReply::OutputStarting) => Some(("output starting".into(), false)),
        Ok(CommandReply::OutputStopping) => Some(("output finishing".into(), false)),
        Ok(CommandReply::Done) => match command {
            "set_preview" | "cut" | "auto" | "set_tbar" | "accept_suggestion" => None,
            other => Some((format!("{other}: done"), false)),
        },
    }
}

/// The notice for an engine that is no longer running, with the reason when
/// it died rather than stopped on request; it replaces any other notice.
pub fn stopped_notice(snap: &ac_engine::EngineSnapshot) -> Option<(String, bool)> {
    if snap.running {
        return None;
    }
    Some(match &snap.fatal {
        Some(why) => (format!("the engine has stopped: {why}"), true),
        None => ("the engine has stopped".into(), true),
    })
}

/// The operator window; see the module docs.
pub struct OperatorApp {
    /// The engine, until the window closes.
    engine: Option<Engine>,
    /// Commands and snapshots.
    handle: EngineHandle,
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
    /// Replies not yet arrived, with their command names.
    pending: Vec<(&'static str, PendingReply)>,
    /// The latest notice.
    notice: Option<Notice>,
    /// Program aspect ratio.
    aspect: f32,
    /// Termination signals; when one has come the window closes.
    quit: QuitSignal,
    /// Notes on output sessions that are not held awake.
    awake: SessionNotes,
    /// Where notes go: stderr ([`awake::log`]); tests collect them.
    log: Box<dyn FnMut(&str)>,
}

impl std::fmt::Debug for OperatorApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperatorApp")
            .field("pending", &self.pending.len())
            .field("textures", &self.textures)
            .finish_non_exhaustive()
    }
}

impl OperatorApp {
    /// An app driving `engine`, which was started from `config`.
    pub fn new(engine: Engine, config: &StudioConfig) -> Self {
        Self {
            handle: engine.handle(),
            engine: Some(engine),
            textures: Textures::default(),
            switch: SwitchState::new(config.switcher.to_transition()),
            query_form: QueryForm::default(),
            prompts: PromptReader::of(config),
            output_form: OutputForm::new(
                config.output.rtmp_url.as_deref(),
                config.output.record_path.as_deref(),
            ),
            pending: Vec::new(),
            notice: None,
            aspect: config.output.width as f32 / config.output.height.max(1) as f32,
            quit: QuitSignal::new(),
            awake: SessionNotes::default(),
            log: Box::new(awake::log),
        }
    }

    /// The same app, closing its window once `quit` is requested.
    pub fn with_quit(mut self, quit: QuitSignal) -> Self {
        self.quit = quit;
        self
    }

    /// Posts `command`; its reply is collected by [`Self::poll`].
    fn post(&mut self, command: EngineCommand) {
        let name = command.name();
        self.pending.push((name, self.handle.post(command)));
    }

    /// Collects arrived replies and raises notices for them.
    fn poll(&mut self) {
        let mut raised = Vec::new();
        self.pending.retain(|(name, reply)| match reply.try_get() {
            Some(r) => {
                raised.extend(notice_for(name, &r));
                false
            }
            None => true,
        });
        for (text, error) in raised {
            self.notice = Some(Notice {
                text,
                error,
                at: Instant::now(),
            });
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.at.elapsed() > NOTICE_FOR)
        {
            self.notice = None;
        }
    }

    /// Stops the engine (idempotent).
    fn shutdown(&mut self) {
        if let Some(engine) = self.engine.take()
            && let Err(e) = engine.shutdown()
        {
            eprintln!("auto-camera: engine shutdown: {e}");
        }
    }

    /// Draws one frame into `ui`; asks the window to close once a
    /// termination signal came (closing shuts the engine down).
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.quit.requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let snap = self.handle.snapshot();
        self.poll();
        if let Some(note) = self.awake.note(&snap.output.keep_awake) {
            (self.log)(&note);
        }
        let ids: Vec<ShotId> = snap.shots.iter().map(|s| s.id.clone()).collect();
        let typing = ctx.egui_wants_keyboard_input();
        let events = ctx.input(|i| i.events.clone());
        let mut commands: Vec<EngineCommand> = keys::actions(&events, typing)
            .into_iter()
            .filter_map(|a| keys::command(a, &ids))
            .collect();
        let mut notice = self.notice.as_ref().map(|n| (n.text.clone(), n.error));
        if let Some(stopped) = stopped_notice(&snap) {
            notice = Some(stopped);
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            panels::status_bar(ui, &snap, notice.as_ref().map(|(t, e)| (t.as_str(), *e)));
        });
        egui::Panel::right("side")
            .default_size(380.0)
            .resizable(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    commands.extend(panels::director(ui, &snap));
                    ui.separator();
                    commands.extend(panels::queries(
                        ui,
                        &snap,
                        &mut self.query_form,
                        &self.prompts,
                    ));
                    ui.separator();
                    commands.extend(panels::output(ui, &snap, &mut self.output_form));
                });
            });
        egui::CentralPanel::default().show(ui, |ui| {
            let height = (ui.available_height() * 0.5).clamp(160.0, 640.0);
            commands.extend(switching::show(
                ui,
                &snap,
                &mut self.switch,
                &mut self.textures,
                self.aspect,
                height,
            ));
            ui.separator();
            commands.extend(multiview::show(ui, &snap, &mut self.textures, self.aspect));
        });
        self.textures.retain(|name| {
            name.starts_with("monitor/")
                || name
                    .strip_prefix("tile/")
                    .is_some_and(|id| ids.iter().any(|s| s.0 == id))
        });
        for command in commands {
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
mod tests {
    use super::*;
    use crate::testing::{have, temp_path};
    use ac_core::QueryId;
    use ac_engine::{EngineOptions, MonitorSettings, OutputState};
    use ac_output::Caffeinate;
    use ac_sources::SourceConfig;
    use ac_switcher::SwitchError;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn routine_replies_are_silent_and_errors_are_shown() {
        assert_eq!(notice_for("cut", &Ok(CommandReply::Done)), None);
        assert_eq!(notice_for("set_tbar", &Ok(CommandReply::Done)), None);
        assert_eq!(
            notice_for("pin", &Ok(CommandReply::Done)),
            Some(("pin: done".into(), false))
        );
        assert_eq!(
            notice_for(
                "add_query",
                &Ok(CommandReply::QueryAdded {
                    id: QueryId("singer".into())
                })
            ),
            Some(("query singer added".into(), false))
        );
        let (text, error) =
            notice_for("auto", &Err(CommandError::Switch(SwitchError::NoPreview))).unwrap();
        assert!(error && text.starts_with("auto: "), "{text}");
    }

    /// Runs frames until every posted command was answered, then fails with
    /// the notice if the last answer was an error (a refused command fails
    /// here, with its reason, instead of as a timeout later).
    fn settle(app: &mut OperatorApp, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !app.pending.is_empty() && Instant::now() < deadline {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.pending.is_empty(), "commands were not answered");
        if let Some(n) = &app.notice {
            assert!(!n.error, "{}", n.text);
        }
    }

    /// A one-source synthetic studio at 160x90 with the monitors off.
    fn small_studio() -> StudioConfig {
        let mut config = StudioConfig::with_source(
            "stage",
            SourceConfig::Synthetic {
                width: 160,
                height: 90,
                fps: 30.0,
                subjects: 1,
            },
        );
        config.output.width = 160;
        config.output.height = 90;
        config.monitors = MonitorSettings::disabled();
        config
    }

    /// Starts `config` with `options` (in real time: on a virtual clock the
    /// engine's sleeps would race ahead of the source thread under load,
    /// the wide would go stale and a cut would be refused) and waits until
    /// the wide is available.
    fn start(config: &StudioConfig, options: EngineOptions) -> Engine {
        let engine = Engine::start_with(config.clone(), options).unwrap();
        engine
            .handle()
            .wait_until(Duration::from_secs(10), |s| {
                s.shots.first().is_some_and(|w| w.available)
            })
            .expect("the source delivers frames");
        engine
    }

    #[test]
    fn frames_draw_and_shortcuts_reach_the_engine() {
        let config = small_studio();
        let mut app = OperatorApp::new(start(&config, EngineOptions::default()), &config);
        let ctx = egui::Context::default();
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let input = egui::RawInput {
            events: vec![key(egui::Key::Num1)],
            ..egui::RawInput::default()
        };
        let _ = ctx.run_ui(input, |ui| app.frame(ui));
        settle(&mut app, &ctx);
        let snap = app
            .handle
            .wait_until(Duration::from_secs(10), |s| s.switcher.preview.is_some())
            .unwrap();
        assert_eq!(snap.switcher.preview, Some(ShotId("stage/wide".into())));
        // Space cuts; the reply is collected on a later frame.
        let input = egui::RawInput {
            events: vec![key(egui::Key::Space)],
            ..egui::RawInput::default()
        };
        let _ = ctx.run_ui(input, |ui| app.frame(ui));
        settle(&mut app, &ctx);
        let snap = app
            .handle
            .wait_until(Duration::from_secs(10), |s| s.switcher.program.is_some())
            .expect("cut to program");
        assert_eq!(snap.switcher.program, Some(ShotId("stage/wide".into())));
        app.shutdown();
        assert!(app.engine.is_none());
        app.shutdown();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
    }

    /// Draws frames of `app` until `done` holds for its newest output
    /// state, for at most 20 s, then one more.
    fn draw_until(app: &mut OperatorApp, ctx: &egui::Context, done: fn(&OutputState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(&app.handle.snapshot().output.state) {
            assert!(
                Instant::now() < deadline,
                "{:?}",
                app.handle.snapshot().output
            );
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
    }

    #[test]
    fn a_session_that_is_not_held_awake_is_noted_once() {
        if !have("ffmpeg") {
            eprintln!("skipped: ffmpeg is not installed");
            return;
        }
        let mut config = small_studio();
        config.output.encoder = ac_engine::EncoderSetting::Libx264;
        // As on a platform without caffeinate, whichever this one is.
        let options = EngineOptions {
            keep_awake: Caffeinate::Unsupported,
            ..EngineOptions::default()
        };
        let mut app = OperatorApp::new(start(&config, options), &config);
        let notes = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = notes.clone();
        app.log = Box::new(move |n| sink.borrow_mut().push(n.to_owned()));
        let ctx = egui::Context::default();
        draw_until(&mut app, &ctx, |_| true);
        assert!(notes.borrow().is_empty(), "noted before any output");
        let path = temp_path("window-awake.mp4");
        app.handle
            .send(EngineCommand::StartOutput {
                rtmp_url: None,
                record_path: Some(path.clone()),
            })
            .unwrap();
        draw_until(&mut app, &ctx, |s| *s == OutputState::Running);
        draw_until(&mut app, &ctx, |_| true);
        let noted = notes.borrow().clone();
        assert_eq!(noted.len(), 1, "{noted:?}");
        assert!(noted[0].contains("only on macOS"), "{noted:?}");
        // Not again while the session runs on, nor when it ends.
        app.handle.send(EngineCommand::StopOutput).unwrap();
        draw_until(&mut app, &ctx, |s| *s == OutputState::Finished);
        assert_eq!(notes.borrow().len(), 1, "{:?}", notes.borrow());
        app.shutdown();
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_dead_engine_is_shown_with_its_reason() {
        let mut snap = super::super::fixtures::snapshot();
        assert_eq!(stopped_notice(&snap), None);
        snap.running = false;
        assert_eq!(
            stopped_notice(&snap),
            Some(("the engine has stopped".into(), true))
        );
        snap.fatal = Some("the engine thread panicked: boom".into());
        let (text, error) = stopped_notice(&snap).unwrap();
        assert!(error && text.ends_with("panicked: boom"), "{text}");
    }
}
