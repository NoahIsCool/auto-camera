//! Keeping the Mac awake while program goes out: macOS's `caffeinate` as a
//! supervised child process.
//!
//! # Theory
//!
//! Left alone, a Mac sleeps: the system after its idle delay, the disks and
//! the display after theirs. A live service runs for hours with nobody at
//! the keyboard (spec C.3: 2 h without a restart, 99.9% of program frames on
//! time), and a sleep stops the whole pipeline in the middle of the stream.
//! macOS stays awake while some process holds a power assertion, and
//! `caffeinate` holds the ones its flags name until it exits:
//!
//! | Flag | Effect |
//! |---|---|
//! | `-i` | no idle sleep of the system |
//! | `-m` | no idle sleep of the disks |
//! | `-s` | no system sleep while on AC power |
//! | `-d` | no display sleep; only when program feeds a display ([`AwakeMode::SystemAndDisplay`]) |
//! | `-w <pid>` | exit, releasing the rest, when process `pid` (the app) exits |
//!
//! Power assertions are IOKit calls, and the workspace forbids `unsafe`
//! code, so instead of calling them the app runs `caffeinate`, the way it
//! runs ffmpeg. The assertions keep the machine awake; they do not stop App
//! Nap from throttling the app while its window is minimized or hidden. App
//! Nap is turned off per app, by `LSAppNapIsDisabled` in an app bundle's
//! `Info.plist`, which this repository does not build yet.
//!
//! # No orphans
//!
//! [`KeepAwake`] owns the child, as [`ProgramEncoder`](crate::ProgramEncoder)
//! owns ffmpeg: [`KeepAwake::stop`] and dropping it kill `caffeinate` if it
//! still runs and reap it before they return, and the assertions end with
//! the process. The child leads its own process group (`process_group(0)`,
//! as the detector sidecar does), so a terminal's Ctrl-C reaches only the
//! app, which keeps the machine awake through its clean shutdown (the
//! recording is finalized then) and ends `caffeinate` itself. When the app
//! ends without that (killed, crashed, or forced out by a second signal),
//! `-w` is the watchdog: macOS has no parent-death signal, but `caffeinate`
//! exits as soon as the process it waits for does. It starts no processes
//! of its own, so killing it, not its group, leaves nothing behind.
//!
//! # Platforms and stand-ins
//!
//! `caffeinate` exists only on macOS. [`Caffeinate::system`] is
//! [`SYSTEM_CAFFEINATE`] there and [`Caffeinate::Unsupported`] elsewhere,
//! where a hold is refused with [`NotHeld::Unsupported`] for the caller to
//! log as a note (the libraries print nothing). Any other command can stand
//! in ([`Caffeinate::command`]): the mode's arguments are appended to its
//! own, so tests run a shell that sleeps instead of the real thing.
//!
//! # Invariants
//!
//! * A [`KeepAwake`] owns exactly one child, and once it is stopped or
//!   dropped that child has been waited on: no zombie and no live process
//!   remain.
//! * A hold never blocks or fails what it keeps awake: starting one is one
//!   spawn, ending one a kill and a reap, each about a millisecond.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use thiserror::Error;

/// Where macOS keeps `caffeinate`.
pub const SYSTEM_CAFFEINATE: &str = "/usr/bin/caffeinate";

/// What a hold keeps awake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwakeMode {
    /// The system and its disks: no display shows program (a headless run).
    System,
    /// The system, its disks and the display, because program feeds one
    /// (the operator window's monitors).
    SystemAndDisplay,
}

impl AwakeMode {
    /// The mode for a program that does (`true`) or does not feed a display.
    pub fn for_display(feeds_display: bool) -> Self {
        if feeds_display {
            Self::SystemAndDisplay
        } else {
            Self::System
        }
    }
}

/// `caffeinate`'s arguments for `mode`, tied to process `pid`: `-i -m -s`,
/// `-d` with a display, then `-w <pid>`; see the module docs.
pub fn caffeinate_args(mode: AwakeMode, pid: u32) -> Vec<String> {
    let mut args: Vec<String> = ["-i", "-m", "-s"].map(String::from).into();
    if mode == AwakeMode::SystemAndDisplay {
        args.push("-d".into());
    }
    args.extend(["-w".into(), pid.to_string()]);
    args
}

/// How the machine is kept awake, or why it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caffeinate {
    /// Runs `program` with `prefix`, then the mode's [`caffeinate_args`].
    Command {
        /// The executable: `caffeinate`, or a stand-in in tests.
        program: PathBuf,
        /// Arguments before the mode's; none for `caffeinate` itself.
        prefix: Vec<String>,
    },
    /// This platform has no `caffeinate`.
    Unsupported,
    /// Turned off, because the caller keeps the machine awake itself.
    Off,
}

