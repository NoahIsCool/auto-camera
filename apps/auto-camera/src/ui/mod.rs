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
//! The window opens on [`shell`], which shows the source picker
//! ([`picker`]) when there is no studio file yet and over a running studio
//! when the operator presses Sources, and restarts the engine on the new
//! sources.
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
mod picker;
mod query_form;
mod save;
mod shell;
mod switching;
mod text;
mod textures;
mod theme;

use eframe::egui;

use crate::signals::QuitSignal;
pub use shell::{Opened, Setup, Start};

/// The camera lister the window's source picker uses.
pub const LIST_CAMERAS: picker::Lister = crate::setup::list_cameras;

/// Runs the window on `start` until it is closed, or until `quit` is
/// requested (a termination signal), which closes it: a studio (its
/// engine started here, opening with the config's warnings in its status
/// bar) or the source picker, with `setup` saying where the picker writes
/// and how studios opened from it start ([`shell`]). No engine runs when
/// this returns, whether the window opened or not.
pub fn run(start: Start, setup: Setup, quit: QuitSignal) -> Result<(), String> {
    let shell = shell::Shell::new(start, setup, quit.clone())?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(shell.title())
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_drag_and_drop(true),
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
            Ok(Box::new(shell))
        }),
    )
    .map_err(|e| format!("cannot open the operator window: {e}"))
}
