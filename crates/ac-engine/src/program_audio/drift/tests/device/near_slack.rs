//! Cross-PR tests: a CoreAudio board's real loss close to the pacer's
//! slack, stamped by the device input's own [`Stamper`], made up by
//! program audio's drift correction and paced by the output's
//! [`AudioPacer`], which now cuts exactly the silence it owes.
//!
//! # Theory
//!
//! The device's sample clock skips a stretch of audio, as after an
//! overload when the HAL resumes at a sample time of its own: 85 to 99.9
//! ms, just under the pacer's slack ([`AUDIO_SLACK`], 100 ms). The device
//! input keeps the loss in its PTS at once (the map stays on the old
//! timeline while its excursion lasts; the gap is reported once its window
//! has passed). The drift correction holds the first block after it out
//! of its fit and makes the loss up with as much silence once
//! [`CONFIRM_BLOCKS`] blocks at the new level confirm it, ahead of the
//! block that confirmed it; the blocks before that one pass as they are.
//! Meanwhile the pacer sees the audio at hand short of the video's share
//! by the loss and by what the board has yet to deliver: close to the
//! slack that runs it out of slack first, and it fills up to the target.
//! The correction's silence then comes as late audio, from which the
//! pacer cuts what it owes, to the frame. Either way the board's audio
//! ends up where it was, the loss made up once.
//!
//! Each block the pacer pushes has an *offset*, the encoder's position at
//! it less its PTS: where the board's audio plays against the video, up to
//! a constant (where the output started on the board's timeline). Only
//! silence and cuts move it, and the drift correction's resampling, alike
//! in a run and its twin, the same board without the loss (played by the
//! parent module's [`play_from`]): the twin's mean offset over the last
//! second is where the board's audio belongs.
//!
//! # Invariants checked
//!
//! For blocks of 512 and 1024 frames at 48 kHz, boards 0.5 and ±100 ppm
//! off, 85 to 99.9 ms lost at each IO cycle over a tick, and the output
//! started as the device input opened (the board's first frame 3 ms later)
//! or a second after:
//!
//! * one gap reported, of the time lost; one step made up and no break;
//!   the pacer's silence, where it filled, cut exactly;
//! * the *lasting offset*, the run's mean offset over the last second less
//!   the twin's, within [`LASTING_MS`];
//! * *sync*: every block within the spec's [`SYNC_MS`] of where the twin
//!   has the board's audio, but for the blocks that confirm the step and
//!   play before the silence that makes it up (at most
//!   `CONFIRM_BLOCKS − 1`, one after another, right after the loss): each
//!   off by no more than the loss, early by it when the pacer did not
//!   fill, late by the board's shortfall before the loss when it did.
//!
//! The pacer fills a gap in some of these runs, so its cut is exercised.

use ac_audio::drift::CONFIRM_BLOCKS;
use ac_sources::audio_input::device::timing::GLITCH_WINDOW;

use super::super::SYNC_MS;
use super::*;

/// When the loss comes, in seconds of the device's clock: after the drift
/// correction of a board 100 ppm off has taken hold.
const LOSS_AT: f64 = 5.0;

/// How long a run plays, in seconds.
const PLAY: u64 = 8;

/// Most the board's audio may end up off where its twin has it, in ms.
const LASTING_MS: f64 = 5.0;

/// The losses played, in ms: from 85 to 99.9, a millisecond apart.
const LOSSES: [f64; 16] = [
    85.0, 86.0, 87.0, 88.0, 89.0, 90.0, 91.0, 92.0, 93.0, 94.0, 95.0, 96.0, 97.0, 98.0, 99.0, 99.9,
];

/// The frames in `ms` of audio at [`RATE`], to the nearest.
fn frames_in(ms: f64) -> u64 {
    (ms * f64::from(RATE) / 1000.0).round() as u64
}

/// A device at [`RATE`] with IO buffers of `frames`, `ppm` fast, losing
/// `ms` of audio at `at` seconds of its clock if given.
fn board(frames: u64, ppm: f64, loss: Option<(f64, f64)>) -> Device {
    let events: Vec<_> = loss
        .iter()
        .map(|&(at, ms)| (at, Event::Skip(frames_in(ms))))
        .collect();
    Device {
        ppm,
        ..Device::new(RATE, frames).with(&events)
    }
}

/// The mean offset of `run`'s blocks with PTS over its last second, in
/// seconds.
fn settled(run: &Run) -> f64 {
    let last = (PLAY - 1) as f64;
    let (sum, n) = run
        .offsets
        .iter()
        .filter(|(pts, _)| *pts >= last)
        .fold((0.0, 0u32), |(sum, n), (_, o)| (sum + o, n + 1));
    sum / f64::from(n.max(1))
}

