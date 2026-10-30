//! The battery's families of a connection's start: a first buffer stamped
//! off, alone and with a cycle lost right after it (M8), and a second
//! buffer's glitch (M9). See [`super`] for how a case is judged.
//!
//! # Theory
//!
//! A device's first IO cycle is the likeliest to be stamped off, and the
//! first two buffers cannot say which of them was: only the blocks after
//! them can. A first buffer off is a constant in the connection's PTS,
//! like its latency, so nothing may be reported, filled or cut for it,
//! and a loss right after it must be kept whole (not less the first
//! buffer's error) to be made up once: by a drift step, or, for a cycle
//! shorter than drift correction's step (128 frames at 96 kHz), as timing
//! error with no step, as the loss alone is. A second buffer's glitch is a
//! glitch like any other. Longer IO buffers than a hand-off block are cut
//! into blocks that share their buffer's host time, so at 2048 and 4096
//! frames the second *block* is still the first buffer's, and the buffers
//! are told apart by later blocks than at short sizes. Both open the
//! connection either just after its first frame's capture (a device
//! already running) or before it.
//!
//! # Invariants checked
//!
//! The families' pass criteria, as [`super`] lists them: M8 and M9.

use ac_audio::DriftSettings;

use super::*;

/// When the connection opened against its first frame's capture: just
/// after it (a device already running), or this many seconds before.
const OPENS: [Option<f64>; 2] = [None, Some(0.030)];

/// The device at `rate` with IO buffers of `frames`, opened as `opened`
/// says (see [`OPENS`]), with `events`.
fn device(rate: u32, frames: u64, opened: Option<f64>, events: &[(f64, Event)]) -> Device {
    let device = Device::new(rate, frames).with(events);
    match opened {
        Some(s) => device.opened_before(s),
        None => device,
    }
}

/// How `opened` reads in a case's name.
fn open_name(opened: Option<f64>) -> String {
    match opened {
        Some(s) => format!("opened {} ms before", s * 1000.0),
        None => "already running".into(),
    }
}

/// A first buffer's timestamp error cannot hide or invent a later loss.
#[test]
fn m8_a_first_buffer_stamped_off_is_not_reported_and_a_loss_after_it_is_made_up_once() {
    let mut card = Card::new("M8 first buffer off by ±3 to 30 ms");
    let sweep = card.sweep;
    let rates = sweep.pick(&[RATE], &[44_100, 48_000, 96_000]);
    let sizes = sweep.pick(&[256, 1024, 2048, 4096], &[128, 256, 512, 1024, 2048, 4096]);
    let sizes_ms = sweep.pick(&[8.0, 30.0], &[3.0, 8.0, 15.0, 30.0]);
    // Before which callback a cycle is lost, if one is.
    let losses = sweep.pick(
        &[None, Some(2), Some(5)],
        &[None, Some(2), Some(3), Some(4), Some(5)],
    );
    let seconds = 8;
    let step_ms = DriftSettings::default().step.as_secs_f64() * 1000.0;
    for &rate in &rates {
        for &frames in &sizes {
            for opened in OPENS {
                for &loss in &losses {
                    let lose: Vec<_> = loss
                        .map(|n| (buffer_at(n, frames, rate), Event::Lose(1)))
                        .into_iter()
                        .collect();
                    let twin = play(device(rate, frames, opened, &lose), seconds);
                    for &ms in &sizes_ms {
                        for by in [ms, -ms] {
                            let mut events = vec![(0.0, Event::Off(1, by / 1000.0))];
                            events.extend(&lose);
                            let run = play(device(rate, frames, opened, &events), seconds);
                            let notices = Notices::of(&run);
                            let mut judge = Judge::default();
                            if loss.is_none() {
                                judge.require("nothing reported", run.found.is_empty());
                                let view = &run.view;
                                judge.require(
                                    "no drift step or break",
                                    (view.drift_steps, view.drift_resets) == (0, 0),
                                );
                                judge.at_most("apart (ms)", run.apart(&twin, 0.0), 1.0);
                            } else {
                                let lost = buffer_at(1, frames, rate) * 1000.0;
                                judge.require(
                                    format!("the loss's gap of {lost:.2} ms alone"),
                                    one_of(&notices.gaps, lost, 0.1) && notices.len() == 1,
                                );
                                // Made up once by a drift step, or taken up
                                // as timing error when shorter than one.
                                let once = if lost > step_ms { (1, 0) } else { (0, 0) };
                                let (view, twins) = (&run.view, &twin.view);
                                judge.require(
                                    "the loss made up once",
                                    (view.drift_steps, view.drift_resets) == once
                                        && (twins.drift_steps, twins.drift_resets) == once,
                                );
                                judge.at_most("apart from 2 s (ms)", run.apart(&twin, 2.0), 1.0);
                            }
                            let case = format!(
                                "{by:+} ms, {rate} Hz, {frames} frames, {}, loss before \
                                 callback {loss:?}",
                                open_name(opened)
                            );
                            card.add(case, judge, numbers(&run, &twin));
                        }
                    }
                }
            }
        }
    }
    card.finish();
}

/// A second-buffer glitch is reported only as a glitch beyond tolerance.
#[test]
fn m9_a_glitch_of_the_second_buffer_is_no_rewind_and_no_gap() {
    let mut card = Card::new("M9 second buffer's glitch of ±3 to 30 ms");
    let sweep = card.sweep;
    let rates = sweep.pick(&[RATE], &[44_100, 48_000, 96_000]);
    let sizes = sweep.pick(&[128, 1024, 4096], &[128, 256, 512, 1024, 2048, 4096]);
    let sizes_ms = sweep.pick(&[3.0, 15.0, 30.0], &[3.0, 8.0, 15.0, 30.0]);
    let seconds = 6;
    for &rate in &rates {
        for &frames in &sizes {
            for opened in OPENS {
                let twin = play(device(rate, frames, opened, &[]), seconds);
                for &ms in &sizes_ms {
                    for by in [ms, -ms] {
                        let second = [(buffer_at(1, frames, rate), Event::Off(1, by / 1000.0))];
                        let run = play(device(rate, frames, opened, &second), seconds);
                        let notices = Notices::of(&run);
                        let mut judge = Judge::default();
                        judge.require("no rewind", notices.rewinds.is_empty());
                        judge.require("no gap", notices.gaps.is_empty());
                        if ms / 1000.0 > block_tolerance(frames, rate) {
                            judge.require("one glitch", notices.glitches.len() == 1);
                        } else {
                            judge.require("no glitch", notices.glitches.is_empty());
                        }
                        // One run may start resampling while its twin
                        // stays inside the dead band. Allow the retained
                        // half kernel and one sample's rounding, not a
                        // lasting timestamp-sized edit.
                        let filter_ms = (HALF_TAPS + 1) as f64 * 1000.0 / f64::from(rate);
                        judge.at_most(
                            "lasting lead offset (ms)",
                            run.apart(&twin, 2.0),
                            filter_ms.max(0.5),
                        );
                        let case = format!(
                            "{by:+} ms, {rate} Hz, {frames} frames, {}",
                            open_name(opened)
                        );
                        card.add(case, judge, numbers(&run, &twin));
                    }
                }
            }
        }
    }
    card.finish();
}
