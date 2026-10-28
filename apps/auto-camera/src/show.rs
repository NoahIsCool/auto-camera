//! Saving the show (spec M13): the engine's export written to a show file,
//! which `--config` opens again.
//!
//! # Where
//!
//! A show is saved to its [`ShowTarget`]: with `--save-show PATH`, to PATH;
//! when the window opened a show file, back to that file; otherwise to the
//! `shows` folder of the data dir ([`data_dir`]), named after the studio
//! file and a short hash of its full path ([`default_name`]), as in
//! `studio-1a2b3c4d.show.toml`, so two studio files named alike in
//! different folders never share a show. The data dir is `AC_DATA_DIR`
//! when that is set, which must be an absolute path (a relative one is an
//! error, so a show never goes somewhere other than asked), else
//! `~/Library/Application Support/auto-camera` on macOS and
//! `$XDG_DATA_HOME/auto-camera` (when that is an absolute path) or
//! `~/.local/share/auto-camera` elsewhere.
//!
//! The target also names the studio file the show lies over, the one the
//! app's config was read from ([`ac_engine::LoadedConfig::studio`]), so a
//! show saved from a show lies over the same studio file, never over the
//! other show, and it holds that file's config as the run read it
//! ([`ac_engine::LoadedConfig::base`]), which the save leaves out where
//! the show only repeats it. Both paths are absolute, so the show opens
//! from any working folder, and a target that is the studio file itself is
//! refused: saving never writes over a hand-written studio file.
//!
//! # How
//!
//! Saving is the operator's act, the window's Save show; nothing saves on
//! a change. The engine thread only exports (`ExportShow` reads the core,
//! and the window hands the reply to its saver, `ui::save`); the writing
//! happens on a thread of its own, `ac-save-show` ([`SaveJob`]), so neither
//! the engine nor the window waits for the disk. The job ([`save`]):
//!
//! 1. names the studio file in the show and leaves out the sources and
//!    the recording path where they only repeat that file's
//!    ([`Show::leave_out_repeats`]), so the studio file stays the base: an
//!    edit to it (a camera added) is there when the show opens, and the
//!    stream key, which no show holds, is always the studio file's;
//! 2. opens the show as `--config` would (the studio file read, the show
//!    laid over it, validated): a show that would not open is not written,
//!    and why is the save's error;
//! 3. refuses a target that exists and is not a show, or is the show of
//!    another studio file, so a mistyped path never replaces another file;
//! 4. writes the text atomically ([`disk::write_atomic`]): to a new temp
//!    file beside the target, readable and writable by its owner only,
//!    synced to the disk, then renamed over the target. The target is
//!    therefore always the previous show, byte for byte, or the new one,
//!    whatever fails when.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;

use ac_engine::{LoadedConfig, Show, StudioConfig};

pub mod disk;

use disk::{Disk, SystemDisk, write_atomic};

/// The variable that overrides the data dir.
pub const DATA_DIR_ENV: &str = "AC_DATA_DIR";

/// The folder of the data dir that shows are saved in.
const SHOWS: &str = "shows";

/// The FNV-1a offset basis, where [`default_name`]'s hash starts.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// The FNV-1a prime, which [`default_name`]'s hash multiplies by.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

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
/// see the module docs. The error says why there is none: no home folder,
/// or an `AC_DATA_DIR` that is not an absolute path. A variable set empty
/// counts as not set. A relative `HOME` or `XDG_DATA_HOME` is ignored, as
/// the XDG convention asks, since it would mean a different folder from
/// each working folder; a relative `AC_DATA_DIR`, which is set for this
/// app alone, is an error instead of a silent fallback.
pub fn data_dir(os: Os, var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, String> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = set(DATA_DIR_ENV) {
        return if dir.is_absolute() {
            Ok(dir)
        } else {
            Err(format!(
                "{DATA_DIR_ENV} is `{}`, which is not an absolute path",
                dir.display()
            ))
        };
    }
    let absolute = |name: &str| set(name).filter(|p| p.is_absolute());
    let home = absolute("HOME");
    let dir = match os {
        Os::Mac => home.map(|h| h.join("Library/Application Support/auto-camera")),
        Os::Other => absolute("XDG_DATA_HOME")
            .or_else(|| home.map(|h| h.join(".local/share")))
            .map(|d| d.join("auto-camera")),
    };
    dir.ok_or_else(|| "there is no home folder".into())
}

