//! The log file: appended to, and rotated when it reaches its cap.
//!
//! # Theory
//!
//! A show runs for hours and the app runs for many shows, so a log that
//! only grew would fill the disk. The current log is `<stem>.log` in the
//! log folder. A line that would take it past the cap first rotates it:
//! the oldest kept file (`<stem>.<keep>.log`) is dropped, every other old
//! file moves up one number, the current file becomes `<stem>.1.log`, and
//! a new current file starts. So the folder holds at most `keep + 1` log
//! files of at most `cap` bytes each, and the newest lines are always in
//! `<stem>.log`. A new session appends to the current file, so many short
//! runs do not push a long one out.
//!
//! A rotated file must still say what wrote it, however long the session
//! has run. So the session's header (the lines given to
//! [`RotatingFile::write_header`]: build, process, config, models) is kept
//! and written again, as first written, at the top of every file a
//! rotation starts.
//!
//! Counting the bytes in memory, and renaming files, is only right while
//! one process writes the files. So the files of a stem have a lock,
//! `<stem>.lock`, held (as an advisory lock of the operating system) for
//! as long as they are open; another process asking for the same stem is
//! refused, and picks another (see the parent module). The system drops
//! the lock when the process ends, crash or not.
//!
//! # Invariants
//!
//! * One owner: the writer thread is the only one that writes (see the
//!   `writer` module); nothing here is shared, and no other process
//!   writes the same files while the lock is held.
//! * A line is never split across files. A line that does not fit beside
//!   the header goes alone (after the header) into a fresh file, so a file
//!   exceeds the cap only by holding one such line, and the writer caps
//!   lines well below it.
//! * Every file a rotation starts begins with the header, which is at most
//!   half the cap (header lines beyond that are written once only).
//! * When a rename is refused (a file locked or made read-only by hand),
//!   the current file is emptied instead: the log loses its older lines
//!   but stays within its cap.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// A size-capped, rotating log file; see the module docs.
#[derive(Debug)]
pub struct RotatingFile {
    /// The folder of the files.
    dir: PathBuf,
    /// File name stem: `auto-camera` names `auto-camera.log`.
    stem: String,
    /// Largest size of a file, in bytes.
    cap: u64,
    /// Rotated files kept.
    keep: usize,
    /// The current file.
    file: BufWriter<File>,
    /// Bytes in the current file, buffered ones included.
    len: u64,
    /// The header lines kept, without their newlines.
    header: Vec<String>,
    /// Bytes of the kept header, newlines included.
    header_len: u64,
    /// `<stem>.lock`, locked while the files are open.
    _lock: File,
}

