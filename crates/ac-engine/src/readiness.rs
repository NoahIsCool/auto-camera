//! The readiness check behind `auto-camera --check`: everything that would
//! stop a studio from starting, or from doing its job once it runs, that
//! can be found without starting it.
//!
//! # Theory
//!
//! `Engine::start` stops at the first problem it meets, and some problems
//! show only later: a missing video file leaves its source offline, and a
//! recording folder that does not exist fails Start Output, perhaps minutes
//! into a show. The check asks about all of them before the show and lists
//! every answer ([`check`]):
//!
//! * **the config**: its schema (a missing key is a warning) and the
//!   problems in its values ([`StudioConfig::problems`]: every bad key of
//!   the engine's own tables and of each source, the first of each table
//!   another crate checks);
//! * **sources**: each file source's video, and each audio input's file,
//!   is a readable file;
//! * **ffmpeg**: `output.ffmpeg -version` runs, and with file sources (or
//!   audio inputs) so do
//!   the ffmpeg and ffprobe they decode with (`AC_FFMPEG`, `AC_FFPROBE`).
//!   Like the program itself, the check runs a tool as named: a bare name
//!   from `PATH`, a relative path from the working folder;
//! * **the detector's files**: OWL's model, text encoder and tokenizer are
//!   readable files, in a build with the `onnx` feature; a sidecar's
//!   program is found and executable (in a venv, a link that still points
//!   somewhere and a base Python whose folder, `pyvenv.cfg`'s `home`, is
//!   still there) and every path in its command exists and is readable;
//! * **the recording**: `output.record_path` does not exist yet (recordings
//!   are never overwritten) and its folder takes a new file. Like
//!   `output.ffmpeg` and unlike the config's other paths, a relative
//!   recording path is taken from the working folder, as it is when the
//!   output starts, so it is checked there;
//! * **the RTMP URL**: shown, its stream key redacted, once its form passes
//!   (a bad form is a config problem).
//!
//! The check opens no camera, source or model, starts no sidecar, contacts
//! no RTMP server and runs nothing but the ffmpeg tools' `-version`; it
//! only looks at files. Whether a model file is the right model (its hash)
//! is a later check (G6).
//!
//! # Shape
//!
//! Every question to the machine goes through [`Inspect`], so [`check`] is
//! a pure function of the config and the answers, tested with a fake
//! inspector; [`SystemInspector`] answers from the file system and by
//! running the tools. [`check_file`] reads the config file first, as the
//! app does.
//!
//! # Invariants
//!
//! * Every finding names the key or the file it is about, so the operator
//!   knows what to fix, and a problem is reported once: a value the config
//!   check refuses (an empty `output.ffmpeg` or sidecar program, an RTMP
//!   URL of the wrong form) gets no finding of its own after that.
//! * A file that cannot be read or parsed yields that one problem and
//!   nothing else, since nothing in it can be trusted; a config that
//!   parses but has bad values is still inspected, so one run shows the
//!   file problems too.
//! * [`Readiness::is_ready`] exactly when there is no problem; warnings do
//!   not count.

use std::path::{Path, PathBuf};

use ac_sources::{FfmpegTools, SourceConfig};

use crate::config::{DetectorSettings, SidecarSettings, StudioConfig};

mod system;

pub use system::SystemInspector;

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Checked and fine.
    Ok,
    /// Worth fixing, but the studio works as it is.
    Warning,
    /// Stops the studio, or part of it, from working.
    Problem,
}

/// One thing the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// How much it matters.
    pub level: Level,
    /// A sentence naming the key or file it is about.
    pub message: String,
}

/// Everything the check found, in the order it looked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Readiness {
    /// The findings.
    pub findings: Vec<Finding>,
}

impl Readiness {
    /// True when nothing would stop the studio (warnings aside).
    pub fn is_ready(&self) -> bool {
        self.problems().next().is_none()
    }

    /// The problems' messages.
    pub fn problems(&self) -> impl Iterator<Item = &str> {
        self.at(Level::Problem)
    }

    /// The warnings' messages.
    pub fn warnings(&self) -> impl Iterator<Item = &str> {
        self.at(Level::Warning)
    }

    /// The messages of the findings at `level`.
    fn at(&self, level: Level) -> impl Iterator<Item = &str> {
        self.findings
            .iter()
            .filter(move |f| f.level == level)
            .map(|f| f.message.as_str())
    }

    /// Adds a finding.
    fn push(&mut self, level: Level, message: String) {
        self.findings.push(Finding { level, message });
    }
}

