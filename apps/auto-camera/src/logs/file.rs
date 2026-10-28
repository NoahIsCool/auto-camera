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
//! # Invariants
//!
//! * One owner: the writer thread is the only one that writes (see the
//!   `writer` module); nothing here is shared.
//! * A line is never split across files. A line longer than the cap goes
//!   alone into a fresh file, so a file exceeds the cap only by holding
//!   one such line, and the writer caps lines well below it.
//! * When a rename is refused (a file locked or made read-only by hand),
//!   the current file is emptied instead: the log loses its older lines
//!   but stays within its cap.

use std::fs::{File, OpenOptions};
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
}

impl RotatingFile {
    /// Opens (or creates) `<dir>/<stem>.log` for appending. `dir` must
    /// exist; `cap` must be positive.
    pub fn open(dir: &Path, stem: &str, cap: u64, keep: usize) -> io::Result<Self> {
        if cap == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the log's size cap must be positive",
            ));
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
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.len += size;
        Ok(())
    }

    /// Writes buffered lines to the file.
    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    /// Moves every file up one number and starts an empty current file;
    /// see the module docs.
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
        Ok(())
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
    fn bad_settings_and_folders_are_errors() {
        let dir = TempDir::new("bad");
        let err = RotatingFile::open(dir.path(), "x", 0, 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(RotatingFile::open(&dir.path().join("missing"), "x", 10, 1).is_err());
    }
}
