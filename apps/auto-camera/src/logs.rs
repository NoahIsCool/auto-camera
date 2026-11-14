//! The app's log: a size-capped, rotating file of `tracing` events, crash
//! reports next to it, and lines that say what ran.
//!
//! # Theory
//!
//! When a show goes wrong the operator needs to know what happened after
//! the fact, on a machine that may have been restarted since. So the app
//! keeps a log on disk, in a per-user folder ([`default_dir`]:
//! `~/Library/Logs/auto-camera` on macOS, `$XDG_STATE_HOME/auto-camera` or
//! `~/.local/state/auto-camera` on Linux), or in the folder `--log-dir`
//! names (tests and CI always pass a temporary one). Nothing is ever sent
//! anywhere: the log and the crash reports stay in that folder.
//!
//! Who logs what:
//!
//! * The adapter crates emit `tracing` events: `ac-output` every line
//!   ffmpeg prints while encoding (stream key redacted), `ac-sources` the
//!   source ffmpegs' stderr, `ac-sidecar` the sidecar's stderr and its
//!   starts, losses and restarts.
//! * The core crates (and the engine) log nothing: their state reaches
//!   the log as data, through the snapshot watcher ([`watch`]), which
//!   logs each change worth a line and keeps the crash summary current.
//! * The app logs its own warnings and errors beside printing them on
//!   stderr, since a window started from the Finder has no terminal: the
//!   config's warnings, errors that end a run, a system not kept awake
//!   while live (`awake`), a termination signal, an engine that did not
//!   shut down cleanly and the shows the window saved, or failed to save,
//!   as it closed; and how a headless run ended.
//! * This module installs the one subscriber (`writer`: one line per
//!   event, queued without blocking, written by one thread into the
//!   rotating file of `file`), the panic hook (`crash`), and writes the
//!   header: the app's version and commit, the process and the log's
//!   path; once the config is loaded ([`LogSession::studio`]) the config's
//!   path and SHA-256 (for a show, also those of the studio file it lies
//!   over, whose edits change what the show opens), and the model files,
//!   hashed in the background (`hashes`). Header lines are marked
//!   (`log_header = true`), so every file a rotation starts begins with
//!   them again.
//!
//! Two apps may run at once on one machine (the window beside a headless
//! rehearsal), and the log's files have one writer each (see `file`). So
//! a session writes the first log no other process holds: `auto-camera.log`,
//! else `auto-camera-2.log`, and so on up to [`SLOTS`]. Within one file the
//! sessions follow each other, each beginning with its header line.
//!
//! # Invariants
//!
//! * Logging never blocks or fails the caller; a log that cannot be
//!   opened is reported on stderr and the app runs without one.
//! * The folder holds, for each of at most [`SLOTS`] logs, at most [`KEEP`]
//!   old files and the current one, each at most [`CAP`] bytes, and at
//!   most `crash::KEEP_REPORTS` crash reports from earlier runs (pruned at
//!   start) plus `crash::MAX_REPORTS` from this one.
//! * Events at [`LEVEL`] and more severe are logged.
//! * Dropping the [`LogSession`] stops the hasher, writes the last line
//!   and flushes the file before the process exits.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ac_engine::LoadedConfig;
use tracing::Level;

mod crash;
mod file;
#[cfg(test)]
mod fixtures;
mod hashes;
mod summary;
#[cfg(test)]
mod testdir;
mod time;
mod watch;
mod writer;

pub use watch::{Watch, watch};

use crash::CrashReporter;
use file::RotatingFile;
use hashes::Hasher;
use writer::Writer;

/// Largest size of one log file, in bytes.
pub const CAP: u64 = 8 * 1024 * 1024;

/// Rotated log files kept besides the current one.
pub const KEEP: usize = 4;

/// Least severe level logged.
pub const LEVEL: Level = Level::INFO;

/// Logs that can be written at once, by as many apps.
pub const SLOTS: usize = 4;

/// Name stem of the log files: `auto-camera.log`, `auto-camera.1.log`, ...
const STEM: &str = "auto-camera";

/// The target of the session's own events.
const TARGET: &str = "auto_camera::logs";

/// The app's name, version and commit, as the header and crash reports
/// give them.
fn app() -> String {
    format!(
        "auto-camera {} (git {})",
        env!("CARGO_PKG_VERSION"),
        env!("AC_GIT_HASH")
    )
}

