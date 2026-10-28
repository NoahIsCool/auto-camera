//! Keeping the machine awake while an output session is live.
//!
//! A session is live from [`OutputController::start`] until it is
//! `Finished` or `Failed`: while it is `Starting` (the encoder probe runs),
//! `Running`, and `Stopping` (ffmpeg flushes the recording), so the machine
//! sleeps neither before the first frame nor before the file is final.
//! [`SessionAwake`] holds one [`KeepAwake`] (macOS `caffeinate`, see
//! [`ac_output::keep_awake`]) for that span: the start takes it, the first
//! poll that finds the session over releases it, and so do shutdown and
//! drop, after the encoder has finished. Taking and releasing a hold is a
//! spawn, or a kill and a reap, about a millisecond each, so both run on the
//! engine thread, unlike the encoder's start and finish.
//!
//! The hold is best effort and never touches the output. On a platform
//! without `caffeinate`, with the hold turned off, or when `caffeinate`
//! cannot start or ends early, the session runs on and the view says why
//! ([`KeepAwakeState::NotHeld`]). A `caffeinate` that ended early is reaped
//! by the next poll and not restarted within the session.
//!
//! What is kept awake is fixed when the engine starts: the display too when
//! program feeds one
//! ([`MonitorSettings::feeds_display`](crate::MonitorSettings::feeds_display)).

use ac_output::{AwakeMode, Caffeinate, KeepAwake};

use super::OutputController;
use crate::snapshot::{KeepAwakeState, OutputState};

/// The keep-awake hold of the current session; see the module docs.
#[derive(Debug)]
pub(super) struct SessionAwake {
    /// How the machine is held awake.
    caffeinate: Caffeinate,
    /// What is kept awake.
    mode: AwakeMode,
    /// The hold of the live session, while `caffeinate` runs.
    hold: Option<KeepAwake>,
    /// What the view says.
    state: KeepAwakeState,
}

impl SessionAwake {
    /// Holds sessions awake with `caffeinate` in `mode`.
    pub(super) fn new(caffeinate: Caffeinate, mode: AwakeMode) -> Self {
        Self {
            caffeinate,
            mode,
            hold: None,
            state: KeepAwakeState::Idle,
        }
    }

    /// Takes the hold for a new session, ending one still held.
    pub(super) fn hold(&mut self) {
        self.release();
        match self.caffeinate.hold(self.mode) {
            Ok(hold) => {
                self.state = KeepAwakeState::Held(hold.pid());
                self.hold = Some(hold);
            }
            Err(why) => self.state = KeepAwakeState::NotHeld(why.to_string()),
        }
    }

    /// Releases the hold once the session is over (`live` is false), and
    /// reaps a `caffeinate` that ended by itself.
    pub(super) fn check(&mut self, live: bool) {
        if !live {
            self.release();
            return;
        }
        let Some(hold) = &mut self.hold else {
            return;
        };
        let ended = match hold.try_wait() {
            Ok(None) => return,
            Ok(Some(status)) => format!("caffeinate exited with {status}"),
            Err(e) => format!("cannot watch caffeinate: {e}"),
        };
        // Dropping the hold reaps it (and kills it if it could not be
        // watched).
        self.hold = None;
        self.state = KeepAwakeState::NotHeld(ended);
    }

    /// Ends the hold, if any: `caffeinate` is killed and reaped.
    pub(super) fn release(&mut self) {
        if let Some(hold) = self.hold.take() {
            hold.stop();
        }
        self.state = KeepAwakeState::Idle;
    }

    /// What the view says.
    pub(super) fn state(&self) -> KeepAwakeState {
        self.state.clone()
    }
}

impl OutputController {
    /// Holds the machine awake in `mode` with `caffeinate` while each
    /// session is live; see the `awake` module. A controller built without
    /// this holds nothing ([`Caffeinate::Off`]).
    pub fn with_keep_awake(mut self, caffeinate: Caffeinate, mode: AwakeMode) -> Self {
        self.awake = SessionAwake::new(caffeinate, mode);
        self
    }

    /// True from a start until the session is finished or failed.
    pub(super) fn live(&self) -> bool {
        matches!(
            self.state,
            OutputState::Starting | OutputState::Running | OutputState::Stopping
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutputSettings;
    use std::time::{Duration, Instant};

    /// Settings whose ffmpeg does not exist, so a session fails to start.
    fn settings() -> OutputSettings {
        OutputSettings {
            width: 64,
            height: 36,
            ffmpeg: "/nonexistent/ffmpeg-for-ac-engine-tests".into(),
            ..OutputSettings::default()
        }
    }

    /// A unique recording path that is never written.
    fn path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ac-engine-awake-{}-{name}.mp4", std::process::id()))
    }

    /// Polls `c` until its session is no longer live, for at most 20 s.
    fn until_over(c: &mut OutputController) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while c.live() && Instant::now() < deadline {
            c.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!c.live(), "{:?}", c.state());
    }

