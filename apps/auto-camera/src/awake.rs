//! Keeping the system awake while live: the app's part.
//!
//! On macOS a supervised `caffeinate` holds the system awake
//! ([`ac_output::keep_awake`]). The engine holds it while an output session
//! is live ([`EngineOptions::keep_awake`](ac_engine::EngineOptions)), which
//! covers the operator window; its monitors show program, so the display
//! stays awake too. A headless run holds it for the whole run instead
//! ([`hold_for_run`]): from before the engine starts, since a detector load
//! can take minutes, until the engine has shut down and the recording is
//! final. It turns the engine's own hold off, so one `caffeinate` runs at a
//! time, and leaves the display free to sleep, since nothing shows program.
//!
//! The libraries print nothing, so the notes are worded and logged here, on
//! stderr. Where `caffeinate` does not exist (every platform but macOS) the
//! step is skipped with a note: once when the window opens
//! ([`window_note`]) and at the start of a headless run. A `caffeinate`
//! that cannot start is noted the same way for a headless run; for a
//! session, the engine's output view says so.

use ac_output::{AwakeMode, Caffeinate, KeepAwake, NotHeld};

/// Logs `note` on stderr, as the app's other messages.
pub fn log(note: &str) {
    eprintln!("auto-camera: {note}");
}

/// The note for a hold that was not taken.
fn note(why: &NotHeld) -> String {
    format!("not keeping the system awake while live: {why}")
}

/// Holds the system awake in `mode` for a whole headless run, or passes a
/// note to `log` and returns `None`; nothing is noted when the hold was
/// turned off on purpose ([`Caffeinate::Off`]).
pub fn hold_for_run(
    caffeinate: &Caffeinate,
    mode: AwakeMode,
    mut log: impl FnMut(&str),
) -> Option<KeepAwake> {
    match caffeinate.hold(mode) {
        Ok(hold) => Some(hold),
        Err(NotHeld::Off) => None,
        Err(why) => {
            log(&note(&why));
            None
        }
    }
}

/// What to log when the window opens: a note when this platform has no
/// `caffeinate`, so its output sessions will not be held awake.
pub fn window_note(caffeinate: &Caffeinate) -> Option<String> {
    matches!(caffeinate, Caffeinate::Unsupported).then(|| note(&NotHeld::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_notes_only_a_platform_without_caffeinate() {
        let note = window_note(&Caffeinate::Unsupported).expect("a note");
        assert!(note.contains("only on macOS"), "{note}");
        assert_eq!(
            window_note(&Caffeinate::program("/usr/bin/caffeinate")),
            None
        );
        assert_eq!(window_note(&Caffeinate::Off), None);
        let system = window_note(&Caffeinate::system());
        assert_eq!(system.is_some(), !cfg!(target_os = "macos"), "{system:?}");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn elsewhere_the_step_is_skipped_with_a_logged_note() {
        let mut notes = Vec::new();
        let hold = hold_for_run(&Caffeinate::system(), AwakeMode::System, |n| {
            notes.push(n.to_owned())
        });
        assert!(hold.is_none());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("only on macOS"), "{notes:?}");
        assert_eq!(window_note(&Caffeinate::system()), Some(notes[0].clone()));
    }

    #[test]
    fn a_caffeinate_that_cannot_start_is_noted_and_off_is_not() {
        let mut notes = Vec::new();
        let missing = Caffeinate::program("/nonexistent/caffeinate-for-auto-camera-tests");
        assert!(hold_for_run(&missing, AwakeMode::System, |n| notes.push(n.to_owned())).is_none());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("cannot start"), "{notes:?}");
        assert!(
            hold_for_run(&Caffeinate::Off, AwakeMode::System, |n| notes
                .push(n.to_owned()))
            .is_none()
        );
        assert_eq!(notes.len(), 1, "off is not noted: {notes:?}");
    }
}
