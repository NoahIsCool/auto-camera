//! The watcher's notes about audio inputs (`[[audio_inputs]]`: a recording
//! of the board, or the board's own device).
//!
//! # Theory
//!
//! The log keeps what happened to each audio input, so the operator can
//! learn, during the show or after it, that the board was missing, came on
//! air or went away, and why. An input is noted when it is first seen (its
//! format once a connection described one, its state with the reason it is
//! not delivering, and whether it is program audio), then at each change
//! of state, with the reason when there is one:
//! `audio input board: running -> offline (audio device "SQ" went away: ...)`.
//!
//! An input that cannot be opened is retried by its supervisor every few
//! seconds, so its state flickers between offline and reconnecting (when a
//! poll catches it) for as long as it stays missing, each failed attempt
//! repeating the reason. Those changes are noted only when the reason reads
//! differently from the one noted last (numbers aside, as for detector
//! errors, see the parent module), and a reason that changes while the
//! state stays is noted the same way. So a board left unplugged logs why
//! once, with the UIDs of the devices that are connected, and again only
//! when that list changes. The memory of the reason ends when the input
//! runs and is heard: the next outage is noted with its reason whatever it
//! reads.
//!
//! A running input's error is the trouble it delivers through (a device's
//! discontinuities). The input logs that itself, at most a line every
//! 10 s with a count, so the watcher leaves it out, with one exception:
//! program audio that is not heard because it runs at another rate than
//! the output was started with (a board plugged in, or back, at another
//! rate).
//! The engine says so as the input's error ([`AudioInputView::unheard`]),
//! and the watcher notes it like the reason of a state, at WARN:
//! `audio input board: offline -> running (Soundboard runs at 44100 Hz;
//! the output was started at 48000 Hz: not heard until the output is
//! restarted)`.
//!
//! # Invariants
//!
//! * Pure, like the rest of the watcher: the notes depend on the views
//!   seen so far only.
//! * The final snapshot shows every input stopped by the shutdown; that
//!   change is not noted.

use std::collections::HashMap;

use ac_core::SourceId;
use ac_engine::AudioInputView;
use ac_media::SourceState;
use tracing::Level;

use super::{Note, lower, shape};

/// What the watcher remembers of one audio input.
#[derive(Debug)]
pub(super) struct InputSeen {
    /// Its state as last noted: a retry the log leaves out does not
    /// change it, so the next note reads on from the last one.
    state: SourceState,
    /// The shape of the reason last noted since it last ran.
    reason: Option<String>,
}

/// Notes about input `a` given what `seen` remembers of the inputs, in a
/// snapshot of an engine that is `running` (else its final one); see the
/// module docs.
pub(super) fn observe(
    seen: &mut HashMap<SourceId, InputSeen>,
    a: &AudioInputView,
    running: bool,
    notes: &mut Vec<Note>,
) {
    let id = &a.id.0;
    let reason = a
        .error
        .as_deref()
        .filter(|_| a.state != SourceState::Running || a.unheard);
    let Some(last) = seen.get_mut(&a.id) else {
        let format = a.format.map_or(String::new(), |(rate, channels)| {
            let s = if channels == 1 { "" } else { "s" };
            format!("{channels} channel{s} at {rate} Hz, ")
        });
        let program = if a.program { ", program audio" } else { "" };
        let level = if reason.is_some() {
            Level::WARN
        } else {
            Level::INFO
        };
        notes.push((
            level,
            format!(
                "audio input {id} ({}): {format}{}{program}",
                a.name,
                state_of(a.state, reason)
            ),
        ));
        seen.insert(
            a.id.clone(),
            InputSeen {
                state: a.state,
                reason: reason.map(shape),
            },
        );
        return;
    };
    if !running && a.state == SourceState::Stopped {
        return;
    }
    let new_reason = reason.filter(|r| last.reason.as_deref() != Some(shape(r).as_str()));
    let retry = matches!(
        (last.state, a.state),
        (SourceState::Offline, SourceState::Reconnecting)
            | (SourceState::Reconnecting, SourceState::Offline)
    );
    if last.state != a.state && (!retry || new_reason.is_some()) {
        let level = if a.state == SourceState::Running && reason.is_none() {
            Level::INFO
        } else {
            Level::WARN
        };
        notes.push((
            level,
            format!(
                "audio input {id}: {} -> {}",
                lower(&last.state),
                state_of(a.state, reason)
            ),
        ));
        last.state = a.state;
    } else if last.state == a.state
        && let Some(r) = new_reason
    {
        notes.push((
            Level::WARN,
            format!("audio input {id}: {}", state_of(a.state, Some(r))),
        ));
    }
    if a.state == SourceState::Running && reason.is_none() {
        last.reason = None;
    } else if let Some(r) = reason {
        last.reason = Some(shape(r));
    }
}

