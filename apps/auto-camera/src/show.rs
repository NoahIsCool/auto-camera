//! Saving the show (spec M13): the engine's export written to a show file,
//! which `--config` opens again.
//!
//! # Where
//!
//! A show is saved to its [`ShowTarget`]: with `--save-show PATH`, to PATH;
//! when the app opened a show file, back to that file; otherwise to
//! `<studio>.show.toml` (the studio file's name) in the `shows` folder of
//! the data dir ([`data_dir`]): `AC_DATA_DIR` when set, else
//! `~/Library/Application Support/auto-camera` on macOS and
//! `$XDG_DATA_HOME/auto-camera` (when that is an absolute path) or
//! `~/.local/share/auto-camera` elsewhere. The target also names the studio
//! file the show lies over: the one the app's config was read from
//! ([`ac_engine::LoadedConfig::studio`]), so a show saved from a show lies
//! over the same studio file, never over the other show. Both paths are
//! absolute, so the show opens from any working folder, and a target that
//! is the studio file itself is refused: saving never writes over a
//! hand-written studio file.
//!
//! # How
//!
//! Saving is the operator's act, the window's Save show or `--save-show`
//! at the end of a headless run; nothing saves on a change. The engine
//! thread only exports (`ExportShow` reads the core, [`export`]); the
//! writing happens on a thread of its own, `ac-save-show` ([`SaveJob`]), so
//! neither the engine nor the window waits for the disk. The job ([`save`]):
//!
//! 1. names the studio file in the show and opens the show as `--config`
//!    would (the studio file read, the show laid over it, validated): a
//!    show that would not open is not written, and why is the save's error;
//! 2. refuses a target that exists and is not a show, so a mistyped path
//!    never replaces another file;
//! 3. writes the text atomically ([`disk::write_atomic`]): to a new temp
//!    file beside the target, readable and writable by its owner only (the
//!    destinations may carry a stream key), synced to the disk, then
//!    renamed over the target. The target is therefore always the previous
//!    show, byte for byte, or the new one, whatever fails when.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;

use ac_engine::{CommandReply, EngineCommand, EngineHandle, LoadedConfig, Show};

pub mod disk;

use disk::{Disk, SystemDisk, write_atomic};

/// The variable that overrides the data dir.
pub const DATA_DIR_ENV: &str = "AC_DATA_DIR";

/// The folder of the data dir that shows are saved in.
const SHOWS: &str = "shows";

/// The operating system's convention for the data dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// `~/Library/Application Support`.
    Mac,
    /// The XDG base directories: `$XDG_DATA_HOME`, else `~/.local/share`.
    Other,
}

impl Os {
    /// The convention of the system this build runs on.
    pub fn this() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Other
        }
    }
}

/// The app's data dir on `os`, reading environment variables with `var`;
/// see the module docs. `None` when there is no home folder to put it in.
/// Relative values are ignored, as the XDG convention asks, since they
/// would mean a different folder from each working folder.
pub fn data_dir(os: Os, var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(dir) = absolute(DATA_DIR_ENV) {
        return Some(dir);
    }
    let home = absolute("HOME");
    match os {
        Os::Mac => home.map(|h| h.join("Library/Application Support/auto-camera")),
        Os::Other => absolute("XDG_DATA_HOME")
            .or_else(|| home.map(|h| h.join(".local/share")))
            .map(|d| d.join("auto-camera")),
    }
}

/// The folder shows are saved in on this machine, if it has a data dir.
pub fn shows_dir() -> Option<PathBuf> {
    data_dir(Os::this(), |name| std::env::var_os(name)).map(|d| d.join(SHOWS))
}

/// Where a show is saved, and the studio file it lies over; see the module
/// docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowTarget {
    /// The show file, absolute.
    pub path: PathBuf,
    /// The studio file the show lies over, absolute.
    pub studio: PathBuf,
}

