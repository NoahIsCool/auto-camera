//! Keeping the system awake while live: the app's part.
//!
//! On macOS a supervised `caffeinate` holds the system awake
//! ([`ac_output::keep_awake`]). The engine holds it while an output session
//! is live ([`EngineOptions::keep_awake`](ac_engine::EngineOptions)), which
//! covers the operator window; its monitors show program, so the display
//! stays awake too. A headless run holds it for the whole run instead
//! ([`hold_for_run`]): from before the engine starts, since a detector load
//! can take minutes, until the engine has shut down and the recording is
//! final ([`end_of_run`]). It leaves the display free to sleep, since
//! nothing shows program. While the run holds, the engine's own hold is
//! turned off, so one `caffeinate` runs at a time and the engine's output
//! view says the session is held by its caller. When the run holds
//! nothing, the engine keeps its own hold: its view then says why a
//! recording is not held, and a `caffeinate` that could not start is tried
//! once more when the recording starts.
//!
//! The libraries print nothing, so the notes are worded and logged here:
//! on stderr, and as warnings in the app's log ([`crate::logs`]), so that a
//! show the system was not held awake for is on record afterwards, also
//! when the window was started without a terminal. Where `caffeinate` does
//! not exist (every platform but macOS) the step is skipped with a note,
//! and a `caffeinate` that cannot start or ends early is noted too: by a
//! headless run at its start (an early end at its end), and by the window
//! from the engine's view, each time an output session turns out not to be
//! held ([`SessionNotes`]).

use std::fmt::Display;

use ac_engine::KeepAwakeState;
use ac_output::{AwakeMode, Caffeinate, KeepAwake, NotHeld};

/// Logs `note` on stderr, as the app's other messages, and as a warning in
/// the app's log.
pub fn log(note: &str) {
    eprintln!("auto-camera: {note}");
    tracing::warn!("{note}");
}

/// The note for a session or run that is not held awake, and why.
fn note(why: impl Display) -> String {
    format!("not keeping the system awake while live: {why}")
}

/// Holds the system awake in `mode` for a whole headless run with
/// `sessions`, the engine's hold, which is then turned off
/// ([`Caffeinate::Off`]) so that the engine starts no second `caffeinate`;
/// a hold that is not taken stays the engine's (see the module docs) and is
/// noted to `log`, unless it was turned off on purpose. Returns the hold.
pub fn hold_for_run(
    sessions: &mut Caffeinate,
    mode: AwakeMode,
    mut log: impl FnMut(&str),
) -> Option<KeepAwake> {
    match sessions.hold(mode) {
        Ok(hold) => {
            *sessions = Caffeinate::Off;
            Some(hold)
        }
        Err(NotHeld::Off) => None,
        Err(why) => {
            log(&note(&why));
            None
        }
    }
}

/// Ends a headless run's hold, if it took one: `caffeinate` is killed and
/// reaped. One that had already exited by itself is noted to `log`, since
/// the run was then not held awake to its end.
pub fn end_of_run(hold: Option<KeepAwake>, mut log: impl FnMut(&str)) {
    let Some(mut hold) = hold else {
        return;
    };
    if let Ok(Some(status)) = hold.try_wait() {
        log(&format!(
            "the system was not kept awake to the end of the run: caffeinate exited with {status}"
        ));
    }
    hold.stop();
}

/// The window's notes on output sessions that are not held awake, worded
/// from the engine's output view, which [`SessionNotes::note`] is fed every
/// frame: one each time the view turns to [`KeepAwakeState::NotHeld`], so
/// when a session starts unheld (no `caffeinate` on this platform, or it
/// cannot start) and when its `caffeinate` ends early. A session shorter
/// than a frame can go unnoted.
#[derive(Debug, Default)]
pub struct SessionNotes {
    /// The view's state at the previous frame.
    last: KeepAwakeState,
}

