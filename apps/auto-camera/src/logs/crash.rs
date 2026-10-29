//! Crash reports: a panic on any thread writes a file next to the log.
//!
//! # Theory
//!
//! A panic is the one failure the log cannot be trusted with: the panicking
//! thread may be the one that would have logged, the process may be about
//! to end, and the line queue may be full. So the panic hook
//! ([`install`]) writes a crash report itself, synchronously, into the log
//! folder: what panicked (thread, message, place), the backtrace, the
//! app's version, and a summary of the engine's newest snapshot, then
//! logs one line saying where the report is, prints that path on stderr
//! and hands the panic to the hook that was there before (which prints the
//! usual message, or libtest's).
//!
//! The snapshot summary is rendered ahead of time: the snapshot watcher
//! (`watch` module) renders the newest one a few times a second and
//! stores the text with [`CrashReporter::set_summary`]. Rendering in the
//! hook would read engine state that the panicking thread may be in the
//! middle of changing; storing text means the hook only copies a string.
//!
//! # Invariants
//!
//! * The hook never blocks on a lock: the summary is taken with
//!   `try_lock`, retried a few times a millisecond apart (the watcher
//!   holds it only to copy text in), and replaced by a note when it stays
//!   busy, as when the panicking thread itself holds it. A poisoned lock
//!   still yields its text.
//! * Memory is bounded: the summary is cut at [`MAX_SUMMARY`] bytes when
//!   stored, the message at [`MAX_MESSAGE`], the backtrace at
//!   [`MAX_BACKTRACE`]. The backtrace is the one allocation that grows
//!   with the panic (with the depth of the stack), as in Rust's own hook.
//! * At most [`MAX_REPORTS`] reports are written per process, so a panic
//!   in a loop cannot fill the disk, and [`prune`] keeps the newest
//!   [`KEEP_REPORTS`] in the folder. A report never overwrites a file.
//! * The previous hook always runs, also when the report cannot be
//!   written.

use std::backtrace::Backtrace;
use std::fmt::Write as _;
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use std::time::Duration;

use super::time::Utc;
use super::writer::one_line;

/// Longest snapshot summary kept, in bytes.
pub const MAX_SUMMARY: usize = 8 * 1024;

/// Longest panic message written, in bytes.
pub const MAX_MESSAGE: usize = 4 * 1024;

/// Longest backtrace written, in bytes.
pub const MAX_BACKTRACE: usize = 64 * 1024;

/// Reports written per process at most.
pub const MAX_REPORTS: usize = 8;

/// Reports kept in the folder by [`prune`].
pub const KEEP_REPORTS: usize = 20;

/// Name prefix of report files.
const PREFIX: &str = "crash-";

/// Tries at taking the summary before giving up.
const SUMMARY_TRIES: u32 = 5;

/// The summary text before the watcher has stored one.
const NO_SUMMARY: &str = "(no snapshot yet)";

/// The previous panic hook, as [`install`] keeps it.
pub type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// Writes crash reports into one folder; see the module docs.
#[derive(Debug)]
pub struct CrashReporter {
    /// The folder (the log's).
    dir: PathBuf,
    /// The app's name, version and commit, for the report's header.
    app: String,
    /// The newest snapshot summary.
    summary: Mutex<String>,
    /// Reports this process has started.
    reports: AtomicUsize,
}

/// The reporter the snapshot watcher stores summaries into, once the
/// app's log session has registered one ([`register`]).
static REPORTER: OnceLock<Arc<CrashReporter>> = OnceLock::new();

impl CrashReporter {
    /// A reporter writing into `dir` (which must exist), naming the app as
    /// `app` in every report.
    pub fn new(dir: &Path, app: &str) -> Self {
        Self {
            dir: dir.to_owned(),
            app: app.to_owned(),
            summary: Mutex::new(NO_SUMMARY.to_owned()),
            reports: AtomicUsize::new(0),
        }
    }

    /// Stores `text` as the newest snapshot summary, cut at
    /// [`MAX_SUMMARY`] bytes; the lock is held only for the copy.
    pub fn set_summary(&self, text: &str) {
        let text = cut(text, MAX_SUMMARY);
        let mut summary = self.summary.lock().unwrap_or_else(|e| e.into_inner());
        summary.clear();
        summary.push_str(text);
    }

