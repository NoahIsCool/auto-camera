//! The battery's families of host times that only scatter or wander and
//! lose no audio: white jitter within the jump threshold (M1) and beyond it
//! (M2, S2), bursts of jitter (M3), host times that go late in steps too
//! small to jump and come back in one (M4), and a sample clock far off the
//! host's under jitter (M10). See [`super`] for how a case is judged.
//!
//! # Theory
//!
//! None of these loses audio or moves the timestamps for good, so the
//! device input's map must end where it began: any notice of a gap or a
//! rewind says its map moved, and the PTS with it. Such a move is not cut
//! or filled at once (the PTS do not step), but drift correction reads PTS
//! that walk ahead of the audio as a clock that runs slow, and stretches
//! the board's audio by as much: lip sync slides later by each move, and
//! the pacer drops at its lead limit in the end. So the lead is compared
//! with a twin's over the whole run (for jitter alone, the same device
//! without jitter; for the ramps, the same jitter without them), and its
//! walk is taken over runs long enough for the correction to show it:
//! minutes in the full grid, where rare bursts and long buffers that give
//! the window few readings had walked the map by tens of milliseconds.
//!
//! # Invariants checked
//!
//! The families' pass criteria, as [`super`] lists them: M1, M2, M3, M4,
//! M10 and S2.

use ac_audio::DriftSettings;

use super::*;

/// The formats of a family that sweeps them: `quick` in the default grid,
/// every rate of `rates` with every size of `sizes` in the full one.
fn formats(sweep: Sweep, quick: &[(u32, u64)], rates: &[u32], sizes: &[u64]) -> Vec<(u32, u64)> {
    let full: Vec<_> = rates
        .iter()
        .flat_map(|&rate| sizes.iter().map(move |&frames| (rate, frames)))
        .collect();
    sweep.pick(quick, &full)
}

/// Judges a case of jitter alone: no rewind, and no walk beyond `walk_ms`.
fn judge_scatter(run: &Run, twin: &Run, walk_ms: f64) -> Judge {
    let mut judge = Judge::default();
    judge.require("no rewind", Notices::of(run).rewinds.is_empty());
    judge.at_most("walk (ms)", walk_size(run, twin), walk_ms);
    judge
}

#[test]
fn m1_white_jitter_within_the_threshold_moves_nothing() {
    let mut card = Card::new("M1 no fault, white jitter up to ±0.5 ms");
    let sweep = card.sweep;
    let formats = formats(
        sweep,
        &[(48_000, 128), (48_000, 512), (96_000, 2048), (44_100, 4096)],
        &[44_100, 48_000, 96_000],
        &[128, 256, 512, 1024, 2048, 4096],
    );
    let peaks = sweep.pick(&[0.3, 0.5], &[0.15, 0.3, 0.5]);
    let seeds = sweep.pick(&[1], &[1, 2]);
    let seconds = sweep.of(30, 600);
    for (rate, frames) in formats {
        let twin = play(jittered(rate, frames, 0.0, 0), seconds);
        for &peak in &peaks {
            for &seed in &seeds {
                let run = play(jittered(rate, frames, peak / 1000.0, seed), seconds);
                let mut judge = Judge::default();
                judge.require("no notice", run.found.is_empty());
                untouched_by(&mut judge, &run);
                judge.at_most("apart (ms)", run.apart(&twin, 0.0), 1.5);
                let walked = walk_size(&run, &twin);
                judge.at_most("walk (ms)", walked, 1.5);
                let case = format!("{rate} Hz, {frames} frames, ±{peak} ms, seed {seed}");
                card.add(case, judge, numbers(&run, &twin));
            }
        }
    }
    card.finish();
}

