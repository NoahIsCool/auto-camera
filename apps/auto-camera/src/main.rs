//! `auto-camera`: the operator app.
//!
//! The app is a thin shell around [`ac_engine::Engine`], which runs the
//! whole pipeline (sources, discovery, tracking, framing, switching,
//! compositing, output) on its own threads. The shell reads the engine's
//! immutable snapshots and sends it commands; it owns no studio state of its
//! own beyond what the operator is typing. It runs in one of two ways:
//!
//! - **Window** (default, [`ui`]): a native eframe/egui operator console with
//!   preview and program monitors, the multiview, transition controls, the
//!   query, director and output panels and a status bar.
//! - **Headless** (`--headless`, [`headless`]): runs the engine for a fixed
//!   time, optionally recording program, and prints a JSON summary, for
//!   demos, CI and soak tests.
//!
//! - **Check** (`--check`, [`check`]): starts nothing; lists every problem
//!   with the config and the files, tools and folders it names, so a show
//!   does not find them at its start.
//!
//! Ctrl-C, `SIGTERM` and `SIGHUP` shut the engine down as closing the
//! window does ([`signals`]), so the detector sidecar never outlives the
//! app; a second signal exits at once. Warnings about the config (a missing
//! `schema` key) go to stderr, and the window shows them in its status bar.
//!
//! Exit status: 0 on success, 1 when the engine or the output failed, 2 for
//! usage and configuration errors and when `--check` found a problem, 130
//! after a termination signal.

mod check;
mod cli;
mod headless;
mod signals;
mod ui;

use std::process::ExitCode;

use ac_engine::StudioConfig;
use clap::Parser;

use cli::Cli;
use signals::QuitSignal;

/// Parses the command line, then checks the studio, or loads the config
/// and runs the window or a headless run; see the module docs for the exit
/// status.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let default_config = cli.config.as_os_str() == cli::DEFAULT_CONFIG;
    if cli.check {
        return check::run(&cli.config, default_config);
    }
    let quit = QuitSignal::new();
    if let Err(e) = quit.install() {
        eprintln!("auto-camera: {e}");
    }
    let (config, warnings) = match StudioConfig::load_with_warnings(&cli.config) {
        Ok(loaded) => (loaded.config, loaded.warnings),
        Err(e) => {
            eprintln!("auto-camera: {e}");
            if default_config {
                eprintln!("{}", check::DEFAULT_CONFIG_HINT);
            }
            return ExitCode::from(2);
        }
    };
    for warning in &warnings {
        eprintln!("auto-camera: warning: {warning}");
    }
    if cli.headless {
        let options = headless::HeadlessOptions {
            seconds: cli.seconds,
            record: cli.record,
        };
        let summary = headless::run(config, &cli.config, &options, &quit);
        match serde_json::to_string_pretty(&summary) {
            Ok(json) => println!("{json}"),
            Err(e) => eprintln!("auto-camera: cannot print the summary: {e}"),
        }
        if quit.requested() {
            ExitCode::from(signals::INTERRUPTED)
        } else if summary.ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    } else {
        match ui::run(config, &warnings, quit.clone()) {
            Ok(()) if quit.requested() => ExitCode::from(signals::INTERRUPTED),
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("auto-camera: {e}");
                ExitCode::FAILURE
            }
        }
    }
}