/// `state`, with `reason` when there is one: `offline (why)`.
fn state_of(state: SourceState, reason: Option<&str>) -> String {
    match reason {
        Some(why) => format!("{} ({why})", lower(&state)),
        None => lower(&state),
    }
}

#[cfg(test)]
mod tests {
    use ac_engine::{Engine, EngineOptions, StudioConfig};
    use ac_sources::{AudioInputConfig, SourceConfig};

    use super::super::Watcher;
    use super::*;
    use crate::logs::fixtures;

    /// The notes of `snap`'s audio inputs, as the watcher `w` gives them.
    fn notes_of(w: &mut Watcher, snap: &ac_engine::EngineSnapshot) -> Vec<(Level, String)> {
        w.observe(snap)
            .into_iter()
            .filter(|(_, text)| text.starts_with("audio input "))
            .collect()
    }

    /// The board's input, program audio, `state` with `error` and no
    /// format yet.
    fn board(state: SourceState, error: Option<&str>) -> AudioInputView {
        AudioInputView {
            id: SourceId("board".into()),
            name: "Soundboard".into(),
            state,
            error: error.map(str::to_owned),
            format: None,
            pick: None,
            program: true,
            blocks: 0,
            dropped: 0,
            unusable: 0,
            unheard: false,
        }
    }

    /// Why the board does not open while the devices `connected` are.
    fn missing(connected: &str) -> String {
        format!(
            "no audio device with UID \"SQ-1\" is connected (no other device is opened \
             in its place); the input devices connected: {connected}"
        )
    }

    /// The fixture snapshot with only `input` as its audio input.
    fn with(input: AudioInputView) -> ac_engine::EngineSnapshot {
        let mut snap = fixtures::snapshot();
        snap.audio_inputs = vec![input];
        snap
    }

    #[test]
    fn a_board_back_at_another_rate_than_the_output_is_noted_as_not_heard() {
        let mut w = Watcher::default();
        let mut on_air = board(SourceState::Running, None);
        on_air.format = Some((48_000, 2));
        assert_eq!(notes_of(&mut w, &with(on_air)).len(), 1);
        let gone = "audio device \"SQ\" went away: Device sample rate changed";
        let snap = with(board(SourceState::Offline, Some(gone)));
        assert_eq!(
            notes_of(&mut w, &snap),
            [(
                Level::WARN,
                format!("audio input board: running -> offline ({gone})")
            )]
        );
        assert!(notes_of(&mut w, &with(board(SourceState::Reconnecting, Some(gone)))).is_empty());
        let text = ac_engine::program_audio::unheard("Soundboard", 44_100, 48_000);
        let mut back = board(SourceState::Running, Some(&text));
        back.format = Some((44_100, 2));
        back.unheard = true;
        assert_eq!(
            notes_of(&mut w, &with(back.clone())),
            [(
                Level::WARN,
                format!("audio input board: offline -> running ({text})")
            )]
        );
        // Once, however long it lasts.
        assert!(notes_of(&mut w, &with(back.clone())).is_empty());
        // The output restarted at its rate: heard, nothing to note.
        back.unheard = false;
        back.error = None;
        assert!(notes_of(&mut w, &with(back)).is_empty());
    }

    #[test]
    fn a_missing_device_is_noted_once_per_reason_and_its_outages_after() {
        let mut w = Watcher::default();
        let mic =
            "\"BuiltInMicrophoneDevice\" (MacBook Pro Microphone, 1 input channel at 48000 Hz)";
        let away = missing(mic);
        let snap = with(board(SourceState::Offline, Some(&away)));
        assert_eq!(
            notes_of(&mut w, &snap),
            [(
                Level::WARN,
                format!("audio input board (Soundboard): offline ({away}), program audio")
            )]
        );
        // Its supervisor retries: the same reason is not noted again.
        for state in [
            SourceState::Reconnecting,
            SourceState::Offline,
            SourceState::Reconnecting,
            SourceState::Offline,
        ] {
            let snap = with(board(state, Some(&away)));
            assert!(notes_of(&mut w, &snap).is_empty(), "{state:?}");
        }
        // Another device is plugged in: the list changed.
        let other = missing(&format!(
            "{mic}, \"Scarlett-2i2\" (Scarlett 2i2, 2 input channels at 48000 Hz)"
        ));
        let snap = with(board(SourceState::Offline, Some(&other)));
        assert_eq!(
            notes_of(&mut w, &snap),
            [(Level::WARN, format!("audio input board: offline ({other})"))]
        );
        // The board comes on air, after a retry the log left out.
        let snap = with(board(SourceState::Reconnecting, Some(&other)));
        assert!(notes_of(&mut w, &snap).is_empty());
        let mut on_air = board(SourceState::Running, None);
        on_air.format = Some((48_000, 32));
        assert_eq!(
            notes_of(&mut w, &with(on_air.clone())),
            [(Level::INFO, "audio input board: offline -> running".into())]
        );
        // A discontinuity is the input's own to log.
        on_air.error = Some("audio discontinuity (1 so far): device overload".into());
        assert!(notes_of(&mut w, &with(on_air.clone())).is_empty());
        // Unplugged: the reason, then the retries' own reason once.
        let gone = "audio device \"SQ\" went away: the device is no longer available";
        let snap = with(board(SourceState::Offline, Some(gone)));
        assert_eq!(
            notes_of(&mut w, &snap),
            [(
                Level::WARN,
                format!("audio input board: running -> offline ({gone})")
            )]
        );
        assert!(notes_of(&mut w, &with(board(SourceState::Reconnecting, Some(gone)))).is_empty());
        let snap = with(board(SourceState::Offline, Some(&other)));
        assert_eq!(
            notes_of(&mut w, &snap),
            [(Level::WARN, format!("audio input board: offline ({other})"))]
        );
        // Back, and away again for the same reason: a new outage is noted.
        assert_eq!(notes_of(&mut w, &with(on_air)).len(), 1);
        let snap = with(board(SourceState::Offline, Some(&other)));
        assert_eq!(
            notes_of(&mut w, &snap),
            [(
                Level::WARN,
                format!("audio input board: running -> offline ({other})")
            )]
        );
    }

