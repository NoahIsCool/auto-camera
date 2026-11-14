//! The window's outer state: the source picker before a studio exists, the
//! operator console once one runs, and the switch from one studio to the
//! next.
//!
//! # Theory
//!
//! A bare launch without a studio file used to print how to copy an
//! example and exit. Now the window opens on the source picker
//! ([`super::picker`]) instead, and the console's Sources button opens the
//! same picker over a running studio. Either way Open studio goes through
//! one path ([`Shell::open`]):
//!
//! 1. the choice is written into the studio file ([`crate::setup::save`]),
//!    which refuses a result that would not load, leaving the file as it
//!    was;
//! 2. the file is loaded as a launch loads it, warnings and all;
//! 3. the running engine, if any, is shut down (the Sources button is off
//!    while output is live, so no recording is cut short), and a new one
//!    is started on the new config with a console of its own.
//!
//! Until step 3 the old studio runs on, so a mistake in the picker never
//! takes the program off. If the new engine does not start, the window
//! goes back to the picker with the reason.
//!
//! The first engine gets the run's [`EngineOptions`] (the trace and
//! detection log of `--trace` and `--record-detections`); an engine started
//! after a change of sources gets the defaults, so those files cover the
//! first studio only.
//!
//! # Invariants
//!
//! * At most one engine runs, and the old one has stopped before the new
//!   one starts (they would open the same cameras).
//! * While the picker is open over a studio, the console's shortcuts are
//!   off, so a key typed at the picker never cuts.

use std::path::{Path, PathBuf};

use ac_engine::{Engine, EngineOptions, StudioConfig};
use eframe::egui;

use super::app::OperatorApp;
use super::header;
use super::picker::{Action, Lister, Picker};
use super::theme;
use crate::logs::Watch;
use crate::setup;
use crate::show::{self, ShowTarget};
use crate::signals::QuitSignal;

/// What the window starts on.
#[derive(Debug)]
pub enum Start {
    /// A studio loaded from the command line's config (boxed: a config
    /// is large).
    Studio(Box<Opened>),
    /// No studio file yet: the source picker.
    Picker,
}

/// A studio loaded from the command line's config.
#[derive(Debug)]
pub struct Opened {
    /// Its config.
    pub config: StudioConfig,
    /// The config's load warnings, for the status bar.
    pub warnings: Vec<String>,
    /// Where Save show writes, or why it cannot.
    pub show: Result<ShowTarget, String>,
}

/// What the shell needs to open studios; see the module docs.
#[derive(Debug)]
pub struct Setup {
    /// The studio file the picker writes and a new studio opens
    /// ([`crate::cli::DEFAULT_CONFIG`]).
    pub studio: PathBuf,
    /// `--save-show`, for the Save show of a studio opened here.
    pub save_show: Option<PathBuf>,
    /// The next engine's options, taken by the first engine started.
    pub options: Option<EngineOptions>,
    /// How the picker lists cameras.
    pub lister: Lister,
}

/// A running studio: its console (which owns the engine) and its log
/// watcher.
struct Studio {
    /// The console and engine.
    app: OperatorApp,
    /// Its name in the window title.
    title: String,
    /// Logs the engine's notes while it runs.
    _watch: Watch,
}

/// The window's state; see the module docs.
pub struct Shell {
    /// The running studio, if any.
    studio: Option<Studio>,
    /// The source picker, while it is open.
    picker: Option<Picker>,
    /// How studios are opened.
    setup: Setup,
    /// Termination signals.
    quit: QuitSignal,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell")
            .field("studio", &self.studio.is_some())
            .field("picker", &self.picker)
            .field("setup", &self.setup)
            .finish()
    }
}

impl Shell {
    /// A shell on `start`, opening studios with `setup` and closing on
    /// `quit`. A [`Start::Studio`] starts its engine here; the error says
    /// why it did not start.
    pub fn new(start: Start, mut setup: Setup, quit: QuitSignal) -> Result<Self, String> {
        let mut shell = match start {
            Start::Studio(opened) => {
                let Opened {
                    config,
                    warnings,
                    show,
                } = *opened;
                let options = setup.options.take().unwrap_or_default();
                let engine =
                    Engine::start_with(config.clone(), options).map_err(|e| e.to_string())?;
                let studio = studio(engine, &config, &warnings, show, &quit);
                Self {
                    studio: Some(studio),
                    picker: None,
                    setup,
                    quit,
                }
            }
            Start::Picker => Self {
                studio: None,
                picker: None,
                setup,
                quit,
            },
        };
        if shell.studio.is_none() {
            shell.picker = Some(shell.new_picker());
        }
        Ok(shell)
    }

    /// The window title for the studio running now, or the picker's.
    pub fn title(&self) -> String {
        match &self.studio {
            Some(s) => format!("Auto Camera · {}", s.title),
            None => "Auto Camera · Sources".into(),
        }
    }