impl Caffeinate {
    /// The system's: [`SYSTEM_CAFFEINATE`] on macOS, else
    /// [`Caffeinate::Unsupported`].
    pub fn system() -> Self {
        if cfg!(target_os = "macos") {
            Self::program(SYSTEM_CAFFEINATE)
        } else {
            Self::Unsupported
        }
    }

    /// Runs `program` with the mode's arguments alone.
    pub fn program(program: impl Into<PathBuf>) -> Self {
        Self::command(program, Vec::<String>::new())
    }

    /// Runs `program` with `prefix` before the mode's arguments.
    pub fn command(
        program: impl Into<PathBuf>,
        prefix: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::Command {
            program: program.into(),
            prefix: prefix.into_iter().map(Into::into).collect(),
        }
    }

    /// Keeps the machine awake in `mode` until the returned hold is stopped
    /// or dropped, or until this process exits (`-w`).
    pub fn hold(&self, mode: AwakeMode) -> Result<KeepAwake, NotHeld> {
        match self {
            Self::Command { program, prefix } => {
                let args = caffeinate_args(mode, std::process::id());
                KeepAwake::spawn(program, prefix, args).map_err(|source| NotHeld::Spawn {
                    program: program.clone(),
                    source,
                })
            }
            Self::Unsupported => Err(NotHeld::Unsupported),
            Self::Off => Err(NotHeld::Off),
        }
    }
}

/// Why [`Caffeinate::hold`] started nothing.
#[derive(Debug, Error)]
pub enum NotHeld {
    /// This platform has no `caffeinate`.
    #[error("caffeinate exists only on macOS")]
    Unsupported,
    /// The hold is turned off.
    #[error("turned off, the caller keeps the system awake")]
    Off,
    /// The program could not be started.
    #[error("cannot start {program:?}: {source}")]
    Spawn {
        /// The program.
        program: PathBuf,
        /// Why.
        source: io::Error,
    },
}

/// A running `caffeinate`: the machine stays awake while this lives. See
/// the module docs.
#[derive(Debug)]
pub struct KeepAwake {
    /// The process.
    child: Child,
    /// Its process id (also its process group id on Unix).
    pid: u32,
    /// The mode's arguments it was given, after the command's own.
    args: Vec<String>,
}

impl KeepAwake {
    /// Starts `program` with `prefix` and then `args`, in its own process
    /// group, with all three standard streams closed: `caffeinate` prints
    /// only its own errors, an early exit shows in its status instead, and
    /// an inherited stderr would keep whoever reads the app's stderr
    /// waiting for it too.
    fn spawn(program: &Path, prefix: &[String], args: Vec<String>) -> io::Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(prefix)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        Ok(Self {
            pid: child.id(),
            child,
            args,
        })
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The mode's arguments the process was given ([`caffeinate_args`]).
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// How the process ended, once it has (it is then reaped), without
    /// waiting; `Ok(None)` while it runs.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Ends the hold: kills the process if it still runs, reaps it and
    /// returns how it ended (`None` when the status could not be read).
    ///
    /// A process whose state cannot be read is not signalled: `try_wait`
    /// fails only when it is no longer this process's child to wait for
    /// (something else reaped it), so its id may already name another
    /// process, and the reap then fails at once too instead of blocking.
    pub fn stop(mut self) -> Option<ExitStatus> {
        self.end()
    }

    /// [`KeepAwake::stop`], for it and for drop; idempotent, since the
    /// status of a reaped child is kept.
    fn end(&mut self) -> Option<ExitStatus> {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        self.child.wait().ok()
    }
}

impl Drop for KeepAwake {
    /// Kills and reaps the process; nothing more after [`KeepAwake::stop`].
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_without_a_display() {
        assert_eq!(
            caffeinate_args(AwakeMode::System, 4242),
            ["-i", "-m", "-s", "-w", "4242"]
        );
        assert_eq!(AwakeMode::for_display(false), AwakeMode::System);
    }

    #[test]
    fn arguments_with_a_display() {
        assert_eq!(
            caffeinate_args(AwakeMode::SystemAndDisplay, 4242),
            ["-i", "-m", "-s", "-d", "-w", "4242"]
        );
        assert_eq!(AwakeMode::for_display(true), AwakeMode::SystemAndDisplay);
    }

    #[test]
    fn the_watched_pid_is_written_in_full() {
        for (pid, text) in [(1, "1"), (u32::MAX, "4294967295")] {
            let args = caffeinate_args(AwakeMode::System, pid);
            assert_eq!(args[args.len() - 2..], ["-w", text]);
        }
    }