/// The per-user log folder on `os` (`std::env::consts::OS`), given the
/// home folder and `XDG_STATE_HOME`; `None` without a home. Relative
/// values are ignored, as the XDG rules say.
pub fn default_dir(os: &str, home: Option<&Path>, xdg_state: Option<&Path>) -> Option<PathBuf> {
    let home = home.filter(|h| h.is_absolute());
    match os {
        "macos" => Some(home?.join("Library/Logs/auto-camera")),
        "linux" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" => {
            match xdg_state.filter(|x| x.is_absolute()) {
                Some(state) => Some(state.join("auto-camera")),
                None => Some(home?.join(".local/state/auto-camera")),
            }
        }
        _ => Some(home?.join(".auto-camera/logs")),
    }
}

/// [`default_dir`] for this machine and user.
fn user_dir() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    default_dir(
        std::env::consts::OS,
        var("HOME").as_deref(),
        var("XDG_STATE_HOME").as_deref(),
    )
}

/// Opens the first log in `dir` that no other process writes:
/// `<STEM>.log`, else `<STEM>-2.log`, and so on up to [`SLOTS`].
fn open_free(dir: &Path, cap: u64, keep: usize) -> io::Result<RotatingFile> {
    for slot in 1..=SLOTS {
        let stem = match slot {
            1 => STEM.to_owned(),
            n => format!("{STEM}-{n}"),
        };
        match RotatingFile::open(dir, &stem, cap, keep) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            opened => return opened,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!("{SLOTS} other auto-camera processes write logs there"),
    ))
}

/// The log of one run of the app; see the module docs.
#[derive(Debug)]
pub struct LogSession {
    /// The writer thread, when the log is open.
    writer: Option<Writer>,
    /// The model file hasher, once started.
    hasher: Option<Hasher>,
}

impl LogSession {
    /// Opens the log in `dir`, or in the per-user folder when `None`,
    /// installs the subscriber and the panic hook and writes the header.
    /// When the log cannot be opened, says so on stderr and returns a
    /// session that logs nothing.
    pub fn start(dir: Option<&Path>) -> Self {
        let Some(dir) = dir.map(Path::to_owned).or_else(user_dir) else {
            eprintln!("auto-camera: no folder for the log (HOME is not set); running without one");
            return Self::inactive();
        };
        match Self::open(&dir) {
            Ok(session) => session,
            Err(e) => {
                eprintln!(
                    "auto-camera: cannot write the log in {}: {e}; running without one",
                    dir.display()
                );
                Self::inactive()
            }
        }
    }

    /// A session without a log.
    fn inactive() -> Self {
        Self {
            writer: None,
            hasher: None,
        }
    }