    /// Whether a studio runs.
    #[cfg(test)]
    pub fn running(&self) -> bool {
        self.studio.is_some()
    }

    /// Whether the picker is open.
    #[cfg(test)]
    pub fn picking(&self) -> bool {
        self.picker.is_some()
    }

    /// A picker on what the studio file reads now.
    fn new_picker(&self) -> Picker {
        let now = std::fs::read_to_string(&self.setup.studio)
            .map(|t| setup::current(&t))
            .unwrap_or_default();
        Picker::new(
            &self.setup.studio,
            &folder(&self.setup.studio),
            now,
            self.studio.is_some(),
            self.setup.lister,
        )
    }

    /// Opens `choice` (see the module docs); the error says why the studio
    /// did not change or, after the old one stopped, why the new one did
    /// not start.
    fn open(&mut self, choice: &setup::Choice, ctx: &egui::Context) -> Result<(), String> {
        let path = &self.setup.studio;
        setup::save(path, choice)?;
        let loaded = StudioConfig::load_with_warnings(path).map_err(|e| e.to_string())?;
        for warning in &loaded.warnings {
            eprintln!("auto-camera: warning: {warning}");
            tracing::warn!("{warning}");
        }
        let target = ShowTarget::for_run(
            &loaded,
            path,
            self.setup.save_show.as_deref(),
            show::shows_dir(),
        );
        if let Some(mut old) = self.studio.take() {
            old.app.shutdown();
            tracing::info!(
                "sources changed: restarting the studio from {}",
                path.display()
            );
        }
        let options = self.setup.options.take().unwrap_or_default();
        let engine = Engine::start_with(loaded.config.clone(), options)
            .map_err(|e| format!("the studio did not start: {e}"))?;
        tracing::info!(
            "studio opened from {}: {} sources",
            path.display(),
            loaded.config.sources.len()
        );
        self.studio = Some(studio(
            engine,
            &loaded.config,
            &loaded.warnings,
            target,
            &self.quit,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));
        Ok(())
    }

    /// Draws one frame into `ui`: the console, the picker over it, or the
    /// picker alone.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        theme::ensure(&ctx);
        let picking = self.picker.is_some();
        let mut asked = false;
        match &mut self.studio {
            Some(studio) => {
                studio.app.block(picking);
                studio.app.frame(ui);
                asked = studio.app.take_sources();
            }
            None if self.quit.requested() => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            None => {}
        }
        if asked && !picking {
            self.picker = Some(self.new_picker());
        }
        let action = match (&mut self.picker, self.studio.is_some()) {
            (Some(picker), true) => {
                let modal = egui::Modal::new(egui::Id::new("source-picker"))
                    .show(&ctx, |ui| picker.show(ui));
                let closed = modal.should_close();
                modal.inner.or(closed.then_some(Action::Cancel))
            }
            (Some(picker), false) => alone(ui, picker),
            (None, _) => None,
        };
        match action {
            Some(Action::Cancel) => self.picker = None,
            Some(Action::Apply(choice)) => match self.open(&choice, &ctx) {
                Ok(()) => self.picker = None,
                Err(e) => {
                    eprintln!("auto-camera: {e}");
                    tracing::error!("{e}");
                    let running = self.studio.is_some();
                    if let Some(picker) = &mut self.picker {
                        picker.fail(e, running);
                    }
                }
            },
            None => {}
        }
    }

    /// Stops the engine, if one runs (idempotent).
    pub fn shutdown(&mut self) {
        if let Some(studio) = &mut self.studio {
            studio.app.shutdown();
        }
    }
}

/// A running studio on `engine`, started from `config`, opening with its
/// load `warnings` and saving shows to `show`.
fn studio(
    engine: Engine,
    config: &StudioConfig,
    warnings: &[String],
    show: Result<ShowTarget, String>,
    quit: &QuitSignal,
) -> Studio {
    let watch = crate::logs::watch(engine.handle());
    Studio {
        app: OperatorApp::new(engine, config)
            .with_quit(quit.clone())
            .with_warnings(warnings)
            .with_show_target(show),
        title: header::studio_title(config),
        _watch: watch,
    }
}

/// The folder the studio file at `path` reads its relative paths against.
fn folder(path: &Path) -> PathBuf {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}

/// Draws `picker` alone in the window, on a card in its middle, and
/// returns the operator's answer.
fn alone(ui: &mut egui::Ui, picker: &mut Picker) -> Option<Action> {
    let central = egui::Frame::central_panel(ui.style()).fill(theme::BACKGROUND);
    egui::CentralPanel::default()
        .frame(central)
        .show(ui, |_| {});
    egui::Area::new(egui::Id::new("source-picker-card"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(theme::CARD_RADIUS)
                .inner_margin(24)
                .show(ui, |ui| picker.show(ui))
                .inner
        })
        .inner
}

#[cfg(test)]
mod tests;