impl RotatingFile {
    /// Opens (or creates) `<dir>/<stem>.log` for appending, taking the
    /// stem's lock. `dir` must exist; `cap` must be positive. An error of
    /// kind [`io::ErrorKind::WouldBlock`] when another holds the lock.
    pub fn open(dir: &Path, stem: &str, cap: u64, keep: usize) -> io::Result<Self> {
        if cap == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the log's size cap must be positive",
            ));
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{stem}.lock")))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("another process writes {stem}.log"),
                ));
            }
            // A file system without locks: better unlocked than no log.
            Err(TryLockError::Error(_)) => {}
        }
        let path = dir.join(format!("{stem}.log"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            dir: dir.to_owned(),
            stem: stem.to_owned(),
            cap,
            keep,
            file: BufWriter::new(file),
            len,
            header: Vec::new(),
            header_len: 0,
            _lock: lock,
        })
    }

    /// The current file's path.
    pub fn path(&self) -> PathBuf {
        self.numbered(0)
    }

    /// The path of rotated file `n` (`0` is the current file).
    pub fn numbered(&self, n: usize) -> PathBuf {
        if n == 0 {
            self.dir.join(format!("{}.log", self.stem))
        } else {
            self.dir.join(format!("{}.{n}.log", self.stem))
        }
    }

    /// Appends `line` and a newline, rotating first when the line would
    /// take the file past its cap (unless the file is empty).
    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        let size = line.len() as u64 + 1;
        if self.len > 0 && self.len + size > self.cap {
            self.rotate()?;
        }
        self.append(line)
    }

    /// Appends `line` like [`Self::write_line`] and keeps it in the header
    /// that every file a rotation starts begins with, while the header
    /// stays within half the cap.
    pub fn write_header(&mut self, line: &str) -> io::Result<()> {
        self.write_line(line)?;
        let size = line.len() as u64 + 1;
        if self.header_len + size <= self.cap / 2 {
            self.header.push(line.to_owned());
            self.header_len += size;
        }
        Ok(())
    }

    /// Writes `line` and a newline into the current file.
    fn append(&mut self, line: &str) -> io::Result<()> {
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.len += line.len() as u64 + 1;
        Ok(())
    }

    /// Writes buffered lines to the file.
    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    /// Moves every file up one number and starts a current file that holds
    /// the header; see the module docs.
    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let shifted = self.shift();
        let file = match shifted {
            Ok(()) => OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path())?,
            // Stay within the cap: start the current file again.
            Err(_) => File::create(self.path())?,
        };
        self.file = BufWriter::new(file);
        self.len = 0;
        let header = std::mem::take(&mut self.header);
        let written = header.iter().try_for_each(|line| self.append(line));
        self.header = header;
        written
    }

    /// Drops the oldest kept file and renames the others up one number,
    /// the current one to `.1`; with nothing kept, removes the current one.
    fn shift(&self) -> io::Result<()> {
        if self.keep == 0 {
            return std::fs::remove_file(self.path());
        }
        match std::fs::remove_file(self.numbered(self.keep)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        for n in (0..self.keep).rev() {
            match std::fs::rename(self.numbered(n), self.numbered(n + 1)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::testdir::TempDir;

    /// The lines of `path`, or none when it does not exist.
    fn lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .map(|t| t.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    #[test]
    fn the_log_rotates_at_its_cap() {
        let dir = TempDir::new("rotate");
        // Lines of 9 bytes and a newline: ten fit in 100 bytes.
        let mut log = RotatingFile::open(dir.path(), "auto-camera", 100, 2).unwrap();
        for i in 0..10 {
            log.write_line(&format!("line {i:04}")).unwrap();
        }
        log.flush().unwrap();
        assert_eq!(std::fs::metadata(log.path()).unwrap().len(), 100);
        assert!(!log.numbered(1).exists(), "exactly at the cap: no rotation");
        log.write_line("line 0010").unwrap();
        log.flush().unwrap();
        assert_eq!(lines(&log.path()), ["line 0010"]);
        let old = lines(&dir.path().join("auto-camera.1.log"));
        assert_eq!(old.len(), 10);
        assert_eq!(old[0], "line 0000");
        // Every file stays within the cap however much is written.
        for i in 11..100 {
            log.write_line(&format!("line {i:04}")).unwrap();
        }
        log.flush().unwrap();
        for n in 0..=2 {
            let len = std::fs::metadata(log.numbered(n)).unwrap().len();
            assert!(len <= 100, "file {n} has {len} bytes");
        }
        assert!(!log.numbered(3).exists(), "only two old files are kept");
        assert_eq!(lines(&log.path()).last().unwrap(), "line 0099");
        assert_eq!(lines(&log.numbered(1))[0], "line 0080");
        assert_eq!(lines(&log.numbered(2))[0], "line 0070");
    }

    #[test]
    fn a_new_session_appends_and_counts_what_is_there() {
        let dir = TempDir::new("append");
        let mut log = RotatingFile::open(dir.path(), "x", 30, 1).unwrap();
        log.write_line("first session").unwrap();
        drop(log);
        let mut log = RotatingFile::open(dir.path(), "x", 30, 1).unwrap();
        log.write_line("second").unwrap();
        log.flush().unwrap();
        assert_eq!(lines(&log.path()), ["first session", "second"]);
        // 14 + 7 bytes are there; 10 more would pass 30.
        log.write_line("third one").unwrap();
        log.flush().unwrap();
        assert_eq!(lines(&log.path()), ["third one"]);
        assert_eq!(lines(&log.numbered(1)), ["first session", "second"]);
    }

    #[test]
    fn a_line_longer_than_the_cap_gets_a_file_of_its_own() {
        let dir = TempDir::new("long");
        let mut log = RotatingFile::open(dir.path(), "x", 10, 3).unwrap();
        log.write_line("short").unwrap();
        log.write_line("a line much longer than ten bytes").unwrap();
        log.write_line("next").unwrap();
        log.flush().unwrap();
        assert_eq!(lines(&log.numbered(2)), ["short"]);
        assert_eq!(
            lines(&log.numbered(1)),
            ["a line much longer than ten bytes"]
        );
        assert_eq!(lines(&log.path()), ["next"]);
    }

    #[test]
    fn keeping_nothing_starts_the_file_again() {
        let dir = TempDir::new("keep0");
        let mut log = RotatingFile::open(dir.path(), "x", 12, 0).unwrap();
        log.write_line("one").unwrap();
        log.write_line("two").unwrap();
        log.write_line("three").unwrap();
        log.flush().unwrap();
        assert_eq!(lines(&log.path()), ["three"]);
        assert!(!log.numbered(1).exists());
    }

    #[test]
    fn every_file_a_rotation_starts_begins_with_the_header() {
        let dir = TempDir::new("header");
        let mut log = RotatingFile::open(dir.path(), "x", 100, 3).unwrap();
        log.write_header("build 1").unwrap();
        log.write_line("line 0000").unwrap();
        // A header line that comes later (a model's hash) joins it.
        log.write_header("model sha1").unwrap();
        for i in 1..40 {
            log.write_line(&format!("line {i:04}")).unwrap();
        }
        log.flush().unwrap();
        // 19 bytes of header leave room for eight lines of ten bytes.
        for n in 0..=3 {
            let lines = lines(&log.numbered(n));
            let first = 32 - 8 * n;
            let body: Vec<String> = (first..first + 8).map(|i| format!("line {i:04}")).collect();
            assert_eq!(lines[..2], ["build 1", "model sha1"], "file {n}");
            assert_eq!(lines[2..], body, "file {n}");
            assert!(std::fs::metadata(log.numbered(n)).unwrap().len() <= 100);
        }
        assert!(!log.numbered(4).exists());
    }

    #[test]
    fn the_header_is_kept_within_half_the_cap() {
        let dir = TempDir::new("bigheader");
        let mut log = RotatingFile::open(dir.path(), "x", 40, 2).unwrap();
        // Two lines of ten bytes fill half the cap; the third is not kept.
        for h in ["header 01", "header 02", "header 03"] {
            log.write_header(h).unwrap();
        }
        log.write_line("next line").unwrap();
        // Too long to fit beside the header: alone after it.
        log.write_line(&"x".repeat(30)).unwrap();
        log.write_line("last").unwrap();
        log.flush().unwrap();
        assert_eq!(
            lines(&log.numbered(2)),
            ["header 01", "header 02", "header 03", "next line"]
        );
        assert_eq!(
            lines(&log.numbered(1)),
            ["header 01", "header 02", &"x".repeat(30)]
        );
        assert_eq!(lines(&log.path()), ["header 01", "header 02", "last"]);
    }

    #[test]
    fn a_second_writer_of_the_same_files_is_refused() {
        let dir = TempDir::new("lock");
        let first = RotatingFile::open(dir.path(), "x", 100, 1).unwrap();
        let err = RotatingFile::open(dir.path(), "x", 100, 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert!(err.to_string().contains("x.log"), "{err}");
        assert!(
            RotatingFile::open(dir.path(), "y", 100, 1).is_ok(),
            "another stem has its own lock"
        );
        drop(first);
        assert!(
            RotatingFile::open(dir.path(), "x", 100, 1).is_ok(),
            "the lock goes with the files"
        );
    }

    #[test]
    fn bad_settings_and_folders_are_errors() {
        let dir = TempDir::new("bad");
        let err = RotatingFile::open(dir.path(), "x", 0, 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(RotatingFile::open(&dir.path().join("missing"), "x", 10, 1).is_err());
    }
}