impl SessionNotes {
    /// Takes in the view's newest `state`; returns the note to log when it
    /// has just turned to [`KeepAwakeState::NotHeld`].
    pub fn note(&mut self, state: &KeepAwakeState) -> Option<String> {
        if *state == self.last {
            return None;
        }
        self.last = state.clone();
        match state {
            KeepAwakeState::NotHeld(why) => Some(note(why)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs [`hold_for_run`] on `caffeinate`; returns the hold, what the
    /// engine is left to hold its sessions with and the notes logged.
    fn hold(mut caffeinate: Caffeinate) -> (Option<KeepAwake>, Caffeinate, Vec<String>) {
        let mut notes = Vec::new();
        let held = hold_for_run(&mut caffeinate, AwakeMode::System, |n| {
            notes.push(n.to_owned())
        });
        (held, caffeinate, notes)
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn elsewhere_the_step_is_skipped_with_a_logged_note() {
        let (held, sessions, notes) = hold(Caffeinate::system());
        assert!(held.is_none());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("only on macOS"), "{notes:?}");
        // The engine's view says so too, should the run record.
        assert_eq!(sessions, Caffeinate::Unsupported);
    }

    #[test]
    fn a_caffeinate_that_cannot_start_is_noted_and_left_to_the_engine() {
        let missing = Caffeinate::program("/nonexistent/caffeinate-for-auto-camera-tests");
        let (held, sessions, notes) = hold(missing.clone());
        assert!(held.is_none());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("cannot start"), "{notes:?}");
        assert_eq!(sessions, missing);
    }

    #[test]
    fn a_hold_turned_off_is_not_noted() {
        let (held, sessions, notes) = hold(Caffeinate::Off);
        assert!(held.is_none());
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(sessions, Caffeinate::Off);
        let mut notes = Vec::new();
        end_of_run(None, |n| notes.push(n.to_owned()));
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn the_window_notes_each_session_that_turns_out_not_held() {
        let mut notes = SessionNotes::default();
        let unsupported = KeepAwakeState::NotHeld("caffeinate exists only on macOS".into());
        let ended = KeepAwakeState::NotHeld("caffeinate exited with exit status: 7".into());
        let mut seen = Vec::new();
        for state in [
            KeepAwakeState::Idle,
            unsupported.clone(),
            unsupported.clone(),
            KeepAwakeState::Idle,
            KeepAwakeState::Held(42),
            ended.clone(),
            ended,
            KeepAwakeState::HeldByCaller,
            KeepAwakeState::Idle,
            unsupported,
        ] {
            seen.extend(notes.note(&state));
        }
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen[0].ends_with("only on macOS"), "{seen:?}");
        assert!(seen[1].ends_with("exit status: 7"), "{seen:?}");
        assert_eq!(seen[2], seen[0]);
    }

    /// The tests that run a stand-in `caffeinate` (a shell).
    #[cfg(unix)]
    mod process {
        use super::*;
        use std::time::{Duration, Instant};

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

        #[test]
        fn a_run_that_holds_turns_the_engines_hold_off() {
            let (held, sessions, notes) = hold(stand_in("exec sleep 60"));
            let mut held = held.expect("held");
            assert!(notes.is_empty(), "{notes:?}");
            assert_eq!(sessions, Caffeinate::Off);
            assert!(matches!(held.try_wait(), Ok(None)), "still running");
            let pid = held.pid();
            let mut notes = Vec::new();
            end_of_run(Some(held), |n| notes.push(n.to_owned()));
            assert!(notes.is_empty(), "{notes:?}");
            assert!(!exists(pid), "{pid} is left behind");
        }

        #[test]
        fn a_caffeinate_that_ended_during_the_run_is_noted_at_its_end() {
            let (held, _, _) = hold(stand_in("exit 7"));
            let mut held = held.expect("started");
            let deadline = Instant::now() + Duration::from_secs(10);
            while matches!(held.try_wait(), Ok(None)) {
                assert!(Instant::now() < deadline, "the stand-in never exited");
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut notes = Vec::new();
            end_of_run(Some(held), |n| notes.push(n.to_owned()));
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(notes[0].contains("not kept awake to the end"), "{notes:?}");
            assert!(notes[0].contains('7'), "{notes:?}");
        }
    }
}