    /// [`Self::start`] in `dir`, failing on the first error.
    fn open(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let pruned = crash::prune(dir, crash::KEEP_REPORTS);
        let file = open_free(dir, CAP, KEEP)?;
        let path = file.path();
        let writer = Writer::start(file)?;
        tracing::subscriber::set_global_default(writer.subscriber(LEVEL))
            .map_err(|_| io::Error::other("another log is already installed"))?;
        let reporter = Arc::new(CrashReporter::new(dir, &app()));
        crash::register(reporter.clone());
        // The hook stays for the life of the process.
        drop(crash::install(reporter));
        tracing::info!(
            target: TARGET,
            log_header = true,
            "{} starting: pid {}, {}/{}, log {}",
            app(),
            std::process::id(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            path.display()
        );
        if pruned > 0 {
            tracing::info!(target: TARGET, "removed {pruned} old crash reports");
        }
        Ok(Self {
            writer: Some(writer),
            hasher: None,
        })
    }

    /// Logs the config file at `path` (its SHA-256), which `loaded` was
    /// read from, and for a show the studio file it lies over, then starts
    /// hashing the model files the config names, in the background.
    pub fn studio(&mut self, path: &Path, loaded: &LoadedConfig) {
        if self.writer.is_none() {
            return;
        }
        let config = &loaded.config;
        match std::fs::read(path) {
            Ok(bytes) => tracing::info!(
                target: TARGET,
                log_header = true,
                "config {}: sha256 {}, {} sources, {} queries",
                path.display(),
                hashes::sha256_hex(&bytes),
                config.sources.len(),
                config.queries.len()
            ),
            Err(e) => tracing::warn!(
                target: TARGET,
                log_header = true,
                "config {}: cannot hash: {e}",
                path.display()
            ),
        }
        if loaded.show {
            studio_under_show(&loaded.studio);
        }
        let paths = hashes::model_paths(&config.detector);
        if !paths.is_empty() {
            self.hasher = Hasher::start(paths, hashes::log);
        }
    }
}

/// Logs the studio file a show lies over, with its SHA-256, as a header
/// line: the show's own hash says nothing of an edit to that file (a
/// camera added, the stream key rotated), which changes what the show
/// opens.
fn studio_under_show(studio: &Path) {
    match std::fs::read(studio) {
        Ok(bytes) => tracing::info!(
            target: TARGET,
            log_header = true,
            "the show lies over studio {}: sha256 {}",
            studio.display(),
            hashes::sha256_hex(&bytes)
        ),
        Err(e) => tracing::warn!(
            target: TARGET,
            log_header = true,
            "the show lies over studio {}: cannot hash: {e}",
            studio.display()
        ),
    }
}

impl Drop for LogSession {
    fn drop(&mut self) {
        if let Some(hasher) = &mut self.hasher {
            hasher.stop();
        }
        if let Some(mut writer) = self.writer.take() {
            tracing::info!(target: TARGET, "auto-camera exiting");
            writer.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use ac_engine::StudioConfig;

    use super::*;

    #[test]
    fn the_log_folder_follows_each_systems_convention() {
        let home = Some(Path::new("/Users/noah"));
        assert_eq!(
            default_dir("macos", home, None),
            Some("/Users/noah/Library/Logs/auto-camera".into())
        );
        assert_eq!(
            default_dir("linux", Some(Path::new("/home/noah")), None),
            Some("/home/noah/.local/state/auto-camera".into())
        );
        assert_eq!(
            default_dir("linux", None, Some(Path::new("/run/state"))),
            Some("/run/state/auto-camera".into()),
            "XDG_STATE_HOME wins and needs no home"
        );
        assert_eq!(
            default_dir("linux", home, Some(Path::new("relative/state"))),
            Some("/Users/noah/.local/state/auto-camera".into()),
            "a relative XDG_STATE_HOME is ignored"
        );
        assert_eq!(
            default_dir("windows", home, None),
            Some("/Users/noah/.auto-camera/logs".into())
        );
    }

    #[test]
    fn without_an_absolute_home_there_is_no_default() {
        assert_eq!(default_dir("macos", None, None), None);
        assert_eq!(default_dir("macos", Some(Path::new("noah")), None), None);
        assert_eq!(default_dir("linux", Some(Path::new("")), None), None);
    }

    #[test]
    fn the_header_names_the_version_and_commit() {
        let app = app();
        assert!(app.starts_with(&format!("auto-camera {} (git ", env!("CARGO_PKG_VERSION"))));
        assert!(app.ends_with(')') && !app.contains("(git )"), "{app}");
    }

    #[test]
    fn a_second_app_writes_the_next_free_log() {
        let dir = testdir::TempDir::new("slots");
        let log = |name: &str| dir.path().join(name);
        let first = open_free(dir.path(), 100, 1).unwrap();
        assert_eq!(first.path(), log("auto-camera.log"));
        let others: Vec<RotatingFile> = (2..=SLOTS)
            .map(|n| {
                let file = open_free(dir.path(), 100, 1).unwrap();
                assert_eq!(file.path(), log(&format!("auto-camera-{n}.log")));
                file
            })
            .collect();
        let err = open_free(dir.path(), 100, 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert!(
            err.to_string().starts_with(&format!("{SLOTS} other")),
            "{err}"
        );
        drop(first);
        let again = open_free(dir.path(), 100, 1).unwrap();
        assert_eq!(
            again.path(),
            log("auto-camera.log"),
            "a freed log is taken again"
        );
        drop(others);
    }

    #[test]
    fn an_inactive_session_ignores_the_studio() {
        let config = StudioConfig::with_source(
            "stage",
            ac_sources::SourceConfig::Synthetic {
                width: 320,
                height: 180,
                fps: 30.0,
                subjects: 1,
            },
        );
        let loaded = LoadedConfig {
            base: config.clone(),
            config,
            warnings: Vec::new(),
            studio: PathBuf::from("/nonexistent/studio.toml"),
            show: true,
        };
        let mut session = LogSession::inactive();
        session.studio(Path::new("/nonexistent/sunday.show.toml"), &loaded);
        assert!(session.hasher.is_none());
    }
}
