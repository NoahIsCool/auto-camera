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
//! (shortcut mapping), [`text`] (labels), [`caption`] (the preview
//! monitor's notes on a waiting take and a lost subject), [`textures`]
//! (upload decisions),
//! [`close_guard`] (asking before a live output is closed or stopped),
//! [`notices`] (what the status bar's notice says, and for how long),
//! [`alarms`] (its persistent list of what is wrong on air), the forms
//! ([`query_form`], which reads prompts the way the engine will, and the
//! output form in [`panels`]) and Save show's saver ([`save`], which
//! writes off the window's thread). [`console`] draws a whole frame from
//! a snapshot and returns commands, apart from the engine that
//! [`app`] owns, so the window itself is tested headless: egui's test
//! harness (`egui_kittest`, without a renderer) presses keys and clicks
//! widgets found through the AccessKit tree, with no GPU or display.
//!
//! The window's look (palette, type scale, spacing) is one module,
//! [`theme`], installed as the window opens; the header bar ([`header`])
//! carries the tally lamps (ON AIR, REC, the director's mode) and the
//! studio clock, and [`controls`] the switcher's transition block.
//!
//! eframe runs with the OpenGL (glow) renderer and, on Linux, X11 (Wayland
//! sessions use XWayland); both load their system libraries at run time.

mod alarms;
mod app;
mod caption;
mod close_guard;
mod console;
mod controls;
#[cfg(test)]
mod fixtures;
mod header;
mod keys;
mod layout;
mod meter;
mod multiview;
mod notices;
mod panels;
mod query_form;
mod save;
mod switching;
mod text;
mod textures;
mod theme;

use ac_engine::{Engine, EngineOptions, StudioConfig};
use eframe::egui;

use crate::show::ShowTarget;
use crate::signals::QuitSignal;
use app::OperatorApp;

/// Starts the engine from `config` with `options` and runs the window until
/// it is closed, or until `quit` is requested (a termination signal), which
/// closes it. The window opens with `warnings`, the config's, in its status
/// bar, and Save show writes to `show`. The engine has stopped when this
/// returns, whether the window opened or not.
pub fn run(
    config: StudioConfig,
    options: EngineOptions,
    warnings: &[String],
    show: Result<ShowTarget, String>,
    quit: QuitSignal,
) -> Result<(), String> {
    let engine = Engine::start_with(config.clone(), options).map_err(|e| e.to_string())?;
    let _watch = crate::logs::watch(engine.handle());
    let warnings = warnings.to_vec();
    let title = format!("Auto Camera · {}", header::studio_title(&config));
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
            theme::apply(&ctx);
            quit.wake_with(move || {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            });
            Ok(Box::new(
                OperatorApp::new(engine, &config)
                    .with_quit(quit)
                    .with_warnings(&warnings)
                    .with_show_target(show),
            ))
        }),
    )
    .map_err(|e| format!("cannot open the operator window: {e}"))
}
