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
//! - **Check** (`--check`, [`check`]): starts nothing; lists the problems
//!   with the config and the files, tools and folders it names, so a show
//!   does not find them at its start.
//!
//! Ctrl-C, `SIGTERM` and `SIGHUP` shut the engine down as closing the
//! window does ([`signals`]), so the detector sidecar never outlives the
//! app; a second signal exits at once. Warnings about the config (a missing
//! `schema` key) go to stderr, and the window shows them in its status bar.
//!
//! In either mode `--trace PATH` writes the engine's per-tick trace as JSON
//! Lines ([`trace`]): the file is created before the engine starts, the
//! engine is given its channel, and the app waits for the last line once
//! the engine has stopped. A run whose engine does not start (a setup no
//! retry mends, such as a detector sidecar whose program is missing)
//! removes the empty file, so that the same command works once the cause
//! is fixed. A source that cannot be opened does not stop the engine (it
//! starts offline and is retried), so a run with a missing source file
//! ticks and, like every run that ticked, keeps its trace.
//!
//! On macOS the system is kept awake while output runs in the window, and
//! for the whole of a headless run ([`awake`]); elsewhere a note on stderr
//! says it is not.
//!
//! Exit status: 0 on success, 1 when the engine, the output or the trace
//! failed, 2 for usage and configuration errors and when `--check` found a
//! problem, 130 after a termination signal.

mod awake;
mod check;
mod cli;
mod headless;
mod signals;
#[cfg(test)]
mod testing;
mod trace;
mod ui;

use std::path::Path;
use std::process::ExitCode;

use ac_engine::{EngineOptions, StudioConfig};
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
            trace: cli.trace,
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
        run_window(config, &warnings, cli.trace.as_deref(), &quit)
    }
}

/// Runs the operator window until it is closed, opening with the config's
/// `warnings` in its status bar and writing the per-tick trace to `trace`
/// if given; see the module docs for the exit status.
fn run_window(
    config: StudioConfig,
    warnings: &[String],
    trace: Option<&Path>,
    quit: &QuitSignal,
) -> ExitCode {
    let (trace, options) = match trace::attach(trace, EngineOptions::default()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("auto-camera: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = ui::run(config, options, warnings, quit.clone());
    // The window's engine has stopped, and its trace sender with it. A
    // window that failed, most often because its engine did not start,
    // leaves no empty trace behind.
    let report = trace.map(|t| match result {
        Ok(()) => t.finish(),
        Err(_) => t.discard_if_empty(),
    });
    let trace_error = report.and_then(|r| r.error);
    if let Err(e) = &result {
        eprintln!("auto-camera: {e}");
    }
    if let Some(e) = &trace_error {
        eprintln!("auto-camera: {e}");
    }
    match result {
        Ok(()) if quit.requested() => ExitCode::from(signals::INTERRUPTED),
        Ok(()) if trace_error.is_none() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
