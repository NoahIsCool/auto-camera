//! The `[detector] kind = "sidecar"` section: a detector that runs in its
//! own process (SAM 3.1 on MLX first), shared by every source.
//!
//! # Theory
//!
//! The section is plain data. It says how to start the process (an argv
//! list, never a shell line, so no quoting rules apply) and how detections
//! are asked for: the longest side of the image sent, a score threshold for
//! every query, and two deadlines. [`crate::detectors`] turns it into an
//! `ac_sidecar` host and one detector per source at engine start.
//!
//! **Paths in the command.** Like the other model paths, relative paths in
//! `command` are resolved against the config file's directory
//! ([`SidecarSettings::resolve_paths`]). An element counts as a path when it
//! contains a `/`, does not start with `-` or `~` and is not a URL (`://`):
//! so `"../models/sam3/.venv/bin/python"` and `"../models/sam31-mlx"` are
//! resolved, while `"python3"` (looked up on `PATH`, as a shell would),
//! `"--side"`, `"672"` and `"--model=x/y"` are passed as written. Nothing
//! expands `~` for the program; an argument starting with `~` is passed as
//! written (`server.py` expands it in `--model`).
//!
//! **Deadlines.** `startup_timeout_s` bounds the wait for the sidecar's
//! hello after a spawn (the model loads in 4.6 s on Noah's M3 Max, measured,
//! plus a warm-up pass); `request_timeout_s` bounds one pass once it reaches
//! the sidecar (about five times the slowest measured pass, 1.05 s for 9
//! prompts at 672 px). The engine itself abandons a request after
//! `discovery.timeout_s`, counted from when it was handed to the source's
//! worker, so the request deadline must be shorter
//! ([`super::StudioConfig::validate`] checks it).
//!
//! **Scores.** SAM and OWLv2 score on different scales. The studio-wide
//! `score_threshold` (default 0.5, SAM's usual cut-off) is also the lowest
//! score a sidecar detection carries, so the subject store's floor follows
//! it when it is lower than the store default
//! ([`super::DetectorSettings::score_floor`]); otherwise boxes between the
//! threshold and the store's 0.35 would be dropped without a word. Measured
//! on Noah's footage, "a person speaking" found the speaker in 10 of 20
//! frames at 0.5 and in 14 of 20 at 0.3.
//!
//! # Invariants
//!
//! * A section that passes [`SidecarSettings::validate`] yields an
//!   `ac_sidecar` host and detector configuration that pass their own
//!   checks.
//! * Resolving paths is idempotent: absolute paths are left alone.

use std::path::Path;
use std::time::Duration;

use ac_core::Lexicon;
use ac_sidecar::{
    DEFAULT_REQUEST_TIMEOUT, DEFAULT_SEND_SIDE, DEFAULT_THRESHOLD, DetectorConfig, HostConfig,
    prep::MAX_SEND_SIDE,
};
use serde::{Deserialize, Serialize};

/// Default longest side of the image sent, in pixels.
fn default_send_side() -> u32 {
    DEFAULT_SEND_SIDE
}

/// Default wait for the sidecar's hello, in seconds.
fn default_startup_timeout_s() -> f64 {
    HostConfig::default().startup_timeout.as_secs_f64()
}

/// Default deadline of one pass, in seconds.
fn default_request_timeout_s() -> f64 {
    DEFAULT_REQUEST_TIMEOUT.as_secs_f64()
}

/// Default score a detection needs.
fn default_score_threshold() -> f32 {
    DEFAULT_THRESHOLD
}

/// Longest accepted startup deadline, in seconds.
const MAX_STARTUP_S: f64 = 3600.0;

/// Longest accepted request deadline, in seconds.
const MAX_REQUEST_S: f64 = 600.0;

