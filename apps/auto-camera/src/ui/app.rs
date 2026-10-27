//! The operator window's application state and frame loop.
//!
//! Each frame [`OperatorApp`] takes the newest engine snapshot (an `Arc`,
//! never blocking the engine), has the [`Console`] draw it (shortcuts,
//! panels, monitors, multiview, the status bar's notice), and posts every
//! command the operator asked for. Commands are posted, not sent: their
//! replies are polled on later frames ([`OperatorApp::poll`]), so a slow
//! command never freezes the window, and the notices they deserve go to
//! the console with the next frame ([`super::notices`]). The window
//! repaints at about the monitor rate while the engine runs.
//!
//! Closing the window shuts the engine down, which finishes a running
//! recording before the process exits; while the output is live the
//! console asks first ([`super::close_guard`]). A termination signal
//! (Ctrl-C in the terminal, `SIGTERM`, `SIGHUP`; [`crate::signals`]) closes
//! the window the same way, without asking.

use std::time::Duration;

use ac_engine::{Engine, EngineCommand, EngineHandle, PendingReply, StudioConfig};
use eframe::egui;

use super::console::Console;
use super::notices::notice_for;
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

    /// Collects arrived replies; returns the notices they deserve, oldest
    /// first.
    fn poll(&mut self) -> Vec<(String, bool)> {
        let mut raised = Vec::new();
        self.pending.retain(|(name, reply)| match reply.try_get() {
            Some(r) => {
                raised.extend(notice_for(name, &r));
                false
            }
            None => true,
        });
        raised
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
        let quitting = self.quit.requested();
        if quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let snap = self.handle.snapshot();
        let raised = self.poll();
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
mod tests {
    use super::*;
    use ac_core::ShotId;
    use ac_engine::{EngineOptions, MonitorSettings};
    use ac_sources::SourceConfig;
    use std::time::Instant;

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
        if let Some((text, error)) = app.console.notice(&app.handle.snapshot()) {
            assert!(!error, "{text}");
        }
    }

    #[test]
    fn frames_draw_and_shortcuts_reach_the_engine() {
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
        // Real time: on a virtual clock the engine's sleeps would race ahead
        // of the source thread under load, the wide would go stale and the
        // cut would be refused.
        let engine = Engine::start_with(config.clone(), EngineOptions::default()).unwrap();
        let handle = engine.handle();
        handle
            .wait_until(Duration::from_secs(10), |s| {
                s.shots.first().is_some_and(|w| w.available)
            })
            .expect("the source delivers frames");
        let mut app = OperatorApp::new(engine, &config);
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

    /// A file removed when dropped, so a failing test leaves no recording
    /// behind.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_close_request_while_recording_asks_and_the_engine_keeps_running() {
        use crate::headless::tests::{have, temp_path};
        use crate::ui::close_guard::Question;
        use ac_engine::OutputState;
        use egui_kittest::{Harness, kittest::Queryable};
        if !(have("ffmpeg") && have("ffprobe")) {
            eprintln!("skipped: ffmpeg is not installed");
            return;
        }
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
        config.output.encoder = ac_engine::EncoderSetting::Libx264;
        config.monitors = MonitorSettings::disabled();
        let engine = Engine::start_with(config.clone(), EngineOptions::default()).unwrap();
        let handle = engine.handle();
        handle
            .wait_until(Duration::from_secs(10), |s| {
                s.shots.first().is_some_and(|w| w.available)
            })
            .expect("the source delivers frames");
        let path = temp_path("close-guard.mp4");
        // Dropped after the app, which finishes the recording first.
        let _scratch = Scratch(path.clone());
        handle
            .send(EngineCommand::StartOutput {
                rtmp_url: None,
                record_path: Some(path.clone()),
            })
            .unwrap();
        handle
            .wait_until(Duration::from_secs(20), |s| {
                s.output.state == OutputState::Running
            })
            .expect("the recording runs");
        let quit = QuitSignal::new();
        let app = OperatorApp::new(engine, &config).with_quit(quit.clone());
        // The app asks for a repaint every 30 ms, so the harness never
        // settles: it runs a counted number of frames instead.
        let mut h = Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .build_ui_state(|ui, app: &mut OperatorApp| app.frame(ui), app);
        // The window's close button.
        let request_close = |h: &mut Harness<'_, OperatorApp>| {
            h.input_mut()
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .events
                .push(egui::ViewportEvent::Close);
        };
        let commands = |h: &Harness<'_, OperatorApp>| {
            h.output().viewport_output[&egui::ViewportId::ROOT]
                .commands
                .clone()
        };
        request_close(&mut h);
        h.step();
        assert!(commands(&h).contains(&egui::ViewportCommand::CancelClose));
        h.run_steps(2);
        h.get_by_label(Question::Close.title());
        // The show goes on: the engine ticks and records, the dialog waits.
        let asked = handle.snapshot();
        let later = handle
            .wait_until(Duration::from_secs(10), |s| {
                s.tick > asked.tick + 15 && s.output.frames_pushed > asked.output.frames_pushed
            })
            .expect("the engine keeps ticking and recording");
        h.run_steps(2);
        assert!(h.state().engine.is_some());
        assert!(later.running);
        assert_eq!(later.output.state, OutputState::Running);
        h.get_by_label(Question::Close.title());
        // A termination signal still closes, unasked: the app sends Close,
        // and the close request it comes back as goes through.
        quit.signal();
        h.step();
        assert!(commands(&h).contains(&egui::ViewportCommand::Close));
        assert!(h.query_by_label(Question::Close.title()).is_none());
        request_close(&mut h);
        h.step();
        assert!(!commands(&h).contains(&egui::ViewportCommand::CancelClose));
        // Closing shuts the engine down, which finishes the recording.
        h.state_mut().shutdown();
        assert!(h.state().engine.is_none());
        let size = std::fs::metadata(&path).map_or(0, |m| m.len());
        assert!(size > 0, "the recording was finished");
    }
}
