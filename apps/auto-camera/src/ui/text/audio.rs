//! The program audio input's lines in the output panel.
//!
//! # Theory
//!
//! When program audio comes from an audio input (the board), the operator
//! needs one line that says whether what the engine does to it now is what
//! goes out. While the input runs and is heard, that is the drift
//! correction applied to its blocks ([`ac_engine::AudioInputView`]'s
//! `correction_ppm`), with the resets and steps the correction made. A
//! correction is a value of the input's running connection, so while the
//! input does not run, the line shows the state and the error instead: the
//! last correction is not being applied and would mislead. An input the
//! live output does not hear, because it runs at another rate than the
//! output was started with, shows only its error, which already names it
//! and says how to hear it again (restart the output); its correction
//! would be of blocks the pacer drops.
//!
//! A running input's error is something else: trouble it delivered
//! through (a device's discontinuities, counted), which it keeps showing
//! until a newer one replaces it or a new connection delivers, so for the
//! rest of a show after one glitch. Its correction is still being applied,
//! and a gap it reported is what the correction makes up as a step, so the
//! line keeps the correction and its steps, and the notice goes on a weak
//! line of its own under it ([`drift_notice`]): it says what happened, not
//! what is wrong now. In its place it would hide the correction for the
//! rest of the connection behind an amber line nobody can act on.
//!
//! # Invariants
//!
//! * Pure: the lines depend on the snapshot only.
//! * The line is plain whenever the input runs and is heard, notice or
//!   not; amber whenever it is not on air as it should be (not running,
//!   or not heard), weak only while it starts with nothing wrong yet.
//! * An error is on exactly one of the two lines: the notice line only
//!   while the input runs and is heard, the drift line otherwise.

use ac_engine::EngineSnapshot;
use ac_media::SourceState;

use super::Tone;

/// The program audio input's line for the output panel, and its tone. While
/// it runs, the drift correction of its blocks, plain, whatever notice it
/// reports ([`drift_notice`]): "Soundboard drift correction -37.3 ppm · 1
/// reset · 2 steps made up" (below zero for a board whose clock runs
/// fast; 0.0 while its blocks pass as they are; steps are the silence put
/// in, or audio cut, for a step in its timestamps). While it does not run,
/// its state and error instead, as no correction is being applied:
/// "Soundboard offline: <why>" in amber, weak while it starts. While it is
/// not heard for its rate, its error alone, in amber: "Soundboard runs at
/// 96000 Hz; the output was started at 48000 Hz: not heard until the
/// output is restarted". `None` while program audio is not an audio
/// input's.
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
    if !state.is_empty() {
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

/// The trouble the program audio input reports while it runs and is heard
/// (a device's discontinuities: "audio discontinuity (1 so far): host time
/// of 1 block(s) from block 10 was up to 30.0 ms off, then came back: no
/// audio missing"), for a weak line under [`drift_line`]'s; see the module
/// docs. `None` when it reports none, when its error is the drift line's
/// (it does not run, or is not heard), or while program audio is not an
/// audio input's.
pub fn drift_notice(snap: &EngineSnapshot) -> Option<String> {
    let input = snap.audio_inputs.iter().find(|a| a.program)?;
    let heard = input.state == SourceState::Running && !input.unheard;
    input.error.clone().filter(|_| heard)
}
