//! The runner's command line.
//!
//! ```text
//! eval-runner --config studio.toml --trace run.jsonl [--seconds S]
//!             [--replay passes.jsonl [--tolerance-ms MS]]
//!             [--record-detections passes.jsonl] [--latency-ticks N]
//! ```
//!
//! Output paths must not exist yet: a run never overwrites an earlier
//! trace or log. Parsing only checks the shape of the arguments; the run
//! checks what they say (`crate::run`).

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

use crate::run::RunOptions;

/// The runner's arguments; see the module docs.
#[derive(Debug, Clone, PartialEq, Parser)]
#[command(
    name = "eval-runner",
    version,
    about = "Steps the engine over a synthetic or decoded clip on simulated time and writes \
             the per-tick trace"
)]
pub struct Cli {
    /// Studio config (TOML, or JSON for a `.json` file).
    #[arg(long, value_name = "PATH")]
    pub config: PathBuf,
    /// Write one trace record per tick here, as JSON Lines (the file must
    /// not exist).
    #[arg(long, value_name = "PATH")]
    pub trace: PathBuf,
    /// Simulated seconds to run; needed unless every source is a file that
    /// ends.
    #[arg(long, value_name = "SECONDS")]
    pub seconds: Option<f64>,
    /// Answer discovery from this detection log instead of the studio's
    /// detector.
    #[arg(long, value_name = "PATH")]
    pub replay: Option<PathBuf>,
    /// With --replay: answer a frame from a pass at most this many
    /// milliseconds from it (default: half the source's frame interval).
    #[arg(long, value_name = "MS", requires = "replay")]
    pub tolerance_ms: Option<f64>,
    /// Write every applied detector pass here, as JSON Lines (the file
    /// must not exist).
    #[arg(long, value_name = "PATH")]
    pub record_detections: Option<PathBuf>,
    /// Ticks from a discovery request to its answer.
    #[arg(long, value_name = "TICKS", default_value_t = 1,
          value_parser = clap::value_parser!(u32).range(1..))]
    pub latency_ticks: u32,
}

impl Cli {
    /// The run's length and pacing.
    pub fn run_options(&self) -> RunOptions {
        RunOptions {
            seconds: self.seconds,
            latency_ticks: self.latency_ticks,
        }
    }

    /// The replay tolerance asked for, if any; refuses one that is not a
    /// finite number of milliseconds of at least 0.
    pub fn tolerance(&self) -> Result<Option<Duration>, String> {
        self.tolerance_ms
            .map(|ms| {
                Duration::try_from_secs_f64(ms / 1000.0).map_err(|_| {
                    format!("--tolerance-ms must be a finite number, at least 0; got {ms}")
                })
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `args` after the program name.
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("eval-runner").chain(args.iter().copied()))
    }

    #[test]
    fn the_config_and_the_trace_are_enough() {
        let cli = parse(&["--config", "s.toml", "--trace", "t.jsonl"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("s.toml"));
        assert_eq!(cli.trace, PathBuf::from("t.jsonl"));
        assert_eq!(
            (cli.seconds, &cli.replay, &cli.record_detections),
            (None, &None, &None)
        );
        assert_eq!(cli.run_options(), RunOptions::default());
        assert_eq!(cli.tolerance(), Ok(None));
    }

    #[test]
    fn every_option_is_read() {
        let cli = parse(&[
            "--config",
            "s.toml",
            "--trace",
            "t.jsonl",
            "--seconds",
            "2.5",
            "--replay",
            "in.jsonl",
            "--tolerance-ms",
            "50",
            "--record-detections",
            "out.jsonl",
            "--latency-ticks",
            "3",
        ])
        .unwrap();
        assert_eq!(cli.replay, Some(PathBuf::from("in.jsonl")));
        assert_eq!(cli.record_detections, Some(PathBuf::from("out.jsonl")));
        assert_eq!(
            cli.run_options(),
            RunOptions {
                seconds: Some(2.5),
                latency_ticks: 3
            }
        );
        assert_eq!(cli.tolerance(), Ok(Some(Duration::from_millis(50))));
    }

    #[test]
    fn missing_or_malformed_arguments_are_refused() {
        assert!(parse(&["--trace", "t.jsonl"]).is_err(), "no config");
        assert!(parse(&["--config", "s.toml"]).is_err(), "no trace");
        let base = ["--config", "s.toml", "--trace", "t.jsonl"];
        let with = |extra: &[&str]| parse(&[&base[..], extra].concat());
        assert!(with(&["--latency-ticks", "0"]).is_err());
        assert!(with(&["--seconds", "soon"]).is_err());
        assert!(with(&["--tolerance-ms", "5"]).is_err(), "needs --replay");
        let cli = with(&["--replay", "in.jsonl", "--tolerance-ms=-5"]).unwrap();
        let err = cli.tolerance().unwrap_err();
        assert!(err.contains("--tolerance-ms"), "{err}");
    }
}
