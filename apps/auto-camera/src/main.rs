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
//! - **Audio devices** (`--list-audio-devices`, [`audio_devices`]): starts
//!   nothing and reads no config; lists the Mac's audio input devices with
//!   the UIDs a `kind = "device"` audio input names.
//!
//! `--config` opens a studio file or a show saved from one ([`show`]): the
//! window's Save show writes the queries, director and transition to a
//! show file (`--save-show PATH`, or a default one), which opens again as
//! its studio file with the show laid over it. A `--save-show` path the
//! window cannot save to (the studio file itself) is a usage error before
//! anything starts.
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
//! `--record-detections PATH` writes every detector pass the engine
//! applied the same way ([`detections`]), for the eval runner to replay.
//!
//! On macOS the system is kept awake while output runs in the window, and
//! for the whole of a headless run ([`awake`]); elsewhere a note on stderr
//! and in the log says it is not.
//!
//! Every window and headless run keeps a log and writes a crash report on
//! a panic ([`logs`]); a check writes none. The warnings and errors the app
//! prints on stderr go to the log too.
//!
//! Exit status: 0 on success, 1 when the engine, the output, the trace or
//! the detection log failed (or the audio devices could not be listed), 2
//! for usage and configuration errors and when `--check` found a problem,
//! 130 after a termination signal.

mod audio_devices;
mod awake;
mod check;
mod cli;
mod detections;
mod headless;
mod logs;
mod show;
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
    if cli.list_audio_devices {
        return audio_devices::run();
    }
    let mut log = logs::LogSession::start(cli.log_dir.as_deref());
    let quit = QuitSignal::new();
    if let Err(e) = quit.install() {
        eprintln!("auto-camera: {e}");
        tracing::error!("{e}");
    }
    let loaded = match StudioConfig::load_with_warnings(&cli.config) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("auto-camera: {e}");
            tracing::error!("{e}");
            if default_config {
                eprintln!("{}", check::DEFAULT_CONFIG_HINT);
            }
            return ExitCode::from(2);
        }
    };
    for warning in &loaded.warnings {
        eprintln!("auto-camera: warning: {warning}");
        tracing::warn!("{warning}");
    }
    log.studio(&cli.config, &loaded);
    if cli.headless {
        let options = headless::HeadlessOptions {
            seconds: cli.seconds,
            record: cli.record,
            trace: cli.trace,
            record_detections: cli.record_detections,
        };
        let summary = headless::run(loaded.config, &cli.config, &options, &quit);
        match &summary.error {
            Some(e) => tracing::error!("headless run failed: {e}"),
            None => tracing::info!("headless run done"),
        }
        match serde_json::to_string_pretty(&summary) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("auto-camera: cannot print the summary: {e}");
                tracing::error!("cannot print the summary: {e}");
            }
        }
        if quit.requested() {
            ExitCode::from(signals::INTERRUPTED)
        } else if summary.ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    } else {
        let target = show::ShowTarget::for_run(
            &loaded,
            &cli.config,
            cli.save_show.as_deref(),
            show::shows_dir(),
        );
        if let (Some(_), Err(e)) = (&cli.save_show, &target) {
            eprintln!("auto-camera: --save-show: {e}");
            tracing::error!("--save-show: {e}");
            return ExitCode::from(2);
        }
        let passes = cli.record_detections.as_deref();
        run_window(
            loaded.config,
            &loaded.warnings,
            cli.trace.as_deref(),
            passes,
            target,
            &quit,
        )
    }
}

/// Runs the operator window until it is closed, opening with the config's
/// `warnings` in its status bar, writing the per-tick trace to `trace` and
/// the applied detector passes to `log` if given, and saving the show to
/// `target` when the operator asks; see the module docs for the exit
/// status.
fn run_window(
    config: StudioConfig,
    warnings: &[String],
    trace: Option<&Path>,
    log: Option<&Path>,
    target: Result<show::ShowTarget, String>,
    quit: &QuitSignal,
) -> ExitCode {
    let (trace, options) = match trace::attach(trace, EngineOptions::default()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("auto-camera: {e}");
            tracing::error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let (log, options) = match detections::attach(log, options) {
        Ok(d) => d,
        Err(e) => {
            // The trace's sender went with the options: its file is empty.
            let _ = trace.map(trace::TraceFile::discard_if_empty);
            eprintln!("auto-camera: {e}");
            tracing::error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let result = ui::run(config, options, warnings, target, quit.clone());
    // The window's engine has stopped, and its senders with it. A window
    // that failed, most often because its engine did not start, leaves no
    // empty trace or log behind.
    let report = trace.map(|t| match result {
        Ok(()) => t.finish(),
        Err(_) => t.discard_if_empty(),
    });
    let log_report = log.map(|l| match result {
        Ok(()) => l.finish(),
        Err(_) => l.discard_if_empty(),
    });
    let trace_error = report.and_then(|r| r.error);
    let log_error = log_report.and_then(|r| r.error);
    if let Err(e) = &result {
        eprintln!("auto-camera: {e}");
        tracing::error!("{e}");
    }
    for e in trace_error.iter().chain(&log_error) {
        eprintln!("auto-camera: {e}");
        tracing::error!("{e}");
    }
    match result {
        Ok(()) if quit.requested() => ExitCode::from(signals::INTERRUPTED),
        Ok(()) if trace_error.is_none() && log_error.is_none() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
