//! Replacing a file atomically: the file-system steps of a save, behind
//! [`Disk`] so a test can make one fail.
//!
//! # Theory
//!
//! Writing a file in place can leave it half written: a crash, a full disk
//! or a lost mount between the first and the last byte, and the previous
//! show is gone with no new one in its place. [`write_atomic`] never opens
//! the target. It writes the new text to a temp file beside it, in the
//! same folder (so on the same file system, where a rename is atomic),
//! syncs the temp file to the disk, renames it over the target, then
//! syncs the folder so the rename itself is on the disk. Until the rename
//! the target is the previous file, byte for byte; after it, the new one.
//! A save that fails before the rename removes its temp file.
//!
//! The temp file is created new (never one that exists) and, on Unix,
//! readable and writable by its owner only (`0600`); the rename keeps that
//! mode. A show holds no stream key (that stays in the studio file), but it
//! is the operator's own: it names their studio file and folders and the
//! people their queries frame, so it is never readable by others, not even
//! while it is written. A folder that has to be created is made `0700`.
//!
//! Syncing the folder is best effort: the rename has already happened, and
//! some file systems refuse to sync a folder, so a failure there leaves the
//! flush to the system rather than failing a save that is in place.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The file-system steps of [`write_atomic`] and of the check before it.
pub trait Disk {
    /// The text of the file at `path` (lossily decoded), or `None` when
    /// there is none.
    fn read(&mut self, path: &Path) -> io::Result<Option<String>>;
    /// Creates the folder `dir` and its parents where missing.
    fn create_dir_all(&mut self, dir: &Path) -> io::Result<()>;
    /// Creates `path`, which must not exist, for its owner only, writes
    /// `bytes` to it and syncs it to the disk.
    fn write_new(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Renames `from` over `to`, replacing it.
    fn replace(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    /// Syncs the entries of the folder `dir` to the disk.
    fn sync_dir(&mut self, dir: &Path) -> io::Result<()>;
    /// Removes the file `path`.
    fn remove(&mut self, path: &Path) -> io::Result<()>;
}

/// The system's file system.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDisk;

impl Disk for SystemDisk {
    fn read(&mut self, path: &Path) -> io::Result<Option<String>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn create_dir_all(&mut self, dir: &Path) -> io::Result<()> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)
    }

    fn write_new(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    fn replace(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn sync_dir(&mut self, dir: &Path) -> io::Result<()> {
        if cfg!(unix) {
            std::fs::File::open(dir)?.sync_all()
        } else {
            Ok(())
        }
    }

    fn remove(&mut self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }
}

/// Temp files made by this process so far, for unique names.
static TEMPS: AtomicU64 = AtomicU64::new(0);

/// A temp file's path beside `path`: hidden, named after it, this process
/// and a count, so two saves never share one.
pub fn temp_beside(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} names no file", path.display()))?;
    let n = TEMPS.fetch_add(1, Ordering::Relaxed);
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    temp.push(format!(".{}-{n}.tmp", std::process::id()));
    Ok(path.with_file_name(temp))
}

/// Replaces the file at `path` with `bytes` through `disk`; see the module
/// docs. The error names the step that failed.
pub fn write_atomic(path: &Path, bytes: &[u8], disk: &mut dyn Disk) -> Result<(), String> {
    let temp = temp_beside(path)?;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    disk.create_dir_all(dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let written = disk
        .write_new(&temp, bytes)
        .map_err(|e| (e.kind(), format!("cannot write {}: {e}", temp.display())))
        .and_then(|()| {
            disk.replace(&temp, path).map_err(|e| {
                (
                    e.kind(),
                    format!(
                        "cannot rename {} to {}: {e}",
                        temp.display(),
                        path.display()
                    ),
                )
            })
        });
    if let Err((kind, why)) = written {
        // A temp file that was already there is not this save's.
        if kind != io::ErrorKind::AlreadyExists {
            let _ = disk.remove(&temp);
        }
        return Err(why);
    }
    let _ = disk.sync_dir(dir);
    Ok(())
}