    #[test]
    fn a_bare_controller_holds_nothing() {
        let mut c = OutputController::new(settings());
        assert_eq!(c.view().keep_awake, KeepAwakeState::Idle);
        c.start(None, Some(path("bare")), None).unwrap();
        assert!(
            matches!(&c.view().keep_awake, KeepAwakeState::NotHeld(why) if why.contains("turned off")),
            "{:?}",
            c.view().keep_awake
        );
        until_over(&mut c);
        assert_eq!(c.view().keep_awake, KeepAwakeState::Idle);
    }

    #[test]
    fn without_caffeinate_a_session_runs_and_says_why() {
        let mut c = OutputController::new(settings())
            .with_keep_awake(Caffeinate::Unsupported, AwakeMode::System);
        c.start(None, Some(path("unsupported")), None).unwrap();
        assert_eq!(c.state(), &OutputState::Starting);
        assert!(
            matches!(&c.view().keep_awake, KeepAwakeState::NotHeld(why) if why.contains("macOS")),
            "{:?}",
            c.view().keep_awake
        );
        let missing = Caffeinate::program("/nonexistent/caffeinate-for-ac-engine-tests");
        let mut c = OutputController::new(settings()).with_keep_awake(missing, AwakeMode::System);
        c.start(None, Some(path("missing")), None).unwrap();
        assert!(
            matches!(&c.view().keep_awake, KeepAwakeState::NotHeld(why) if why.contains("cannot start")),
            "{:?}",
            c.view().keep_awake
        );
        until_over(&mut c);
        assert!(matches!(c.state(), OutputState::Failed(_)));
        assert_eq!(c.view().keep_awake, KeepAwakeState::Idle);
    }

    /// The tests that run a stand-in `caffeinate`.
    #[cfg(unix)]
    mod process {
        use super::*;

        /// A stand-in that runs `script` in `sh`.
        fn stand_in(script: &str) -> Caffeinate {
            Caffeinate::command("/bin/sh", ["-c", script, "fake-caffeinate"])
        }

        /// True while process `pid` exists, a zombie included.
        fn exists(pid: u32) -> bool {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        }

        /// The process id the view says is held.
        fn held(c: &OutputController) -> u32 {
            match c.view().keep_awake {
                KeepAwakeState::Held(pid) => pid,
                other => panic!("not held: {other:?}"),
            }
        }

        #[test]
        fn the_hold_starts_with_the_session_and_ends_with_it() {
            let mut c = OutputController::new(settings())
                .with_keep_awake(stand_in("exec sleep 60"), AwakeMode::System);
            c.start(None, Some(path("session")), None).unwrap();
            let pid = held(&c);
            assert!(exists(pid));
            // ffmpeg is missing: the session fails, and the hold goes with it.
            until_over(&mut c);
            assert!(matches!(c.state(), OutputState::Failed(_)));
            assert_eq!(c.view().keep_awake, KeepAwakeState::Idle);
            assert!(!exists(pid), "{pid} is left behind");
            // The next session takes a new hold.
            c.start(None, Some(path("again")), None).unwrap();
            let again = held(&c);
            assert!(exists(again));
            c.shutdown();
            assert_eq!(c.view().keep_awake, KeepAwakeState::Idle);
            assert!(!exists(again), "{again} is left behind");
        }

        #[test]
        fn a_caffeinate_that_ends_early_is_reaped_and_reported() {
            let mut awake = SessionAwake::new(stand_in("exit 7"), AwakeMode::System);
            awake.hold();
            let KeepAwakeState::Held(pid) = awake.state() else {
                panic!("not held: {:?}", awake.state());
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            while matches!(awake.state(), KeepAwakeState::Held(_)) {
                assert!(Instant::now() < deadline, "the exit is never noticed");
                awake.check(true);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                matches!(&awake.state(), KeepAwakeState::NotHeld(why) if why.contains("exited")),
                "{:?}",
                awake.state()
            );
            assert!(!exists(pid), "{pid} is not reaped");
            // Not restarted while the session is live; idle once it is over.
            awake.check(true);
            assert!(matches!(awake.state(), KeepAwakeState::NotHeld(_)));
            awake.check(false);
            assert_eq!(awake.state(), KeepAwakeState::Idle);
        }

        #[test]
        fn dropping_the_controller_ends_the_hold() {
            let mut c = OutputController::new(settings())
                .with_keep_awake(stand_in("exec sleep 60"), AwakeMode::System);
            c.start(None, Some(path("drop")), None).unwrap();
            let pid = held(&c);
            drop(c);
            assert!(!exists(pid), "{pid} is left behind");
        }
    }
}