#[test]
fn m2_white_jitter_beyond_the_threshold_is_no_rewind_no_gap_and_no_walk() {
    let mut card = Card::new("M2 white jitter ±0.75 to ±2 ms, 128 to 1024 frames");
    let sweep = card.sweep;
    let formats = formats(
        sweep,
        &[(48_000, 128), (48_000, 512), (48_000, 1024)],
        &[44_100, 48_000, 96_000],
        &[128, 256, 512, 1024],
    );
    let peaks = sweep.pick(&[0.75, 1.5, 2.0], &[0.75, 1.0, 1.5, 2.0]);
    let seeds = sweep.pick(&[1], &[1, 2]);
    let seconds = sweep.of(30, 600);
    for (rate, frames) in formats {
        let twin = play(jittered(rate, frames, 0.0, 0), seconds);
        for &peak in &peaks {
            for &seed in &seeds {
                let run = play(jittered(rate, frames, peak / 1000.0, seed), seconds);
                let mut judge = judge_scatter(&run, &twin, 2.0);
                judge.require("no gap", Notices::of(&run).gaps.is_empty());
                let case = format!("{rate} Hz, {frames} frames, ±{peak} ms, seed {seed}");
                card.add(case, judge, numbers(&run, &twin));
            }
        }
    }
    card.finish();
}

#[test]
fn m3_bursts_of_jitter_are_no_rewind_and_no_walk() {
    let mut card = Card::new("M3 bursts of ±1.25 to ±2 ms, 0.3 s every 5 to 60 s");
    let sweep = card.sweep;
    let sizes = sweep.pick(&[128, 512, 1024], &[128, 256, 512, 1024]);
    let peaks = sweep.pick(&[1.5, 2.0], &[1.25, 1.5, 2.0]);
    // How often a burst comes, and how long a run of them plays.
    let periods = sweep.pick(
        &[(5.0, 30), (20.0, 60)],
        &[(5.0, 300), (20.0, 600), (60.0, 1800)],
    );
    for frames in sizes {
        for &(every, seconds) in &periods {
            let twin = play(jittered(RATE, frames, 0.0, 0), seconds);
            for &peak in &peaks {
                for late_only in [false, true] {
                    let bursts = Bursts {
                        peak: peak / 1000.0,
                        late_only,
                        from: 2.0,
                        every,
                        lasting: 0.3,
                    };
                    let device = Device {
                        bursts: Some(bursts),
                        ..jittered(RATE, frames, JITTER, 1)
                    };
                    let run = play(device, seconds);
                    let judge = judge_scatter(&run, &twin, 2.0);
                    let sides = if late_only { "late-only" } else { "two-sided" };
                    let case = format!(
                        "{frames} frames, ±{peak} ms {sides} every {every} s for {seconds} s"
                    );
                    card.add(case, judge, numbers(&run, &twin));
                }
            }
        }
    }
    card.finish();
}

/// Every 10 s from 5 s on (and `phase` buffers later), host times that go
/// late by `step` more on each of `steps` buffers and are then back in one
/// step, for `seconds`, at IO buffers of `frames` at `rate`.
fn ramps(
    rate: u32,
    frames: u64,
    (steps, step): (u64, f64),
    phase: u64,
    seconds: u64,
) -> Vec<(f64, Event)> {
    let periods = (seconds as f64 - 5.0) / 10.0;
    (0..periods.ceil() as u64)
        .flat_map(|p| {
            let first = buffer_index(5.0 + 10.0 * p as f64, frames, rate) + phase;
            (0..steps).map(move |i| {
                let late = (i + 1) as f64 * step;
                (buffer_at(first + i, frames, rate), Event::Off(1, late))
            })
        })
        .collect()
}