    #[test]
    fn the_system_caffeinate_is_macos_only() {
        if cfg!(target_os = "macos") {
            assert_eq!(
                Caffeinate::system(),
                Caffeinate::Command {
                    program: SYSTEM_CAFFEINATE.into(),
                    prefix: Vec::new(),
                }
            );
        } else {
            assert_eq!(Caffeinate::system(), Caffeinate::Unsupported);
        }
    }

    #[test]
    fn unsupported_and_off_start_nothing() {
        let err = Caffeinate::Unsupported.hold(AwakeMode::System).unwrap_err();
        assert!(matches!(err, NotHeld::Unsupported));
        assert!(err.to_string().contains("only on macOS"), "{err}");
        let err = Caffeinate::Off.hold(AwakeMode::System).unwrap_err();
        assert!(matches!(err, NotHeld::Off), "{err}");
    }

    #[test]
    fn a_missing_program_is_reported_by_name() {
        let missing = "/nonexistent/caffeinate-for-ac-output-tests";
        let err = Caffeinate::program(missing)
            .hold(AwakeMode::System)
            .unwrap_err();
        assert!(
            matches!(&err, NotHeld::Spawn { program, source }
                if program == Path::new(missing) && source.kind() == io::ErrorKind::NotFound),
            "{err:?}"
        );
        assert!(err.to_string().contains(missing), "{err}");
    }

    /// The tests that run a stand-in process.
    #[cfg(unix)]
    mod process {
        use super::*;
        use std::time::{Duration, Instant};

        /// A stand-in that runs `script` in `sh`, with the mode's arguments as
        /// `$1`... and `zero` as `$0`.
        fn stand_in(script: &str, zero: &str) -> Caffeinate {
            Caffeinate::command("/bin/sh", ["-c", script, zero])
        }

        /// True while process `pid` exists, a zombie included.
        fn exists(pid: u32) -> bool {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        }

        /// Waits up to 10 s for `done`.
        fn eventually(mut done: impl FnMut() -> bool) -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done() {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            true
        }

        #[test]
        fn a_hold_runs_with_its_arguments_until_stopped_and_is_reaped() {
            let record = std::env::temp_dir()
                .join(format!("ac-output-keep-awake-{}-args", std::process::id()));
            let _ = std::fs::remove_file(&record);
            // Writes its arguments, one per line, to `$0` (atomically), then
            // waits to be killed.
            let script = r#"printf '%s\n' "$@" > "$0.tmp" && mv "$0.tmp" "$0"; exec sleep 60"#;
            let fake = stand_in(script, record.to_str().unwrap());
            let mut hold = fake.hold(AwakeMode::SystemAndDisplay).unwrap();
            let expected = caffeinate_args(AwakeMode::SystemAndDisplay, std::process::id());
            assert_eq!(hold.args(), expected);
            assert!(eventually(|| record.exists()), "the stand-in never ran");
            let written = std::fs::read_to_string(&record).unwrap();
            assert_eq!(written.lines().collect::<Vec<_>>(), expected);
            assert!(exists(hold.pid()));
            assert!(matches!(hold.try_wait(), Ok(None)), "still running");
            let pid = hold.pid();
            let status = hold.stop().expect("reaped");
            assert!(!status.success(), "killed: {status}");
            assert!(!exists(pid), "{pid} is left behind");
            std::fs::remove_file(&record).unwrap();
        }

        #[test]
        fn dropping_a_hold_kills_and_reaps_it() {
            let hold = stand_in("exec sleep 60", "fake-caffeinate")
                .hold(AwakeMode::System)
                .unwrap();
            let pid = hold.pid();
            assert!(exists(pid));
            drop(hold);
            assert!(!exists(pid), "{pid} is left behind");
        }

        #[test]
        fn a_process_that_exits_by_itself_is_reaped_with_its_status() {
            let mut hold = stand_in("exit 3", "fake-caffeinate")
                .hold(AwakeMode::System)
                .unwrap();
            let pid = hold.pid();
            let mut status = None;
            assert!(eventually(|| {
                status = hold.try_wait().unwrap();
                status.is_some()
            }));
            assert_eq!(status.and_then(|s| s.code()), Some(3));
            assert!(!exists(pid), "reaped by try_wait");
            // Stopping a hold that already ended kills nothing and keeps the
            // status.
            assert_eq!(hold.stop().and_then(|s| s.code()), Some(3));
        }

        #[test]
        fn a_hold_leads_its_own_process_group() {
            let hold = stand_in("exec sleep 60", "fake-caffeinate")
                .hold(AwakeMode::System)
                .unwrap();
            let out = Command::new("ps")
                .args(["-o", "pgid=", "-p", &hold.pid().to_string()])
                .output()
                .expect("ps runs");
            let group = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            assert_eq!(group, hold.pid().to_string());
        }
    }
}