/// What is at a path, links followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// Nothing.
    Missing,
    /// A symbolic link whose target (given) does not exist.
    BrokenLink(PathBuf),
    /// A regular file.
    File {
        /// It can be opened for reading.
        readable: bool,
        /// Someone may execute it (always true where there are no
        /// permission bits).
        executable: bool,
    },
    /// A folder.
    Dir {
        /// Its entries can be listed.
        readable: bool,
    },
    /// Something else: a socket, a pipe, a device.
    Other,
    /// It could not be looked at (a parent folder that may not be
    /// searched, an I/O error); the reason.
    Unknown(String),
}

/// The questions the check asks of the machine; see the module docs.
pub trait Inspect {
    /// What is at `path`, links followed.
    fn entry(&self, path: &Path) -> Entry;
    /// `Ok` when a new file can be created in the folder `dir`, else why
    /// not.
    fn can_create_in(&self, dir: &Path) -> Result<(), String>;
    /// The first line `tool -version` prints, or why it did not run.
    fn version(&self, tool: &Path) -> Result<String, String>;
    /// Where the bare program name `name` is found on `PATH`.
    fn find_program(&self, name: &str) -> Option<PathBuf>;
    /// The text of a small file (a venv's `pyvenv.cfg`).
    fn read_text(&self, path: &Path) -> Result<String, String>;
    /// The tools file sources decode with.
    fn decoder_tools(&self) -> FfmpegTools;
}

/// Reads the config file at `path` and checks it with `inspector`; see the
/// module docs.
pub fn check_file(path: &Path, inspector: &dyn Inspect) -> Readiness {
    match StudioConfig::read_file(path) {
        Ok(loaded) => {
            let mut readiness = Readiness::default();
            for warning in loaded.warnings {
                readiness.push(Level::Warning, warning);
            }
            readiness
                .findings
                .extend(check(&loaded.config, inspector).findings);
            readiness
        }
        Err(e) => Readiness {
            findings: vec![Finding {
                level: Level::Problem,
                message: e.to_string(),
            }],
        },
    }
}

/// Checks `config` (its paths already resolved) with `inspector`, for this
/// build; see the module docs.
pub fn check(config: &StudioConfig, inspector: &dyn Inspect) -> Readiness {
    check_build(config, inspector, cfg!(feature = "onnx"))
}

/// [`check`] for a build with (`onnx`) or without the ONNX detector.
fn check_build(config: &StudioConfig, inspector: &dyn Inspect, onnx: bool) -> Readiness {
    let mut c = Checker {
        inspector,
        out: Readiness::default(),
    };
    c.config(config);
    let mut file_sources = false;
    for s in &config.sources {
        if let SourceConfig::File { path, .. } = &s.input {
            file_sources = true;
            c.file(&format!("source {:?}", s.id), path);
        }
    }
    for a in &config.audio_inputs {
        if let Some(path) = a.input.path() {
            file_sources = true;
            c.file(&format!("audio input {:?}", a.id), path);
        }
    }
    c.tools(config, file_sources);
    match &config.detector {
        DetectorSettings::Owl {
            model,
            text_encoder,
            tokenizer,
            ..
        } => {
            if !onnx {
                c.problem(
                    "detector: kind \"owl\" needs auto-camera built with `--features onnx`".into(),
                );
            }
            c.file("detector.model", model);
            if let Some(t) = text_encoder {
                c.file("detector.text_encoder", t);
            }
            c.file("detector.tokenizer", tokenizer);
        }
        DetectorSettings::Sidecar(sidecar) => c.sidecar(sidecar),
        DetectorSettings::None | DetectorSettings::Synthetic { .. } => {}
    }
    c.output(config);
    c.out
}

/// The check in progress: the inspector and what it found so far.
struct Checker<'a> {
    /// Answers the questions.
    inspector: &'a dyn Inspect,
    /// The findings so far.
    out: Readiness,
}