#[test]
fn m4_host_times_that_ramp_late_and_snap_back_are_no_rewind_and_no_walk() {
    let mut card = Card::new("M4 ramp then snap every 10 s");
    let sweep = card.sweep;
    let formats = formats(
        sweep,
        &[(48_000, 256), (48_000, 512), (48_000, 1024)],
        &[44_100, 48_000, 96_000],
        &[128, 256, 512, 1024],
    );
    // Four buffers late by 0.5, 1, 1.5 and 2 ms; twelve by 0.25 ms more
    // each, to 3 ms.
    let patterns = [(4u64, 0.5e-3), (12, 0.25e-3)];
    let phases = sweep.pick(&[0], &[0, 1, 2]);
    let seconds = sweep.of(60, 300);
    for (rate, frames) in formats {
        let twin = play(jittered(rate, frames, JITTER, 1), seconds);
        for pattern in patterns {
            for &phase in &phases {
                let events = ramps(rate, frames, pattern, phase, seconds);
                let run = play(jittered(rate, frames, JITTER, 1).with(&events), seconds);
                let mut judge = Judge::default();
                judge.require("no rewind", Notices::of(&run).rewinds.is_empty());
                let walked = walk_size(&run, &twin);
                judge.at_most("walk (ms)", walked, 1.5);
                let (steps, step) = pattern;
                let case = format!(
                    "{rate} Hz, {frames} frames, {steps} x {} ms, phase {phase}",
                    step * 1000.0
                );
                card.add(case, judge, numbers(&run, &twin));
            }
        }
    }
    card.finish();
}

#[test]
fn m10_a_clock_far_off_the_hosts_under_jitter_is_corrected_with_no_notice_and_no_walk() {
    let mut card = Card::new("M10 sample clock ±200 ppm, white jitter ±0.5 ms");
    let sweep = card.sweep;
    let formats = formats(
        sweep,
        &[(48_000, 512)],
        &[44_100, 48_000, 96_000],
        &[128, 512, 1024, 4096],
    );
    let ppms = sweep.pick(&[-200.0, 200.0], &[-200.0, -100.0, 100.0, 200.0]);
    let seconds = sweep.of(180, 600);
    let dead_band = DriftSettings::default().dead_band_ppm;
    for (rate, frames) in formats {
        for &ppm in &ppms {
            let device = |peak, seed| Device {
                ppm,
                ..jittered(rate, frames, peak, seed)
            };
            let twin = play(device(0.0, 0), seconds);
            let run = play(device(0.5e-3, 1), seconds);
            let mut judge = Judge::default();
            judge.require("no notice", run.found.is_empty());
            // Settled where it cancels the drift, within the dead band the
            // jitter left in the PTS makes the estimate wander over.
            let cancels = -ppm / (1.0 + ppm * 1e-6);
            let off = (run.view.correction_ppm - cancels).abs();
            judge.at_most("correction off its settling (ppm)", off, dead_band);
            let walked = walk_size(&run, &twin);
            judge.at_most("walk (ms)", walked, 1.5);
            let case = format!("{rate} Hz, {frames} frames, {ppm:+} ppm");
            card.add(case, judge, numbers(&run, &twin));
        }
    }
    card.finish();
}

#[test]
fn should_s2_white_jitter_beyond_the_threshold_on_long_buffers_does_not_walk() {
    let mut card = Card::new("S2 white jitter ±1 to ±2 ms, 2048 and 4096 frames");
    let sweep = card.sweep;
    let formats = formats(
        sweep,
        &[(48_000, 2048), (48_000, 4096)],
        &[44_100, 48_000, 96_000],
        &[2048, 4096],
    );
    let peaks = sweep.pick(&[2.0], &[1.0, 1.5, 2.0]);
    let seeds = sweep.pick(&[1], &[1, 2]);
    let seconds = sweep.of(120, 1800);
    for (rate, frames) in formats {
        let twin = play(jittered(rate, frames, 0.0, 0), seconds);
        for &peak in &peaks {
            for &seed in &seeds {
                let run = play(jittered(rate, frames, peak / 1000.0, seed), seconds);
                let mut judge = Judge::default();
                let walked = walk_size(&run, &twin);
                judge.at_most("walk (ms)", walked, 2.0);
                let case = format!("{rate} Hz, {frames} frames, ±{peak} ms, seed {seed}");
                card.add(case, judge, numbers(&run, &twin));
            }
        }
    }
    card.finish();
}
