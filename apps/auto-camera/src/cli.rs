//! Command-line interface.
//!
//! ```text
//! auto-camera [--config PATH]                                   # operator window
//! auto-camera [--config PATH] --headless [--seconds N] [--record PATH]
//! auto-camera [--config PATH] --check                           # readiness check
//! ```
//!
//! The headless options are only accepted together with `--headless`, so a
//! typo cannot silently open a window that records nothing, and `--check`
//! is accepted only on its own, since it starts nothing. The config path
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
    /// Studio configuration: TOML, or JSON when the file ends in `.json`.
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
    /// Check the studio without starting it: list the problems with the
    /// config and the files, tools and folders it names, then exit 0 when
    /// there is none and 2 otherwise.
    #[arg(long, conflicts_with_all = ["headless", "seconds", "record"])]
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