impl Checker<'_> {
    /// Notes a fine finding.
    fn ok(&mut self, message: String) {
        self.out.push(Level::Ok, message);
    }

    /// Notes a problem.
    fn problem(&mut self, message: String) {
        self.out.push(Level::Problem, message);
    }

    /// The config's own problems, or a line saying what it is.
    fn config(&mut self, config: &StudioConfig) {
        let problems = config.problems();
        if problems.is_empty() {
            let detector = match config.detector {
                DetectorSettings::None => "none",
                DetectorSettings::Synthetic { .. } => "synthetic",
                DetectorSettings::Owl { .. } => "owl",
                DetectorSettings::Sidecar(_) => "sidecar",
            };
            self.ok(format!(
                "config: schema {}, {}, {}, detector {detector:?}",
                config.schema,
                count(config.sources.len(), "source", "sources"),
                count(config.queries.len(), "query", "queries"),
            ));
        }
        for p in problems {
            self.problem(format!("config: {p}"));
        }
    }

    /// `path` must be a readable file; `what` names it.
    fn file(&mut self, what: &str, path: &Path) {
        let shown = path.display();
        match self.inspector.entry(path) {
            Entry::File { readable: true, .. } => self.ok(format!("{what}: {shown} is readable")),
            Entry::File {
                readable: false, ..
            } => self.problem(format!(
                "{what}: {shown} is not readable (check its permissions)"
            )),
            Entry::Dir { .. } => self.problem(format!("{what}: {shown} is a folder, not a file")),
            other => self.absent(what, path, other),
        }
    }

    /// `path` must be a readable file or folder; `what` names it.
    fn file_or_folder(&mut self, what: &str, path: &Path) {
        match self.inspector.entry(path) {
            Entry::Dir { readable: true } => {
                self.ok(format!("{what}: folder {} is readable", path.display()));
            }
            Entry::Dir { readable: false } => self.problem(format!(
                "{what}: folder {} is not readable (check its permissions)",
                path.display()
            )),
            _ => self.file(what, path),
        }
    }

    /// The problem for an `entry` at `path` that is neither a file nor a
    /// folder.
    fn absent(&mut self, what: &str, path: &Path, entry: Entry) {
        let shown = path.display();
        let message = match entry {
            Entry::Missing => format!("{what}: {shown} does not exist"),
            Entry::BrokenLink(target) => format!(
                "{what}: {shown} is a link to {}, which does not exist",
                target.display()
            ),
            Entry::Unknown(why) => format!("{what}: cannot look at {shown}: {why}"),
            Entry::Other | Entry::File { .. } | Entry::Dir { .. } => {
                format!("{what}: {shown} is not a regular file")
            }
        };
        self.problem(message);
    }

    /// The ffmpeg tools: the output's, and the decoders when there are
    /// file sources; a tool named twice is run once.
    fn tools(&mut self, config: &StudioConfig, file_sources: bool) {
        // (what it is, where, how to name another one)
        let mut tools: Vec<(String, PathBuf, &str)> = Vec::new();
        if file_sources {
            let decoders = self.inspector.decoder_tools();
            tools.push((
                "file sources' ffmpeg (AC_FFMPEG)".into(),
                decoders.ffmpeg,
                "AC_FFMPEG",
            ));
            tools.push((
                "file sources' ffprobe (AC_FFPROBE)".into(),
                decoders.ffprobe,
                "AC_FFPROBE",
            ));
        }
        let output = &config.output.ffmpeg;
        match tools.iter_mut().find(|(_, path, _)| path == output) {
            Some((what, _, hint)) => {
                what.insert_str(0, "output.ffmpeg and ");
                *hint = "output.ffmpeg or AC_FFMPEG";
            }
            // An empty path is a config problem already.
            None if output.as_os_str().is_empty() => {}
            None => tools.push((
                "output.ffmpeg".into(),
                output.clone(),
                "output.ffmpeg or AC_FFMPEG",
            )),
        }
        for (what, tool, hint) in tools {
            let shown = tool_shown(&tool);
            match self.inspector.version(&tool) {
                Ok(version) => self.ok(format!("{what}: {shown} runs ({version})")),
                Err(why) => self.problem(format!(
                    "{what}: {shown} does not run: {why}; install ffmpeg, or name it with {hint}"
                )),
            }
        }
    }

    /// The sidecar's program, its venv if it has one, and the paths in its
    /// command.
    fn sidecar(&mut self, sidecar: &SidecarSettings) {
        let program = sidecar.command.first().map_or("", String::as_str);
        let what = "detector.command[0]";
        if program.trim().is_empty() {
            // A config problem already says so.
        } else if !program.contains('/') {
            match self.inspector.find_program(program) {
                Some(found) => self.ok(format!("{what}: {program} is {} on PATH", found.display())),
                None => self.problem(format!(
                    "{what}: {program:?} is not on PATH; give its path (relative to the \
                     config file, or absolute)"
                )),
            }
        } else {
            self.program(what, Path::new(program));
        }
        for (i, arg) in sidecar.path_args().filter(|(i, _)| *i > 0) {
            self.file_or_folder(&format!("detector.command[{i}]"), Path::new(arg));
        }
    }

    /// The sidecar's program at `path`: an executable file, and a working
    /// venv when it is in one.
    fn program(&mut self, what: &str, path: &Path) {
        let shown = path.display();
        match self.inspector.entry(path) {
            Entry::File {
                executable: true, ..
            } => {
                self.ok(format!("{what}: {shown} is executable"));
                self.venv(path);
            }
            Entry::File {
                executable: false, ..
            } => self.problem(format!("{what}: {shown} is not executable")),
            Entry::Dir { .. } => {
                self.problem(format!("{what}: {shown} is a folder, not a program"))
            }
            Entry::BrokenLink(target) => self.problem(format!(
                "{what}: {shown} is a link to {}, which does not exist; a venv whose base \
                 Python was moved or removed must be made again",
                target.display()
            )),
            Entry::Missing if venv_root(path).is_some() => self.problem(format!(
                "{what}: {shown} does not exist; make the sidecar's venv as its README says"
            )),
            other => self.absent(what, path, other),
        }
    }

    /// When `program` lies in a venv (`<root>/bin/<program>` beside
    /// `<root>/pyvenv.cfg`), the venv's base Python folder must still be
    /// there.
    fn venv(&mut self, program: &Path) {
        let Some(root) = venv_root(program) else {
            return;
        };
        let cfg = root.join("pyvenv.cfg");
        if !matches!(self.inspector.entry(&cfg), Entry::File { .. }) {
            return;
        }
        let text = match self.inspector.read_text(&cfg) {
            Ok(text) => text,
            Err(why) => {
                self.problem(format!(
                    "detector venv: cannot read {}: {why}",
                    cfg.display()
                ));
                return;
            }
        };
        let version = venv_key(&text, "version")
            .or_else(|| venv_key(&text, "version_info"))
            .unwrap_or("of unknown version");
        match venv_key(&text, "home") {
            Some(home) if matches!(self.inspector.entry(Path::new(home)), Entry::Dir { .. }) => {
                self.ok(format!(
                    "detector venv: {} (Python {version}, from {home})",
                    root.display()
                ));
            }
            Some(home) => self.problem(format!(
                "detector venv: {}: its base Python folder {home} (`home` in pyvenv.cfg) \
                 is gone; make the venv again",
                root.display()
            )),
            None => self.ok(format!(
                "detector venv: {} (Python {version})",
                root.display()
            )),
        }
    }

    /// The recording path and the RTMP URL.
    fn output(&mut self, config: &StudioConfig) {
        if let Some(url) = config
            .output
            .rtmp_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .filter(|u| ac_output::config::validate_rtmp_url(u).is_ok())
        {
            self.ok(format!(
                "output.rtmp_url: {} (its form is checked; the server is not contacted)",
                ac_output::config::redact_url(url)
            ));
        }
        let Some(path) = config
            .output
            .record_path
            .as_deref()
            .filter(|p| !p.as_os_str().is_empty())
        else {
            return;
        };
        let what = "output.record_path";
        match self.inspector.entry(path) {
            Entry::Missing => {}
            Entry::Unknown(why) => {
                self.problem(format!("{what}: cannot look at {}: {why}", path.display()));
            }
            _ => self.problem(format!(
                "{what}: {} already exists; recordings are never overwritten, so starting \
                 the output would fail: move the file away or change the path",
                path.display()
            )),
        }
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let shown = if dir.is_relative() {
            format!("{} (from the working folder)", dir.display())
        } else {
            dir.display().to_string()
        };
        match self.inspector.entry(dir) {
            Entry::Dir { .. } => match self.inspector.can_create_in(dir) {
                Ok(()) => self.ok(format!("{what}: folder {shown} takes new files")),
                Err(why) => self.problem(format!("{what}: cannot create a file in {shown}: {why}")),
            },
            Entry::Missing => self.problem(format!(
                "{what}: folder {shown} does not exist (starting the output does not create it)"
            )),
            Entry::Unknown(why) => self.problem(format!("{what}: cannot look at {shown}: {why}")),
            _ => self.problem(format!("{what}: {shown} is not a folder")),
        }
    }
}

/// How a tool at `path` is shown: a relative path with a folder in it
/// marked as taken from the working folder; an absolute path, or a bare
/// name (looked up on `PATH`), as it is.
fn tool_shown(path: &Path) -> String {
    if path.is_relative() && path.components().count() > 1 {
        format!("{} (from the working folder)", path.display())
    } else {
        path.display().to_string()
    }
}

/// `n` and the noun for it, singular or plural.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The venv a program at `<root>/bin/<name>` would be in: `<root>`.
fn venv_root(program: &Path) -> Option<&Path> {
    let bin = program.parent()?;
    (bin.file_name()? == "bin").then(|| bin.parent()).flatten()
}

/// The value of `key` in a `pyvenv.cfg` (`key = value` lines).
fn venv_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key)
            .then(|| v.trim())
            .filter(|v| !v.is_empty())
    })
}

#[cfg(test)]
mod tests;