    #[test]
    fn numbers_alone_do_not_make_a_new_reason() {
        let mut w = Watcher::default();
        let at = |rate: u32| missing(&format!("\"Mic\" (Mic, 1 input channel at {rate} Hz)"));
        let snap = with(board(SourceState::Offline, Some(&at(48_000))));
        assert_eq!(notes_of(&mut w, &snap).len(), 1);
        // The microphone was switched to another rate.
        let snap = with(board(SourceState::Offline, Some(&at(44_100))));
        assert!(notes_of(&mut w, &snap).is_empty());
    }

    #[test]
    fn a_running_input_reads_its_format_and_an_end_warns() {
        let mut w = Watcher::default();
        let mut input = board(SourceState::Running, None);
        input.format = Some((44_100, 1));
        input.program = false;
        assert_eq!(
            notes_of(&mut w, &with(input.clone())),
            [(
                Level::INFO,
                "audio input board (Soundboard): 1 channel at 44100 Hz, running".into()
            )]
        );
        // A recording that played to its end while the engine runs.
        input.state = SourceState::Stopped;
        assert_eq!(
            notes_of(&mut w, &with(input)),
            [(Level::WARN, "audio input board: running -> stopped".into())]
        );
    }

    #[test]
    fn the_shutdown_stopping_an_input_is_not_noted() {
        let mut w = Watcher::default();
        let mut input = board(SourceState::Running, None);
        input.format = Some((48_000, 2));
        assert_eq!(notes_of(&mut w, &with(input.clone())).len(), 1);
        input.state = SourceState::Stopped;
        let mut last = with(input);
        last.running = false;
        let notes = w.observe(&last);
        assert!(
            notes.iter().all(|(_, t)| !t.starts_with("audio input")),
            "{notes:?}"
        );
        assert!(
            notes.last().unwrap().1.starts_with("engine stopped"),
            "{notes:?}"
        );
    }

    #[test]
    fn a_device_input_that_cannot_open_is_in_the_log_with_why() {
        // One synthetic source and the board, by a UID no machine has:
        // Linux and a build without `coreaudio` cannot open it, and a Mac
        // build that can does not find it. Either way the first snapshot
        // has it offline, and the log says why.
        let mut config = StudioConfig::with_source(
            "stage",
            SourceConfig::Synthetic {
                width: 320,
                height: 180,
                fps: 30.0,
                subjects: 1,
            },
        );
        config.audio_inputs.push(ac_engine::AudioInputEntry {
            id: "board".into(),
            name: Some("Soundboard".into()),
            channels: None,
            channel_count: None,
            input: AudioInputConfig::Device {
                uid: "AppleUSBAudioEngine:Allen&Heath:SQ:no-such-board:1".into(),
            },
        });
        config.output.audio_source = Some("board".into());
        let engine = Engine::start_with(config, EngineOptions::default()).expect("engine");
        let snap = engine.handle().snapshot();
        let notes = Watcher::default().observe(&snap);
        engine.shutdown().expect("shutdown");
        let view = &snap.audio_inputs[0];
        let why = view.error.as_deref().expect("why it is offline");
        // Offline, or retrying on a slow machine (the first retry is due
        // 0.5 s after the start).
        let state = lower(&view.state);
        assert!(
            ["offline", "reconnecting"].contains(&state.as_str()),
            "{view:?}"
        );
        assert!(
            notes.contains(&(
                Level::WARN,
                format!("audio input board (Soundboard): {state} ({why}), program audio")
            )),
            "{notes:?}"
        );
    }
}