/// An out-of-process detector shared by every source; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarSettings {
    /// Program and arguments, such as `["../models/sam3/.venv/bin/python",
    /// "../sidecars/sam3-mlx/server.py", "--model", "../models/sam31-mlx"]`.
    pub command: Vec<String>,
    /// Longest side of the image sent, in pixels (`1..=4096`); frames are
    /// shrunk to it, never enlarged.
    #[serde(default = "default_send_side")]
    pub send_side: u32,
    /// Longest wait for the sidecar's hello after it is started, in seconds.
    #[serde(default = "default_startup_timeout_s")]
    pub startup_timeout_s: f64,
    /// Longest wait for one pass once the sidecar has it, in seconds; below
    /// `discovery.timeout_s`.
    #[serde(default = "default_request_timeout_s")]
    pub request_timeout_s: f64,
    /// Score every query's detections need, in `0..=1`.
    #[serde(default = "default_score_threshold")]
    pub score_threshold: f32,
}

impl SidecarSettings {
    /// Settings for `command` with every other key at its default.
    pub fn new<S: Into<String>>(command: impl IntoIterator<Item = S>) -> Self {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            send_side: default_send_side(),
            startup_timeout_s: default_startup_timeout_s(),
            request_timeout_s: default_request_timeout_s(),
            score_threshold: default_score_threshold(),
        }
    }

    /// Checks the command, the size, the deadlines and the threshold; the
    /// message names the key.
    pub fn validate(&self) -> Result<(), String> {
        let Some(program) = self.command.first() else {
            return Err(
                "detector.command must name the sidecar's program and its arguments, \
                 as in [\"python3\", \"server.py\"]"
                    .into(),
            );
        };
        if program.trim().is_empty() {
            return Err("detector.command[0], the program, is empty".into());
        }
        if let Some(i) = self.command.iter().position(String::is_empty) {
            return Err(format!("detector.command[{i}] is empty"));
        }
        if !(1..=MAX_SEND_SIDE).contains(&self.send_side) {
            return Err(format!(
                "detector.send_side {} must be in 1..={MAX_SEND_SIDE}",
                self.send_side
            ));
        }
        for (name, v, max) in [
            ("startup_timeout_s", self.startup_timeout_s, MAX_STARTUP_S),
            ("request_timeout_s", self.request_timeout_s, MAX_REQUEST_S),
        ] {
            if !(v.is_finite() && v > 0.0 && v <= max) {
                return Err(format!("detector.{name} {v} must be in (0, {max}] seconds"));
            }
        }
        let t = self.score_threshold;
        if !(t.is_finite() && (0.0..=1.0).contains(&t)) {
            return Err(format!("detector.score_threshold {t} must be in 0..=1"));
        }
        Ok(())
    }

    /// Makes the relative paths in `command` relative to `dir`; see the
    /// module docs for what counts as a path.
    pub fn resolve_paths(&mut self, dir: &Path) {
        for arg in &mut self.command {
            if let Some(resolved) = resolve_arg(arg, dir) {
                *arg = resolved;
            }
        }
    }

    /// Elements of `command` that are paths (see the module docs), with
    /// their index; the program comes first when it is one.
    pub fn path_args(&self) -> impl Iterator<Item = (usize, &str)> {
        self.command
            .iter()
            .enumerate()
            .filter(|(_, a)| is_path_arg(a))
            .map(|(i, a)| (i, a.as_str()))
    }

    /// The host configuration (the rarely changed keys keep the host's
    /// defaults: 2 s exit grace, 1 s restart backoff doubling up to 30 s,
    /// 200 log lines).
    pub fn host_config(&self) -> HostConfig {
        HostConfig {
            startup_timeout: Duration::from_secs_f64(self.startup_timeout_s),
            ..HostConfig::new(self.command.iter().cloned())
        }
    }

    /// The detector configuration of every source, telling people from
    /// objects with `lexicon` (the studio's prompt lexicon).
    pub fn detector_config(&self, lexicon: Lexicon) -> DetectorConfig {
        DetectorConfig {
            threshold: self.score_threshold,
            thresholds: Default::default(),
            send_side: self.send_side,
            request_timeout: Duration::from_secs_f64(self.request_timeout_s),
            lexicon,
        }
    }
}