    /// The stored summary, without waiting on its lock; see the module
    /// docs.
    fn summary(&self) -> String {
        for _ in 0..SUMMARY_TRIES {
            match self.summary.try_lock() {
                Ok(text) => return text.clone(),
                Err(TryLockError::Poisoned(p)) => return p.into_inner().clone(),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        "(the summary was being updated and could not be read)".into()
    }

    /// Writes the report of the panic `info` describes; the path when it
    /// was written.
    pub fn report(&self, info: &PanicHookInfo<'_>) -> Option<PathBuf> {
        let n = self.reports.fetch_add(1, Ordering::Relaxed);
        if n >= MAX_REPORTS {
            return None;
        }
        let thread = std::thread::current();
        let text = render(&Report {
            app: &self.app,
            time: Utc::now(),
            thread: thread.name().unwrap_or("<unnamed>"),
            message: &panic_message(info),
            location: info.location().map(|l| l.to_string()),
            backtrace: &Backtrace::force_capture().to_string(),
            summary: &self.summary(),
        });
        let name = format!(
            "{PREFIX}{}-{}-{n}.txt",
            Utc::now().compact(),
            std::process::id()
        );
        let path = self.dir.join(name);
        let mut file = std::fs::File::create_new(&path).ok()?;
        file.write_all(text.as_bytes()).ok()?;
        Some(path)
    }
}

/// Makes `reporter` the one summaries go to ([`publish_summary`]); the
/// first registration wins.
pub fn register(reporter: Arc<CrashReporter>) {
    let _ = REPORTER.set(reporter);
}

/// Stores `text` as the newest snapshot summary of the registered
/// reporter, if there is one.
pub fn publish_summary(text: &str) {
    if let Some(reporter) = REPORTER.get() {
        reporter.set_summary(text);
    }
}

/// Installs a panic hook that writes `reporter`'s report, logs and prints
/// where it is, then runs the hook that was installed before, which is
/// returned (a test puts it back).
pub fn install(reporter: Arc<CrashReporter>) -> Arc<PanicHook> {
    let previous: Arc<PanicHook> = Arc::new(std::panic::take_hook());
    let chained = previous.clone();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(path) = reporter.report(info) {
            let thread = std::thread::current();
            tracing::error!(
                target: "auto_camera::crash",
                "panic on thread {}: {}; crash report {}",
                thread.name().unwrap_or("<unnamed>"),
                cut(&panic_message(info), 200),
                path.display()
            );
            eprintln!("auto-camera: crash report written to {}", path.display());
        }
        chained(info);
    }));
    previous
}

/// Removes all but the newest `keep` crash reports in `dir` (names sort
/// by time); returns how many were removed.
pub fn prune(dir: &Path, keep: usize) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut reports: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(".txt"))
        })
        .collect();
    reports.sort();
    let excess = reports.len().saturating_sub(keep);
    reports
        .drain(..excess)
        .filter(|p| std::fs::remove_file(p).is_ok())
        .count()
}

/// The text of a panic's payload, cut at [`MAX_MESSAGE`] bytes.
fn panic_message(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(a panic without a message)");
    cut(text, MAX_MESSAGE).to_owned()
}

/// What a report says.
struct Report<'a> {
    /// The app's name, version and commit.
    app: &'a str,
    /// When the panic happened.
    time: Utc,
    /// The panicking thread's name.
    thread: &'a str,
    /// The panic message.
    message: &'a str,
    /// Where it panicked.
    location: Option<String>,
    /// The backtrace.
    backtrace: &'a str,
    /// The newest snapshot summary.
    summary: &'a str,
}

/// The text of a report.
fn render(r: &Report<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Auto Camera crash report");
    let _ = writeln!(out, "app: {}", r.app);
    let _ = writeln!(out, "time: {}", r.time.rfc3339());
    let _ = writeln!(out, "pid: {}", std::process::id());
    let _ = writeln!(out, "thread: {}", r.thread);
    let _ = writeln!(out, "panic: {}", one_line(r.message, MAX_MESSAGE));
    let _ = writeln!(out, "at: {}", r.location.as_deref().unwrap_or("unknown"));
    let _ = writeln!(out, "\nlast snapshot:\n{}", r.summary.trim_end());
    let _ = writeln!(
        out,
        "\nbacktrace:\n{}",
        cut(r.backtrace, MAX_BACKTRACE).trim_end()
    );
    out
}

/// The longest prefix of `text` of at most `max` bytes that ends on a
/// character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests;