/// What is wrong with `run`, which lost `ms` of audio, `ppm` fast, against
/// `base`, its twin's settled offset; empty when nothing is.
fn faults(run: &Run, ms: f64, ppm: f64, base: f64) -> Vec<String> {
    let mut wrong = Vec::new();
    // The host time lost, give or take the jitter either side of it and
    // the drift over the excursion the gap is reported at the end of
    // (measured against the old timeline at the nominal rate: 50 us over
    // its window at 100 ppm), a block or two included.
    let lost = frames_in(ms) as f64 / (f64::from(RATE) * (1.0 + ppm * 1e-6));
    let drift = (GLITCH_WINDOW.as_secs_f64() + 0.1) * ppm.abs() * 1e-6;
    match gaps(run) {
        Some(by) if run.found.len() == 1 && (by - lost).abs() <= 2.0 * JITTER + drift => {}
        _ => wrong.push(format!("reported {:?}", run.found)),
    }
    let (view, pacer) = (&run.view, &run.pacer);
    if (view.drift_steps, view.drift_resets) != (1, 0) || pacer.silence != pacer.dropped {
        wrong.push(format!("{view:?}, {pacer:?}"));
    }
    let left = (settled(run) - base) * 1e3;
    if left.abs() > LASTING_MS {
        wrong.push(format!("{left:+.2} ms left"));
    }
    // Blocks out of sync: only those that confirm the step, one after
    // another, right after the loss, each off by no more than it.
    let off: Vec<_> = run
        .offsets
        .iter()
        .enumerate()
        .map(|(i, &(pts, o))| (i, pts, (o - base) * 1e3))
        .filter(|&(_, _, by)| by.abs() > SYNC_MS as f64)
        .collect();
    let confirming = off.len() < CONFIRM_BLOCKS as usize
        && off.windows(2).all(|w| w[1].0 == w[0].0 + 1)
        && off
            .iter()
            .all(|&(_, pts, by)| pts >= LOSS_AT && by.abs() <= ms + 0.5);
    if !confirming {
        let off: Vec<_> = off
            .iter()
            .map(|(_, pts, by)| format!("{pts:.4} s {by:+.2} ms"))
            .collect();
        wrong.push(format!("out of sync: {}", off.join(", ")));
    }
    wrong
}

/// Plays a board with blocks of 512 and 1024 frames, at 0.5 and ±100 ppm,
/// losing each of [`LOSSES`] at each IO cycle over a tick from [`LOSS_AT`],
/// with the output started at tick `output_at`; returns what went wrong,
/// and in how many runs the pacer filled a gap.
fn lose_close_to_the_slack(output_at: u64) -> (Vec<String>, u32) {
    let (mut wrong, mut filled) = (Vec::new(), 0);
    for frames in [512u64, 1024] {
        let buffer = frames as f64 / f64::from(RATE);
        let cycles = (1.0 / FPS as f64 / buffer).ceil() as u32;
        for ppm in [0.5, 100.0, -100.0] {
            let twin = play_from(board(frames, ppm, None), PLAY, output_at);
            let base = settled(&twin);
            for ms in LOSSES {
                for cycle in 0..cycles {
                    let at = LOSS_AT + f64::from(cycle) * buffer;
                    let run = play_from(board(frames, ppm, Some((at, ms))), PLAY, output_at);
                    filled += u32::from(run.pacer.silence > 0);
                    for fault in faults(&run, ms, ppm, base) {
                        wrong.push(format!(
                            "{frames} frames, {ppm} ppm, {ms} ms lost at {at:.4} s: {fault}"
                        ));
                    }
                }
            }
        }
    }
    (wrong, filled)
}

#[test]
fn a_real_loss_close_to_the_slack_after_the_output_started_is_made_up_once_in_sync() {
    // The output starts a second after the board, as when a show goes live
    // with its inputs running.
    let (wrong, filled) = lose_close_to_the_slack(FPS);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(filled > 0, "the pacer never ran out of slack");
}

#[test]
fn a_real_loss_close_to_the_slack_of_a_board_opened_with_the_output_is_made_up_once_in_sync() {
    // The output starts as the device input opens: the board runs behind
    // the video's share by its first frame's 3 ms and a block, and the
    // pacer runs out of slack more often.
    let (wrong, filled) = lose_close_to_the_slack(0);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(filled > 0, "the pacer never ran out of slack");
}
