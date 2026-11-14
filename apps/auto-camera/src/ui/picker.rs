//! The source picker: choose a video file or a set of cameras in the
//! window instead of editing the studio file.
//!
//! # Theory
//!
//! The picker is a form over [`crate::setup`]: it shows what the studio
//! file reads now ([`crate::setup::current`]), lets the operator pick
//! either one video file (typed, chosen with the Mac's Open panel, or
//! dropped on the window) or any number of the cameras macOS lists, with
//! one capture mode for all, and returns the [`Choice`] when they press
//! Open studio. Writing the file and restarting the engine are the
//! shell's job ([`super::shell`]); the picker never touches the disk
//! except to see whether a typed path is a file.
//!
//! Listing cameras runs `system_profiler`, which takes a second or more,
//! so it runs on a thread of its own (`ac-list-cameras`) and the form polls
//! its answer each frame; Refresh lists again after a camera is plugged
//! in. Cameras the studio names that are not connected now stay in the
//! list, ticked and marked, so opening the picker never drops them
//! unasked.
//!
//! # Invariants
//!
//! * Open studio is only offered for a choice that can be written: a path
//!   to an existing file, or at least one camera.
//! * A relative path is read against the working folder, as one typed on
//!   the command line, and returned absolute, so the studio file never
//!   depends on where the app is started from.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui::{self, RichText, Ui};

use super::theme;
use crate::setup::{Camera, Choice, Current, MODES, Mode};

/// What the operator answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Leave the studio as it is (only offered while one runs).
    Cancel,
    /// Write this choice and open the studio.
    Apply(Choice),
}

/// Which kind of source the form shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// One video file.
    File,
    /// Cameras.
    Cameras,
}

/// Lists the connected cameras (blocking); [`crate::setup::list_cameras`]
/// in the app, a canned list in tests.
pub type Lister = fn() -> Result<Vec<Camera>, String>;

/// The camera listing, as far as it has got.
#[derive(Debug)]
enum Listing {
    /// Running on its thread.
    Waiting(Receiver<Result<Vec<Camera>, String>>),
    /// The cameras connected when it ran.
    Ready(Vec<Camera>),
    /// Why there is none.
    Failed(String),
}

/// The picker's form; see the module docs.
#[derive(Debug)]
pub struct Picker {
    /// The kind of source shown.
    pub tab: Tab,
    /// The video file path as typed.
    pub path: String,
    /// The connected cameras.
    listing: Listing,
    /// How cameras are listed.
    lister: Lister,
    /// The cameras the studio names now, connected or not.
    known: Vec<Camera>,
    /// The ticked cameras' unique IDs.
    ticked: Vec<String>,
    /// The mode every camera is asked for.
    mode: Mode,
    /// Why the last Open studio did not work (set by the shell).
    error: Option<String>,
    /// Whether Cancel is offered (a studio is running behind the form).
    cancellable: bool,
    /// The studio file, as shown.
    target: String,
}

/// Width of the picker's card.
pub const WIDTH: f32 = 560.0;
/// The primary button's label.
pub const OPEN: &str = "Open studio";
/// The Cancel button's label.
pub const CANCEL: &str = "Cancel";
/// The video file field's label.
pub const FILE_FIELD: &str = "Video file path";
/// What the form says while cameras are listed.
pub const LOOKING: &str = "Looking for cameras…";
/// The extensions the Open panel offers as video.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const VIDEO_EXTENSIONS: [&str; 9] = [
    "mp4", "mov", "m4v", "mkv", "avi", "webm", "mts", "m2ts", "ts",
];

impl Picker {
    /// A form for the studio file `target` (in the folder `dir`, which its
    /// paths are read against), opened on what it reads now, listing
    /// cameras with `lister`; `cancellable` while a studio runs.
    pub fn new(target: &Path, dir: &Path, now: Current, cancellable: bool, lister: Lister) -> Self {
        let path = now
            .file
            .map(|f| absolute(&dir.join(f)).display().to_string())
            .unwrap_or_default();
        // A video file is the usual first try; cameras once the studio
        // has some.
        let tab = if now.cameras.is_empty() {
            Tab::File
        } else {
            Tab::Cameras
        };
        Self {
            tab,
            path,
            listing: list(lister),
            lister,
            ticked: now.cameras.iter().map(|c| c.unique_id.clone()).collect(),
            known: now.cameras,
            mode: now.mode.unwrap_or(MODES[0]),
            error: None,
            cancellable,
            target: target.display().to_string(),
        }
    }

