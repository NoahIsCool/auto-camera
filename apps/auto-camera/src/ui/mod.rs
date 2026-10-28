//! The native operator window (eframe/egui).
//!
//! The UI is immediate-mode and stateless with respect to the studio: every
//! frame is drawn from the newest [`ac_engine::EngineSnapshot`], and every
//! operator action becomes an [`ac_engine::EngineCommand`]. The engine
//! thread owns all studio state, so the window can never disagree with
//! what is on air; at worst it is one snapshot behind.
//!
//! The code is split so that everything but the drawing is testable
//! without a window: [`layout`] (tile grid and monitor geometry), [`keys`]
//! (shortcut mapping), [`text`] (labels), [`textures`] (upload decisions),
//! [`close_guard`] (asking before a live output is closed or stopped),
//! [`notices`] (what the status bar's notice says, and for how long),
//! [`alarms`] (its persistent list of what is wrong on air) and
//! the forms ([`query_form`], which reads prompts the way the engine will,
//! and the output form in [`panels`]). [`console`] draws a whole frame from
//! a snapshot and returns commands, apart from the engine that
//! [`app`] owns, so the window itself is tested headless: egui's test
//! harness (`egui_kittest`, without a renderer) presses keys and clicks
//! widgets found through the AccessKit tree, with no GPU or display.
//!
//! eframe runs with the OpenGL (glow) renderer and, on Linux, X11 (Wayland
//! sessions use XWayland); both load their system libraries at run time.

mod alarms;
mod app;
mod close_guard;
mod console;
#[cfg(test)]
mod fixtures;
mod keys;
mod layout;
mod multiview;
mod notices;
mod panels;
mod query_form;
mod switching;
mod text;
mod textures;

use ac_engine::{Engine, EngineOptions, StudioConfig};
use eframe::egui;

use crate::signals::QuitSignal;
use app::OperatorApp;

/// Starts the engine from `config` with `options` and runs the window until
/// it is closed, or until `quit` is requested (a termination signal), which
/// closes it. The window opens with `warnings`, the config's, in its status
/// bar. The engine has stopped when this returns, whether the window opened
/// or not.
pub fn run(
    config: StudioConfig,
    options: EngineOptions,
    warnings: &[String],
    quit: QuitSignal,
) -> Result<(), String> {
    let engine = Engine::start_with(config.clone(), options).map_err(|e| e.to_string())?;
    let warnings = warnings.to_vec();
    let title = match config.sources.as_slice() {
        [one] => format!("Auto Camera · {}", one.display_name()),
        many => format!("Auto Camera · {} sources", many.len()),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "auto-camera",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            quit.wake_with(move || {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            });
            Ok(Box::new(
                OperatorApp::new(engine, &config)
                    .with_quit(quit)
                    .with_warnings(&warnings),
            ))
        }),
    )
    .map_err(|e| format!("cannot open the operator window: {e}"))
}
