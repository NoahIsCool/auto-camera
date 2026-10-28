//! Command-line interface.
//!
//! ```text
//! auto-camera [--config PATH] [--trace PATH] [--save-show PATH]  # operator window
//! auto-camera [--config PATH] --headless [--seconds N] [--record PATH] [--trace PATH]
//! auto-camera [--config PATH] --check                           # readiness check
//! ```
//!
//! The headless options are only accepted together with `--headless`, so a
//! typo cannot silently open a window that records nothing, and `--check`
//! is accepted only on its own, since it starts nothing. `--trace` works in
//! both other modes: it writes the engine's per-tick trace as JSON Lines
//! ([`crate::trace`]). `--save-show` is the window's alone: the show file
//! its Save show writes ([`crate::show`]); a headless run changes nothing
//! a show holds, so it has nothing to save and refuses the flag.
//! `--config` opens a studio file or a saved show. The config path
//! defaults to the demo studio relative to the working directory, which is
//! the repository root under `cargo run`.

use std::path::PathBuf;

use clap::Parser;

/// Default studio configuration.
pub const DEFAULT_CONFIG: &str = "config/demo.toml";

/// Longest headless run accepted: one day.
const MAX_SECONDS: f64 = 86_400.0;

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Parser)]
#[command(
    name = "auto-camera",
    version,
    about = "AI camera crew: framed virtual shots, preview/program switching, RTMP and MP4 output"
)]
pub struct Cli {
    /// Studio configuration, or a saved show: TOML, or JSON when the file
    /// ends in `.json`.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_CONFIG)]
    pub config: PathBuf,
    /// Run without a window for `--seconds` and print a JSON summary.
    #[arg(long)]
    pub headless: bool,
    /// Length of a headless run in seconds (after the detector has loaded
    /// and recording has started).
    #[arg(
        long,
        value_name = "N",
        default_value_t = 10.0,
        requires = "headless",
        value_parser = parse_seconds
    )]
    pub seconds: f64,
    /// Record program to this MP4 during a headless run; it must not exist.
    #[arg(long, value_name = "PATH", requires = "headless")]
    pub record: Option<PathBuf>,
    /// Write one JSON line per engine tick (subjects, shot targets and
    /// crops, program and preview) to this file; it must not exist.
    #[arg(long, value_name = "PATH")]
    pub trace: Option<PathBuf>,
    /// Save the show (queries, director, transition) to this file when
    /// Save show is pressed in the window. Without it the window saves to
    /// the show file it opened, or to the data dir's `shows` folder.
    #[arg(long, value_name = "PATH", conflicts_with = "headless")]
    pub save_show: Option<PathBuf>,
    /// Check the studio without starting it: list the problems with the
    /// config and the files, tools and folders it names, then exit 0 when
    /// there is none and 2 otherwise.
    #[arg(long, conflicts_with_all = ["headless", "seconds", "record", "trace", "save_show"])]
    pub check: bool,
}

/// Parses a positive, finite number of seconds up to one day.
fn parse_seconds(text: &str) -> Result<f64, String> {
    let v: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("{text:?} is not a number"))?;
    if v.is_finite() && v > 0.0 && v <= MAX_SECONDS {
        Ok(v)
    } else {
        Err(format!("must be more than 0 and at most {MAX_SECONDS}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `args` after the program name.
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("auto-camera").chain(args.iter().copied()))
    }

    #[test]
    fn defaults_open_the_demo_in_a_window() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.config, PathBuf::from(DEFAULT_CONFIG));
        assert!(!cli.headless);
        assert!(!cli.check);
        assert_eq!(cli.record, None);
        assert_eq!(cli.trace, None);
    }

    #[test]
    fn both_modes_take_a_trace() {
        let window = parse(&["--trace", "/tmp/t.jsonl"]).unwrap();
        assert!(!window.headless);
        assert_eq!(window.trace, Some(PathBuf::from("/tmp/t.jsonl")));
        let headless = parse(&["--headless", "--trace", "t.jsonl"]).unwrap();
        assert_eq!(headless.trace, Some(PathBuf::from("t.jsonl")));
        assert!(parse(&["--trace"]).is_err(), "a path is required");
    }

    #[test]
    fn check_runs_alone() {
        let cli = parse(&["--check", "--config", "show.toml"]).unwrap();
        assert!(cli.check && !cli.headless);
        assert_eq!(cli.config, PathBuf::from("show.toml"));
        assert!(parse(&["--check"]).unwrap().check);
        assert!(parse(&["--check", "--headless"]).is_err());
        assert!(parse(&["--check", "--record", "x.mp4"]).is_err());
        assert!(parse(&["--check", "--seconds", "3"]).is_err());
    }

    #[test]
    fn check_takes_no_trace() {
        // A check starts no engine, so there is no trace to write.
        assert!(parse(&["--check", "--trace", "t.jsonl"]).is_err());
    }

    #[test]
    fn the_window_takes_a_show_file_to_save_to() {
        assert_eq!(parse(&[]).unwrap().save_show, None);
        let window = parse(&["--save-show", "sunday.show.toml"]).unwrap();
        assert_eq!(window.save_show, Some(PathBuf::from("sunday.show.toml")));
        assert!(parse(&["--save-show"]).is_err(), "a path is required");
        // A headless run changes nothing a show holds: nothing to save.
        assert!(parse(&["--headless", "--save-show", "s.toml"]).is_err());
        // A check starts no engine, so there is no show to save.
        assert!(parse(&["--check", "--save-show", "s.toml"]).is_err());
    }

    #[test]
    fn headless_runs_take_seconds_and_a_recording() {
        let cli = parse(&[
            "--config",
            "studio.json",
            "--headless",
            "--seconds",
            "2.5",
            "--record",
            "/tmp/x.mp4",
        ])
        .unwrap();
        assert_eq!(cli.config, PathBuf::from("studio.json"));
        assert!(cli.headless);
        assert_eq!(cli.seconds, 2.5);
        assert_eq!(cli.record, Some(PathBuf::from("/tmp/x.mp4")));
        assert_eq!(parse(&["--headless"]).unwrap().seconds, 10.0);
    }

    #[test]
    fn headless_options_require_headless() {
        assert!(parse(&["--seconds", "3"]).is_err());
        assert!(parse(&["--record", "x.mp4"]).is_err());
    }

    #[test]
    fn invalid_values_are_rejected() {
        for bad in ["0", "-1", "nan", "inf", "abc", "100000"] {
            assert!(parse(&["--headless", "--seconds", bad]).is_err(), "{bad}");
        }
        assert!(parse(&["--frobnicate"]).is_err());
        assert_eq!(parse_seconds(" 1e1 "), Ok(10.0));
    }
}