impl ShowTarget {
    /// The target of a run that read `loaded` from `opened`, given
    /// `--save-show` as `save_show`, with shows saved in `shows_dir` by
    /// default; see the module docs. The error says why there is none.
    pub fn for_run(
        loaded: &LoadedConfig,
        opened: &Path,
        save_show: Option<&Path>,
        shows_dir: Option<PathBuf>,
    ) -> Result<Self, String> {
        let studio = absolute(&loaded.studio)?;
        let path = match save_show {
            Some(path) => absolute(path)?,
            None if loaded.show => absolute(opened)?,
            None => {
                let dir = shows_dir.ok_or_else(|| {
                    format!(
                        "no folder for shows: there is no home folder; set {DATA_DIR_ENV} or \
                         pass --save-show PATH"
                    )
                })?;
                let stem = studio
                    .file_stem()
                    .map_or("studio".into(), |s| s.to_os_string());
                let mut name = stem;
                name.push(".show.toml");
                absolute(&dir.join(name))?
            }
        };
        if same_file(&path, &studio) {
            return Err(format!(
                "{} is the studio file; a show is saved beside it, never over it",
                path.display()
            ));
        }
        Ok(Self { path, studio })
    }
}

/// `path` made absolute against the working folder.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// True when `a` and `b` name the same file: the same file once links are
/// followed, or the same absolute path when either is not there.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The show as the engine behind `handle` has it, asked with
/// `ExportShow`.
pub fn export(handle: &EngineHandle) -> Result<Show, String> {
    match handle.send(EngineCommand::ExportShow) {
        Ok(CommandReply::Show(show)) => Ok(*show),
        Ok(other) => Err(format!("the engine answered {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Saves `show` to `target` through `disk`; see the module docs. Returns
/// the path written.
pub fn save(mut show: Show, target: &ShowTarget, disk: &mut dyn Disk) -> Result<PathBuf, String> {
    show.studio = Some(target.studio.clone());
    let unopenable = |e: &dyn std::fmt::Display| format!("the show would not open: {e}");
    let opened = show.open(&target.path).map_err(|e| unopenable(&e))?;
    opened.config.validate().map_err(|e| unopenable(&e))?;
    let json = target.path.extension().is_some_and(|e| e == "json");
    let text = show.to_text(json)?;
    let existing = disk
        .read(&target.path)
        .map_err(|e| format!("cannot read {}: {e}", target.path.display()))?;
    if existing.is_some_and(|old| !Show::is_show(&old, json)) {
        return Err(format!(
            "{} is not a show file; it is left as it is",
            target.path.display()
        ));
    }
    write_atomic(&target.path, text.as_bytes(), disk)?;
    Ok(target.path.clone())
}

/// One save running on its own thread; see the module docs. Dropping it
/// waits for the thread.
#[derive(Debug)]
pub struct SaveJob {
    /// The save's result, sent once.
    result: Receiver<Result<PathBuf, String>>,
    /// The thread, until it has been joined.
    thread: Option<JoinHandle<()>>,
}

impl SaveJob {
    /// Starts saving `show` to `target` on the system's disk.
    pub fn start(show: Show, target: ShowTarget) -> Result<Self, String> {
        Self::start_with(show, target, Box::new(SystemDisk))
    }

    /// [`Self::start`] through `disk` (tests fail one of its steps).
    pub fn start_with(
        show: Show,
        target: ShowTarget,
        mut disk: Box<dyn Disk + Send>,
    ) -> Result<Self, String> {
        let (tx, result) = channel();
        let thread = std::thread::Builder::new()
            .name("ac-save-show".into())
            .spawn(move || {
                let _ = tx.send(save(show, &target, disk.as_mut()));
            })
            .map_err(|e| format!("cannot start the save: {e}"))?;
        Ok(Self {
            result,
            thread: Some(thread),
        })
    }

    /// The result once the save has ended (its thread joined), without
    /// waiting.
    pub fn try_finish(&mut self) -> Option<Result<PathBuf, String>> {
        let result = match self.result.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("the save stopped without a result".into()),
        };
        self.join();
        Some(result)
    }

    /// Waits for the save to end.
    pub fn wait(mut self) -> Result<PathBuf, String> {
        let result = self
            .result
            .recv()
            .unwrap_or_else(|_| Err("the save stopped without a result".into()));
        self.join();
        result
    }

    /// Joins the thread, once.
    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SaveJob {
    fn drop(&mut self) {
        self.join();
    }
}

/// Exports the show from the engine behind `handle` and saves it to
/// `target`, waiting for the save's thread: the headless run's
/// `--save-show`.
pub fn save_now(handle: &EngineHandle, target: &ShowTarget) -> Result<PathBuf, String> {
    SaveJob::start(export(handle)?, target.clone())?.wait()
}

#[cfg(test)]
mod tests;