/// The folder shows are saved in on this machine, or why there is none.
pub fn shows_dir() -> Result<PathBuf, String> {
    data_dir(Os::this(), |name| std::env::var_os(name)).map(|d| d.join(SHOWS))
}

/// Where a show is saved, and the studio file it lies over; see the module
/// docs.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowTarget {
    /// The show file, absolute.
    pub path: PathBuf,
    /// The studio file the show lies over, absolute.
    pub studio: PathBuf,
    /// That studio file's config as the run read it, before any show was
    /// laid over it: what a saved show leaves out where it only repeats it.
    pub base: StudioConfig,
}

impl ShowTarget {
    /// The target of a run that read `loaded` from `opened`, given
    /// `--save-show` as `save_show`, with shows saved in `shows_dir` by
    /// default; see the module docs. The error says why there is none.
    pub fn for_run(
        loaded: &LoadedConfig,
        opened: &Path,
        save_show: Option<&Path>,
        shows_dir: Result<PathBuf, String>,
    ) -> Result<Self, String> {
        let studio = absolute(&loaded.studio)?;
        let path = match save_show {
            Some(path) => absolute(path)?,
            None if loaded.show => absolute(opened)?,
            None => {
                let dir = shows_dir.map_err(|why| {
                    format!(
                        "no folder for shows: {why}; set {DATA_DIR_ENV} to an absolute path, \
                         or pass --save-show PATH"
                    )
                })?;
                absolute(&dir.join(default_name(&studio)))?
            }
        };
        if same_file(&path, &studio) {
            return Err(format!(
                "{} is the studio file; a show is saved beside it, never over it",
                path.display()
            ));
        }
        Ok(Self {
            path,
            studio,
            base: loaded.base.clone(),
        })
    }
}

/// The default show file name for the absolute studio file path `studio`:
/// its name, then the low 32 bits of a hash of its full path (links
/// followed, when it is there), in hex, so `church/studio.toml` and
/// `school/studio.toml` get shows of their own. The hash is FNV-1a, fixed
/// here rather than the standard library's hasher, which may change from
/// one Rust release to the next: a later build saves to the same file.
fn default_name(studio: &Path) -> OsString {
    let full = std::fs::canonicalize(studio).unwrap_or_else(|_| studio.to_path_buf());
    let hash = full
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(FNV_OFFSET, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(FNV_PRIME)
        });
    let mut name = studio
        .file_stem()
        .map_or("studio".into(), OsStr::to_os_string);
    name.push(format!("-{:08x}.show.toml", hash & 0xffff_ffff));
    name
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

/// Saves `show` to `target` through `disk`; see the module docs. Returns
/// the path written.
pub fn save(mut show: Show, target: &ShowTarget, disk: &mut dyn Disk) -> Result<PathBuf, String> {
    show.studio = Some(target.studio.clone());
    show.leave_out_repeats(&target.base);
    let unopenable = |e: &dyn std::fmt::Display| format!("the show would not open: {e}");
    let opened = show.open(&target.path).map_err(|e| unopenable(&e))?;
    opened.config.validate().map_err(|e| unopenable(&e))?;
    let json = target.path.extension().is_some_and(|e| e == "json");
    let text = show.to_text(json)?;
    let existing = disk
        .read(&target.path)
        .map_err(|e| format!("cannot read {}: {e}", target.path.display()))?;
    if let Some(old) = existing {
        if !Show::is_show(&old, json) {
            return Err(format!(
                "{} is not a show file; it is left as it is",
                target.path.display()
            ));
        }
        if let Some(other) = studio_of(&old, json, &target.path)
            && !same_file(&other, &target.studio)
        {
            return Err(format!(
                "{} is the show of another studio file, {}; it is left as it is",
                target.path.display(),
                other.display()
            ));
        }
    }
    write_atomic(&target.path, text.as_bytes(), disk)?;
    Ok(target.path.clone())
}

/// The studio file that the show `text` (JSON when `json`), found at
/// `path`, lies over, a relative one taken from that file's folder; `None`
/// when the show does not parse or names none.
fn studio_of(text: &str, json: bool, path: &Path) -> Option<PathBuf> {
    let (mut show, _) = Show::parse_with_warnings(text, json).ok()?;
    if let Some(dir) = path.parent() {
        show.resolve_paths(dir);
    }
    show.studio
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

#[cfg(test)]
mod tests;