    /// Shows why the last choice could not be opened; Cancel stays only
    /// while a studio still `running` behind the form.
    pub fn fail(&mut self, error: String, running: bool) {
        self.error = Some(error);
        self.cancellable = running;
    }

    /// The choice the form holds, or why it cannot be opened yet.
    pub fn choice(&self) -> Result<Choice, String> {
        match self.tab {
            Tab::File => {
                let typed = self.path.trim();
                if typed.is_empty() {
                    return Err("Choose a video file.".into());
                }
                let path = absolute(Path::new(typed));
                if !path.is_file() {
                    return Err(format!("There is no file at {}.", path.display()));
                }
                Ok(Choice::File(path))
            }
            Tab::Cameras => {
                let cameras: Vec<Camera> = self
                    .rows()
                    .into_iter()
                    .filter(|(c, _)| self.is_ticked(c))
                    .map(|(c, _)| c)
                    .collect();
                if cameras.is_empty() {
                    return Err("Tick at least one camera.".into());
                }
                Ok(Choice::Cameras {
                    cameras,
                    mode: self.mode,
                })
            }
        }
    }

    /// The camera rows, connected ones first in macOS's order, then the
    /// studio's cameras that are not connected, each with whether it is
    /// connected.
    fn rows(&self) -> Vec<(Camera, bool)> {
        let connected: &[Camera] = match &self.listing {
            Listing::Ready(cameras) => cameras,
            _ => &[],
        };
        let mut rows: Vec<(Camera, bool)> = connected.iter().map(|c| (c.clone(), true)).collect();
        for camera in &self.known {
            if !connected.iter().any(|c| same(c, camera)) {
                rows.push((camera.clone(), false));
            }
        }
        rows
    }

    /// Whether `camera` is ticked.
    fn is_ticked(&self, camera: &Camera) -> bool {
        self.ticked
            .iter()
            .any(|id| id.eq_ignore_ascii_case(&camera.unique_id))
    }

    /// Ticks or unticks `camera`.
    fn set_ticked(&mut self, camera: &Camera, on: bool) {
        self.ticked
            .retain(|id| !id.eq_ignore_ascii_case(&camera.unique_id));
        if on {
            self.ticked.push(camera.unique_id.clone());
        }
    }

