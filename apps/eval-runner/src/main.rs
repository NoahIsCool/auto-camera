//! `eval-runner`: runs a studio over its sources on simulated time and
//! writes the trace; see the library docs.
//!
//! The binary owns the file I/O: it reads the config and the replayed log,
//! sets the run up (so a bad config, input or log fails before anything is
//! written), creates the output files (refusing ones that exist), runs, and
//! prints the run's report as JSON on stdout. A run that fails before its
//! first tick removes the files it created; one that fails later keeps the
//! records written so far, which the error message says. The exit status
//! is 0 for a finished run, 1 for a failed one and 2 for bad arguments.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ac_engine::config::StudioConfig;
use ac_sources::VirtualClock;
use ac_vision::parse_pass_log;
use clap::Parser;
use eval_runner::{Cli, Detections, RunError, RunReport, Runner};

/// Parses the arguments, runs, and reports; see the module docs.
fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(report) => {
            let json = serde_json::to_string_pretty(&report).unwrap_or_default();
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("eval-runner: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Sets up and runs what `cli` asks for; see the module docs.
fn run(cli: &Cli) -> Result<RunReport, String> {
    let config = StudioConfig::load(&cli.config).map_err(|e| e.to_string())?;
    let detections = match &cli.replay {
        None => Detections::Live,
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read the detection log {}: {e}", path.display()))?;
            let log = parse_pass_log(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            Detections::Replay {
                log,
                tolerance: cli.tolerance()?,
            }
        }
    };
    let runner =
        Runner::new(&config, &detections, &cli.run_options()).map_err(|e| e.to_string())?;
    let mut outputs = Outputs::default();
    let mut trace = outputs.create(&cli.trace, "trace")?;
    let mut log = cli
        .record_detections
        .as_deref()
        .map(|path| outputs.create(path, "detection log"))
        .transpose()?;
    let clock = VirtualClock::new();
    let result = runner.run(
        &clock,
        &mut trace,
        log.as_mut().map(|w| w as &mut dyn Write),
    );
    drop((trace, log));
    match result {
        Ok(report) => Ok(report),
        Err(e @ (RunError::Setup(_) | RunError::Source { ticks: 0, .. })) => {
            outputs.remove();
            Err(e.to_string())
        }
        Err(e) => Err(format!(
            "{e} (the records written so far are kept in {})",
            outputs.list()
        )),
    }
}

/// The files a run created, to remove when it fails before writing.
#[derive(Debug, Default)]
struct Outputs(Vec<PathBuf>);

impl Outputs {
    /// Creates the `what` file at `path`, which must not exist yet.
    fn create(&mut self, path: &Path, what: &str) -> Result<BufWriter<File>, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("cannot create the {what} {}: {e}", path.display()));
        match file {
            Ok(file) => {
                self.0.push(path.to_path_buf());
                Ok(BufWriter::new(file))
            }
            Err(e) => {
                self.remove();
                Err(e)
            }
        }
    }

    /// Removes every file created.
    fn remove(&mut self) {
        for path in self.0.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The files created, for a message.
    fn list(&self) -> String {
        let names: Vec<String> = self.0.iter().map(|p| p.display().to_string()).collect();
        names.join(" and ")
    }
}
