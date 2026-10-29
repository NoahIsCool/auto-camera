//! The program audio input's line in the output panel.
//!
//! # Theory
//!
//! When program audio comes from an audio input (the board), the operator
//! needs one line that says whether what the engine does to it now is what
//! goes out. While the input runs and is heard, that is the drift
//! correction applied to its blocks ([`ac_engine::AudioInputView`]'s
//! `correction_ppm`), with the resets and steps the correction made. A
//! correction is a value of the input's running connection, so while the
//! input does not run, or runs with an error, the line shows the state and
//! the error instead: the last correction is not being applied and would
//! mislead. An input the live output does not hear, because it runs at
//! another rate than the output was started with, shows only its error,
//! which already names it and says how to hear it again (restart the
//! output); its correction would be of blocks the pacer drops.
//!
//! # Invariants
//!
//! * Pure: the line depends on the snapshot only.
//! * Plain only while the input runs, is heard and reports no error;
//!   amber whenever it is not on air as it should be, weak only while it
//!   starts with nothing wrong yet.

use ac_engine::EngineSnapshot;
use ac_media::SourceState;

use super::Tone;

/// The program audio input's line for the output panel, and its tone. While
/// it runs, the drift correction of its blocks, plain: "Soundboard drift
/// correction -37.3 ppm · 1 reset · 2 steps made up" (below zero for a
/// board whose clock runs fast; 0.0 while its blocks pass as they are;
/// steps are the silence put in, or audio cut, for a step in its
/// timestamps). While it does not run, or has an error, its state and
/// error instead, as no correction is being applied: "Soundboard offline:
/// <why>" in amber, weak while it starts. While it is not heard for its
/// rate, its error alone, in amber: "Soundboard runs at 96000 Hz; the
/// output was started at 48000 Hz: not heard until the output is
/// restarted". `None` while program audio is not an audio input's.
pub fn drift_line(snap: &EngineSnapshot) -> Option<(String, Tone)> {
    let input = snap.audio_inputs.iter().find(|a| a.program)?;
    if input.unheard {
        let why = input
            .error
            .clone()
            .unwrap_or_else(|| format!("{} is not heard at its rate", input.name));
        return Some((why, Tone::Warning));
    }
    let state = match input.state {
        SourceState::Running => "",
        SourceState::Starting => " starting",
        SourceState::Offline => " offline",
        SourceState::Reconnecting => " reconnecting",
        SourceState::Stopped => " stopped",
    };
    if !state.is_empty() || input.error.is_some() {
        let why = input.error.as_ref().map(|e| format!(": {e}"));
        let tone = match (input.state, &why) {
            (SourceState::Starting, None) => Tone::Weak,
            _ => Tone::Warning,
        };
        let line = format!("{}{state}{}", input.name, why.unwrap_or_default());
        return Some((line, tone));
    }
    let mut line = format!(
        "{} drift correction {:.1} ppm",
        input.name, input.correction_ppm
    );
    for (n, one, many) in [
        (input.drift_resets, "reset", "resets"),
        (input.drift_steps, "step made up", "steps made up"),
    ] {
        match n {
            0 => {}
            1 => line.push_str(&format!(" · 1 {one}")),
            n => line.push_str(&format!(" · {n} {many}")),
        }
    }
    Some((line, Tone::Plain))
}