    /// Collects the listing's answer if it has come.
    fn poll(&mut self) {
        if let Listing::Waiting(rx) = &self.listing {
            self.listing = match rx.try_recv() {
                Ok(Ok(cameras)) => Listing::Ready(cameras),
                Ok(Err(why)) => Listing::Failed(why),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    Listing::Failed("the camera listing stopped without an answer".into())
                }
            };
        }
    }

    /// Whether the camera listing is still running.
    pub fn listing(&self) -> bool {
        matches!(self.listing, Listing::Waiting(_))
    }

    /// Draws the form into `ui` and returns the operator's answer, if they
    /// gave one this frame.
    pub fn show(&mut self, ui: &mut Ui) -> Option<Action> {
        self.poll();
        if self.listing() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let dropped = ui
            .ctx()
            .input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(path) = dropped {
            self.tab = Tab::File;
            self.path = path.display().to_string();
            self.error = None;
        }
        ui.set_width(WIDTH);
        ui.label(theme::caps(ui, "Sources"));
        ui.add_space(2.0);
        ui.label(
            theme::semibold(ui, "What should Auto Camera film?", theme::LARGE)
                .color(theme::TEXT_STRONG),
        );
        ui.label(
            RichText::new(format!(
                "Saved to {}. The rest of that file, queries and output included, is kept.",
                self.target
            ))
            .color(theme::TEXT_WEAK)
            .size(theme::SMALL),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::File, "Video file");
            ui.selectable_value(&mut self.tab, Tab::Cameras, "Cameras");
        });
        ui.add_space(8.0);
        match self.tab {
            Tab::File => self.file_form(ui),
            Tab::Cameras => self.camera_form(ui),
        }
        ui.add_space(10.0);
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(theme::RED_TEXT));
            ui.add_space(6.0);
        }
        let ready = self.choice();
        let mut action = None;
        let footer = egui::vec2(ui.available_width(), 28.0);
        let right = egui::Layout::right_to_left(egui::Align::Center);
        ui.allocate_ui_with_layout(footer, right, |ui| {
            let open = ui
                .add_enabled(ready.is_ok(), theme::primary(ui, OPEN))
                .on_disabled_hover_text(ready.as_ref().err().cloned().unwrap_or_default());
            if open.clicked()
                && let Ok(choice) = &ready
            {
                action = Some(Action::Apply(choice.clone()));
            }
            if self.cancellable && ui.button(CANCEL).clicked() {
                action = Some(Action::Cancel);
            }
            if let Err(why) = &ready {
                ui.label(
                    RichText::new(why)
                        .color(theme::TEXT_WEAK)
                        .size(theme::SMALL),
                );
            }
        });
        action
    }

    /// The video file part of the form.
    fn file_form(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let browse = browse_button(ui);
            let field = egui::TextEdit::singleline(&mut self.path)
                .hint_text("Path to a video file")
                .desired_width(ui.available_width());
            ui.add(field).on_hover_text(FILE_FIELD);
            if let Some(path) = browse {
                self.path = path.display().to_string();
                self.error = None;
            }
        });
        ui.label(
            RichText::new(if cfg!(target_os = "macos") {
                "Choose a file, type its path, or drop it on this window. It plays in a loop, \
                 with its sound as program audio."
            } else {
                "Type the file's path or drop it on this window. It plays in a loop, with its \
                 sound as program audio."
            })
            .color(theme::TEXT_WEAK)
            .size(theme::SMALL),
        );
    }

    /// The camera part of the form.
    fn camera_form(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::caps(ui, "Connected cameras"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!self.listing(), egui::Button::new("Refresh"))
                    .on_hover_text("List the cameras again, after plugging one in")
                    .clicked()
                {
                    self.listing = list(self.lister);
                }
            });
        });
        egui::Frame::new()
            .fill(theme::WELL)
            .corner_radius(theme::RADIUS)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                match &self.listing {
                    Listing::Waiting(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(LOOKING);
                        });
                    }
                    Listing::Failed(why) => {
                        ui.label(RichText::new(why).color(theme::RED_TEXT));
                    }
                    Listing::Ready(cameras) if cameras.is_empty() && self.known.is_empty() => {
                        ui.label(
                            RichText::new("No cameras found. Plug one in and press Refresh.")
                                .color(theme::TEXT_WEAK),
                        );
                    }
                    Listing::Ready(_) => {}
                }
                for (camera, connected) in self.rows() {
                    let mut on = self.is_ticked(&camera);
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut on, camera.name.as_str()).changed() {
                            self.set_ticked(&camera, on);
                        }
                        let note = if connected {
                            camera.unique_id.clone()
                        } else {
                            format!("{} · not connected", camera.unique_id)
                        };
                        let color = if connected {
                            theme::TEXT_WEAK
                        } else {
                            theme::AMBER
                        };
                        ui.label(
                            RichText::new(note)
                                .monospace()
                                .size(theme::SMALL)
                                .color(color),
                        );
                    });
                }
            });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Ask each camera for");
            egui::ComboBox::from_id_salt("picker-mode")
                .selected_text(self.mode.label())
                .show_ui(ui, |ui| {
                    for mode in MODES {
                        ui.selectable_value(&mut self.mode, mode, mode.label());
                    }
                });
        });
        ui.label(
            RichText::new(
                "Each ticked camera becomes a source, in this order. A mode a camera does not \
                 offer leaves it offline, and the status bar lists the modes it has.",
            )
            .color(theme::TEXT_WEAK)
            .size(theme::SMALL),
        );
    }
}

/// Whether `a` and `b` are the same camera (unique IDs ignoring ASCII
/// case, as the engine matches them).
fn same(a: &Camera, b: &Camera) -> bool {
    a.unique_id.trim().eq_ignore_ascii_case(b.unique_id.trim())
}

/// Starts listing cameras with `lister` on a thread of its own.
fn list(lister: Lister) -> Listing {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("ac-list-cameras".into())
        .spawn(move || {
            let _ = tx.send(lister());
        });
    match spawned {
        Ok(_) => Listing::Waiting(rx),
        Err(e) => Listing::Failed(format!("cannot list cameras: {e}")),
    }
}

/// `path` made absolute against the working folder (as it is when that
/// cannot be read).
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The Browse button: the Mac's Open panel for a video file, whose choice
/// it returns. Elsewhere there is no button.
fn browse_button(ui: &mut Ui) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        if ui.button("Choose…").clicked() {
            return rfd::FileDialog::new()
                .set_title("Choose a video file")
                .add_filter("Video", &VIDEO_EXTENSIONS)
                .pick_file();
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ui;
        None
    }
}

#[cfg(test)]
mod tests;
