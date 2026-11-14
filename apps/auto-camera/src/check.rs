//! `--check`: the readiness check, printed for the operator.
//!
//! The check itself is the engine's ([`ac_engine::readiness`]): it reads
//! the config, lists the problems in its values, and looks at the files,
//! tools and folders the config names, without opening a camera, a model,
//! a network connection or the sidecar. This module prints what it found on
//! stdout, each finding on a line of its own marked `ok`, `warning` or
//! `problem` (a message of several lines, such as a parse error's excerpt of
//! the file, goes on under it, indented past the mark), then a verdict
//! line, and turns the verdict into the exit status: 0 when nothing was
//! found that would stop the studio (warnings allowed), [`NOT_READY`] (2,
//! as for a config that does not load) otherwise. When the default config
//! path names no file, a hint says that it is relative to the working
//! folder; a default config that is there gets none, whatever its problems.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use ac_engine::readiness::{self, Level};
use ac_engine::{Readiness, SystemInspector};

/// Exit status when the check found a problem.
pub const NOT_READY: u8 = 2;

/// Printed about the default config path (`config/show.toml`, relative to
/// the working directory), by `--check` and by a run when nothing is
/// there or it does not load: how to make the operator's studio from an
/// example, since a bare launch no longer falls back to the demo.
pub const DEFAULT_CONFIG_HINT: &str = concat!(
    "  Start the window without --headless to choose a video file or cameras,\n",
    "  or make your studio from an example, edit it, then start again:\n",
    "    cp config/file-source.example.toml config/show.toml   # a video file\n",
    "    cp config/capture.example.toml config/show.toml       # cameras\n",
    "  Run from the repository root, or pass --config PATH; --demo opens the demo.",
);

/// Width of a finding's mark column, `ok`, `warning` or `problem`.
const MARK: usize = 8;

/// Checks the studio at `config` on this machine, prints the report and
/// returns the exit status; `default_config` says the path was not given.
pub fn run(config: &Path, default_config: bool) -> ExitCode {
    let found = readiness::check_file(config, &SystemInspector::default());
    let hint = default_config && !config.exists();
    print!("{}", report(config, &found, hint));
    if found.is_ready() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(NOT_READY)
    }
}

/// The printed report of `found` for the studio at `config`, with the
/// default path's hint when `hint`.
fn report(config: &Path, found: &Readiness, hint: bool) -> String {
    let mut out = format!("Checking {}\n", config.display());
    for f in &found.findings {
        let mark = match f.level {
            Level::Ok => "ok",
            Level::Warning => "warning",
            Level::Problem => "problem",
        };
        let mut lines = f
            .message
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty());
        let _ = writeln!(out, "  {mark:<MARK$} {}", lines.next().unwrap_or(""));
        for line in lines {
            let _ = writeln!(out, "  {:<MARK$} {line}", "");
        }
    }
    if hint {
        let _ = writeln!(out, "{DEFAULT_CONFIG_HINT}");
    }
    let problems = found.problems().count();
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
    fn problems_are_counted_and_a_missing_default_path_gets_a_hint() {
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

    #[test]
    fn a_message_of_several_lines_goes_on_under_its_mark() {
        let excerpt = "  |\n9 | keyframe_seconds = \"two\"\n  |                    ^^^^^\n";
        let message = format!(
            "cannot parse s.toml: TOML parse error at line 9, column 20\n{excerpt}\
             invalid type: string \"two\", expected u32\n\n"
        );
        let parse = found(&[(Level::Problem, message.as_str())]);
        let pad = " ".repeat(2 + MARK + 1);
        assert_eq!(
            report(Path::new("s.toml"), &parse, false),
            format!(
                "Checking s.toml\n\
                 \x20 problem  cannot parse s.toml: TOML parse error at line 9, column 20\n\
                 {pad}  |\n\
                 {pad}9 | keyframe_seconds = \"two\"\n\
                 {pad}  |                    ^^^^^\n\
                 {pad}invalid type: string \"two\", expected u32\n\
                 1 problem found; fix it and check again.\n"
            ),
            "the excerpt keeps its own alignment; the blank line is dropped"
        );
    }
}
