//! `--check`: the readiness check, printed for the operator.
//!
//! The check itself is the engine's ([`ac_engine::readiness`]): it reads
//! the config, lists every problem in it, and looks at the files, tools
//! and folders the config names, without opening a camera, a model, a
//! network connection or the sidecar. This module prints what it found on
//! stdout, one finding per line marked `ok`, `warning` or `problem`, then a
//! verdict line, and turns the verdict into the exit status: 0 when nothing
//! was found that would stop the studio (warnings allowed), [`NOT_READY`]
//! (2, as for a config that does not load) otherwise.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use ac_engine::readiness::{self, Level};
use ac_engine::{Readiness, SystemInspector};

/// Exit status when the check found a problem.
pub const NOT_READY: u8 = 2;

/// Printed under a problem with the default config path, which is
/// relative to the working directory.
pub const DEFAULT_CONFIG_HINT: &str = "(run from the repository root, or pass --config PATH)";

/// Checks the studio at `config` on this machine, prints the report and
/// returns the exit status; `default_config` says the path was not given.
pub fn run(config: &Path, default_config: bool) -> ExitCode {
    let found = readiness::check_file(config, &SystemInspector::default());
    print!("{}", report(config, &found, default_config));
    if found.is_ready() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(NOT_READY)
    }
}

/// The printed report of `found` for the studio at `config`.
fn report(config: &Path, found: &Readiness, default_config: bool) -> String {
    let mut out = format!("Checking {}\n", config.display());
    for f in &found.findings {
        let mark = match f.level {
            Level::Ok => "ok",
            Level::Warning => "warning",
            Level::Problem => "problem",
        };
        let _ = writeln!(out, "  {mark:<8} {}", f.message);
    }
    let problems = found.problems().count();
    if problems > 0 && default_config {
        let _ = writeln!(out, "  {DEFAULT_CONFIG_HINT}");
    }
    match problems {
        0 => out.push_str("Ready: nothing found that would stop this studio.\n"),
        1 => out.push_str("1 problem found; fix it and check again.\n"),
        n => {
            let _ = writeln!(out, "{n} problems found; fix them and check again.");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_engine::readiness::Finding;

    /// A report of findings at the given levels.
    fn found(levels: &[(Level, &str)]) -> Readiness {
        Readiness {
            findings: levels
                .iter()
                .map(|(level, message)| Finding {
                    level: *level,
                    message: (*message).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_ready_studio_lists_what_was_checked() {
        let r = found(&[
            (Level::Ok, "output.ffmpeg: ffmpeg runs (ffmpeg version 7.1)"),
            (Level::Warning, "studio.toml: no `schema` key"),
        ]);
        assert_eq!(
            report(Path::new("studio.toml"), &r, false),
            "Checking studio.toml\n\
             \x20 ok       output.ffmpeg: ffmpeg runs (ffmpeg version 7.1)\n\
             \x20 warning  studio.toml: no `schema` key\n\
             Ready: nothing found that would stop this studio.\n"
        );
    }

    #[test]
    fn problems_are_counted_and_the_default_path_gets_a_hint() {
        let one = found(&[(Level::Problem, "cannot read config/demo.toml")]);
        let text = report(Path::new("config/demo.toml"), &one, true);
        assert!(
            text.contains("  problem  cannot read config/demo.toml\n"),
            "{text}"
        );
        assert!(text.contains(DEFAULT_CONFIG_HINT), "{text}");
        assert!(
            text.ends_with("\n1 problem found; fix it and check again.\n"),
            "{text}"
        );
        let two = found(&[
            (Level::Problem, "a"),
            (Level::Ok, "b"),
            (Level::Problem, "c"),
        ]);
        let text = report(Path::new("show.toml"), &two, false);
        assert!(!text.contains(DEFAULT_CONFIG_HINT), "{text}");
        assert!(
            text.ends_with("\n2 problems found; fix them and check again.\n"),
            "{text}"
        );
    }
}