/// True when the command element `arg` is a path: it contains a `/`, does
/// not start with `-` or `~` and is not a URL.
fn is_path_arg(arg: &str) -> bool {
    arg.contains('/') && !arg.starts_with(['-', '~']) && !arg.contains("://")
}

/// `arg` joined to `dir` when it is a relative path; `None` when it stays
/// as written (not a path, already absolute, or not valid UTF-8 once
/// joined).
fn resolve_arg(arg: &str, dir: &Path) -> Option<String> {
    if !is_path_arg(arg) || Path::new(arg).is_absolute() {
        return None;
    }
    dir.join(arg).to_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings for the design's command line.
    fn mac() -> SidecarSettings {
        SidecarSettings::new([
            "../models/sam3/.venv/bin/python",
            "../sidecars/sam3-mlx/server.py",
            "--backend",
            "mlx",
            "--model",
            "../models/sam31-mlx",
            "--side",
            "672",
        ])
    }

    #[test]
    fn defaults_match_the_host_and_detector_defaults() {
        let s = mac();
        s.validate().unwrap();
        assert_eq!(s.send_side, 1008);
        assert_eq!(s.score_threshold, 0.5);
        let host = s.host_config();
        assert!(host.validate().is_ok());
        assert_eq!(host.startup_timeout, Duration::from_secs(60));
        assert_eq!(host.command, s.command);
        assert_eq!(host.current_dir, None);
        let d = s.detector_config(Lexicon::builtin());
        assert!(d.validate().is_ok());
        assert_eq!(d.request_timeout, Duration::from_secs(5));
        assert!(d.thresholds.is_empty());
    }

    #[test]
    fn relative_paths_resolve_against_the_config_and_words_stay() {
        let mut s = mac();
        s.command.extend(
            [
                "python3",
                "/opt/abs/server.py",
                "--out=a/b",
                "tcp://host/x",
                "~/models/sam31-mlx",
                "./here",
            ]
            .map(String::from),
        );
        s.resolve_paths(Path::new("/studio/config"));
        let want = [
            "/studio/config/../models/sam3/.venv/bin/python",
            "/studio/config/../sidecars/sam3-mlx/server.py",
            "--backend",
            "mlx",
            "--model",
            "/studio/config/../models/sam31-mlx",
            "--side",
            "672",
            "python3",
            "/opt/abs/server.py",
            "--out=a/b",
            "tcp://host/x",
            "~/models/sam31-mlx",
            "/studio/config/./here",
        ];
        assert_eq!(s.command, want);
        let again = s.clone();
        s.resolve_paths(Path::new("/elsewhere"));
        assert_eq!(s, again, "resolving twice changes nothing");
        let paths: Vec<usize> = s.path_args().map(|(i, _)| i).collect();
        assert_eq!(paths, [0, 1, 5, 9, 13]);
    }

    /// A named way to break valid settings.
    type Breakage = (&'static str, fn(&mut SidecarSettings));

    #[test]
    fn bad_values_name_their_key() {
        let cases: [Breakage; 9] = [
            ("detector.command", |s| s.command.clear()),
            ("command[0]", |s| s.command[0] = "  ".into()),
            ("command[3]", |s| s.command[3] = String::new()),
            ("send_side", |s| s.send_side = 0),
            ("send_side", |s| s.send_side = 4097),
            ("startup_timeout_s", |s| s.startup_timeout_s = 0.0),
            ("request_timeout_s", |s| s.request_timeout_s = f64::NAN),
            ("request_timeout_s", |s| s.request_timeout_s = 601.0),
            ("score_threshold", |s| s.score_threshold = 1.01),
        ];
        for (key, breakage) in cases {
            let mut s = mac();
            breakage(&mut s);
            let msg = s.validate().unwrap_err();
            assert!(msg.contains(key), "{key}: {msg}");
        }
        let mut edge = mac();
        edge.score_threshold = 0.0;
        edge.send_side = 4096;
        edge.request_timeout_s = 600.0;
        edge.validate().unwrap();
    }
}
