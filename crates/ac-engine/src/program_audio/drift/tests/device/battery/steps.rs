//! The battery's families of one event in a running stream: a lasting
//! rewind under jitter, alone and with a loss after it (M5, S1), glitches
//! ahead and back that come home at once or in steps (M6, S3), and real
//! losses, of an IO cycle and close to the pacer's slack (M7). See
//! [`super`] for how a case is judged.
//!
//! # Theory
//!
//! Each event is one the device input's window has to tell from its
//! neighbours. A rewind that lasted moves the map, so the PTS run on with
//! no step and nothing is cut; taken for a glitch, the map goes back and
//! drift correction cuts as much real audio, a skip half a second after
//! the timestamps went back. A loss after a rewind is kept whole in the
//! PTS, so drift correction makes it up once; folded into the rewind, it
//! is not made up. A glitch leaves the PTS on the old timeline after it,
//! so nothing is made up for good; taken for a loss or a rewind, the board
//! ends early or late by its size. And a real loss must be one gap, made
//! up once, wherever in the IO cycle and the video's ticks it falls.
//!
//! # Invariants checked
//!
//! The families' pass criteria, as [`super`] lists them: M5, M6, M7, S1 and
//! S3.

use super::*;

/// The rate this module's families play at, in Hz.
const AT: u32 = RATE;

/// Most a rewind or a gap the device input reports may be off its size
/// under white jitter of `peak` seconds, in ms: the jitter either side of
/// the jump, and a little drift.
fn about(peak: f64) -> f64 {
    2.0 * peak * 1000.0 + 0.25
}

/// A lasting rewind of a family's grid: the device's IO buffer, how far
/// back the host times go, the jitter's peak, and the phase.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rewind {
    /// IO buffer frames.
    frames: u64,
    /// How far back, in ms.
    back: f64,
    /// White jitter's peak, in ms.
    peak: f64,
    /// Buffers after the family's start at which it comes.
    phase: u64,
}

/// Plays rewinds over a family's grid into `card`: each alone (one rewind
/// of about its size if `sized`, no cut, apart within `apart_ms` when
/// given), and with one cycle lost each of `afters` buffers after it (the
/// rewind and one gap, made up once, apart from the loss alone within
/// `apart_ms`), for `seconds` from `at` seconds on.
fn rewinds(
    card: &mut Card,
    cases: &[Rewind],
    afters: &[u64],
    (at, seconds): (f64, u64),
    sized: bool,
    apart_ms: Option<f64>,
) {
    for &Rewind {
        frames,
        back,
        peak,
        phase,
    } in cases
    {
        let device = || jittered(AT, frames, peak / 1000.0, 1);
        let start = buffer_index(at, frames, AT) + phase;
        let rewind = (
            buffer_at(start, frames, AT),
            Event::Off(u64::MAX, -back / 1000.0),
        );
        let what = format!("{back} ms back, {frames} frames, ±{peak} ms, phase {phase}");
        let twin = play(device(), seconds);
        let run = play(device().with(&[rewind]), seconds);
        let notices = Notices::of(&run);
        let mut judge = Judge::default();
        if sized {
            judge.require(
                format!("one rewind of about {back} ms"),
                one_of(&notices.rewinds, back, about(peak / 1000.0)) && notices.len() == 1,
            );
        } else {
            judge.require("one rewind", notices.rewinds.len() == 1);
        }
        no_cut(&mut judge, &run, &twin);
        if let Some(limit) = apart_ms {
            judge.at_most("apart (ms)", run.apart(&twin, 0.0), limit);
        }
        card.add(what.clone(), judge, numbers(&run, &twin));
        for &after in afters {
            let lose = (buffer_at(start + after, frames, AT), Event::Lose(1));
            let alone = play(device().with(&[lose]), seconds);
            let run = play(device().with(&[rewind, lose]), seconds);
            let notices = Notices::of(&run);
            let lost = buffer_at(1, frames, AT) * 1000.0;
            let within = about(peak / 1000.0);
            let mut judge = Judge::default();
            judge.require(
                "the rewind and one gap of the loss",
                one_of(&notices.rewinds, back, within)
                    && one_of(&notices.gaps, lost, within)
                    && notices.len() == 2,
            );
            let (view, alones) = (&run.view, &alone.view);
            judge.require(
                "the loss made up once",
                (view.drift_steps, view.drift_resets) == (alones.drift_steps, alones.drift_resets),
            );
            judge.require(
                "no pacer edit beyond the loss alone's",
                run.pacer.dropped <= alone.pacer.dropped,
            );
            if let Some(limit) = apart_ms {
                judge.at_most(
                    "apart from the loss alone (ms)",
                    run.apart(&alone, 0.0),
                    limit,
                );
            }
            let case = format!("{what}, a cycle lost {after} buffers on");
            card.add(case, judge, numbers(&run, &alone));
        }
    }
}

/// The rewinds of a family's grid: every size of `sizes`, back by every
/// one of `backs` ms, at every peak of `peaks` ms, at phases `0..phases`
/// seven buffers apart.
fn grid(sizes: &[u64], backs: &[f64], peaks: &[f64], phases: u64) -> Vec<Rewind> {
    let mut cases = Vec::new();
    for &frames in sizes {
        for &back in backs {
            for &peak in peaks {
                for phase in 0..phases {
                    cases.push(Rewind {
                        frames,
                        back,
                        peak,
                        phase: 7 * phase,
                    });
                }
            }
        }
    }
    cases
}

#[test]
fn m5_a_lasting_rewind_under_jitter_is_one_rewind_and_a_loss_after_it_made_up_once() {
    let mut card = Card::new("M5 lasting rewind under white jitter up to ±0.5 ms");
    let sweep = card.sweep;
    let cases = grid(
        &[256, 512, 1024],
        &[2.5, 15.0, 30.0],
        &sweep.pick(&[0.15, 0.3, 0.5], &[0.03, 0.15, 0.3, 0.5]),
        sweep.of(2, 40),
    );
    let afters = sweep.pick(&[3, 30], &[3, 10, 30]);
    let timing = sweep.of((4.0, 12), (10.0, 25));
    rewinds(&mut card, &cases, &afters, timing, true, Some(1.5));
    card.finish();
}

#[test]
fn should_s1_a_lasting_rewind_under_jitter_beyond_the_threshold_is_one_rewind_and_no_cut() {
    let mut card = Card::new("S1 lasting rewind under white jitter ±0.75 to ±1.5 ms");
    let sweep = card.sweep;
    let cases = grid(
        &sweep.pick(&[512, 1024], &[256, 512, 1024]),
        &[15.0, 30.0],
        &sweep.pick(&[0.75, 1.5], &[0.75, 1.0, 1.5]),
        sweep.of(2, 20),
    );
    let timing = sweep.of((4.0, 12), (10.0, 25));
    rewinds(&mut card, &cases, &[], timing, false, None);
    card.finish();
}

/// How an excursion's host times come home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Home {
    /// In one step.
    AtOnce,
    /// Half way for one buffer, then the rest.
    Half,
    /// Two thirds and one third back for a buffer each, then the rest.
    Thirds,
}

/// The events of an excursion of `buffers` buffers of `frames` from
/// buffer `start` on, off by `by` seconds and coming home as `home` says,
/// its steps home among its buffers; `None` when it has too few buffers
/// for its steps home and one buffer off by all of `by`.
fn excursion(
    start: u64,
    frames: u64,
    buffers: u64,
    by: f64,
    home: Home,
) -> Option<Vec<(f64, Event)>> {
    let at = |n: u64| buffer_at(start + n, frames, AT);
    let steps: &[f64] = match home {
        Home::AtOnce => &[],
        Home::Half => &[0.5],
        Home::Thirds => &[2.0 / 3.0, 1.0 / 3.0],
    };
    let whole = buffers.checked_sub(steps.len() as u64).filter(|&n| n > 0)?;
    let mut events = vec![(at(0), Event::Off(whole, by))];
    for (i, part) in steps.iter().enumerate() {
        events.push((at(whole + i as u64), Event::Off(1, by * part)));
    }
    Some(events)
}

/// A family's grid of glitches.
#[derive(Debug, Clone, PartialEq)]
struct Glitches {
    /// IO buffer frames.
    sizes: Vec<u64>,
    /// How far off, in ms.
    sizes_ms: Vec<f64>,
    /// Which ways: 1 ahead, -1 back.
    ways: Vec<f64>,
    /// How many buffers are off, the steps home among them: drift
    /// correction confirms a step only from its fourth block on.
    lengths: Vec<u64>,
    /// How they come home.
    homes: Vec<Home>,
    /// White jitter's peak, in seconds.
    peak: f64,
    /// Phases, five buffers apart.
    phases: u64,
    /// Glitches of 1 to 3 buffers are judged by M6's criteria: at most one
    /// glitch reported and nothing made up.
    strict: bool,
}

/// Plays `grid`'s glitches into `card`: for any, no lasting offset
/// (0.5 ms); for 1 to 3 buffers off when `strict`, at most one glitch
/// reported and nothing made up, else at most two drift edits.
fn glitches(card: &mut Card, grid: &Glitches) {
    let (at, seconds) = (3.0, 8);
    for &frames in &grid.sizes {
        for phase in 0..grid.phases {
            let device = || jittered(AT, frames, grid.peak, 1);
            let twin = play(device(), seconds);
            let start = buffer_index(at, frames, AT) + 5 * phase;
            for &ms in &grid.sizes_ms {
                for by in grid.ways.iter().map(|way| way * ms) {
                    for &buffers in &grid.lengths {
                        for &home in &grid.homes {
                            let Some(events) = excursion(start, frames, buffers, by / 1000.0, home)
                            else {
                                continue;
                            };
                            let run = play(device().with(&events), seconds);
                            let after = buffer_at(start + buffers, frames, AT) + 2.0;
                            let mut judge = Judge::default();
                            judge.at_most("lasting offset (ms)", run.apart(&twin, after), 0.5);
                            if grid.strict && buffers <= 3 {
                                let notices = Notices::of(&run);
                                judge.require(
                                    "at most one glitch",
                                    notices.len() == notices.glitches.len()
                                        && notices.glitches.len() <= 1,
                                );
                                untouched_by(&mut judge, &run);
                            } else {
                                let view = &run.view;
                                judge.require(
                                    "at most two drift edits",
                                    view.drift_steps + view.drift_resets <= 2,
                                );
                            }
                            let case = format!(
                                "{by:+} ms for {buffers} buffers of {frames}, home {home:?}, \
                                 phase {phase}"
                            );
                            card.add(case, judge, numbers(&run, &twin));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn m6_glitches_ahead_and_back_leave_no_lasting_offset() {
    let mut card = Card::new("M6 glitches of 3 to 30 ms, jitter 0.03 ms");
    let sweep = card.sweep;
    let grid = Glitches {
        sizes: sweep.pick(&[128, 1024], &[128, 256, 512, 1024]),
        sizes_ms: sweep.pick(&[3.0, 30.0], &[3.0, 8.0, 15.0, 30.0]),
        ways: vec![1.0, -1.0],
        lengths: sweep.pick(&[1, 3, 4, 6], &[1, 2, 3, 4, 6, 10]),
        homes: vec![Home::AtOnce, Home::Half, Home::Thirds],
        peak: JITTER,
        phases: sweep.of(1, 5),
        strict: true,
    };
    glitches(&mut card, &grid);
    card.finish();
}

#[test]
fn should_s3_a_glitch_back_of_four_buffers_or_more_under_jitter_leaves_no_lasting_offset() {
    let mut card = Card::new("S3 glitch back of 4+ buffers under ±0.5 ms jitter");
    let sweep = card.sweep;
    for peak in sweep.pick(&[0.5], &[0.3, 0.5]) {
        let grid = Glitches {
            sizes: sweep.pick(&[512], &[256, 512, 1024]),
            sizes_ms: sweep.pick(&[15.0], &[3.0, 8.0, 15.0, 30.0]),
            ways: vec![-1.0],
            lengths: sweep.pick(&[4, 10], &[4, 6, 10]),
            homes: sweep.pick(
                &[Home::AtOnce, Home::Half],
                &[Home::AtOnce, Home::Half, Home::Thirds],
            ),
            peak: peak / 1000.0,
            phases: sweep.of(1, 5),
            strict: false,
        };
        glitches(&mut card, &grid);
    }
    card.finish();
}

/// A real loss: an IO cycle the device skipped, or frames its sample
/// clock skipped.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Loss {
    /// This many IO cycles.
    Cycles(u64),
    /// This many ms of frames, to the nearest.
    Ms(f64),
}

#[test]
fn m7_a_real_loss_is_one_gap_made_up_once() {
    let mut card = Card::new("M7 real losses, a cycle to 99.9 ms");
    let sweep = card.sweep;
    let sizes = sweep.pick(&[128, 512, 1024], &[128, 256, 512, 1024]);
    let losses = sweep.pick(
        &[Loss::Cycles(1), Loss::Ms(85.0), Loss::Ms(99.9)],
        &[
            Loss::Cycles(1),
            Loss::Cycles(2),
            Loss::Ms(50.0),
            Loss::Ms(85.0),
            Loss::Ms(95.0),
            Loss::Ms(99.0),
            Loss::Ms(99.9),
        ],
    );
    let starts = sweep.pick(&[0], &[0, FPS]);
    let (at, seconds) = (5.0, 8);
    for frames in sizes {
        // The IO cycles over a video tick, where in it the loss falls.
        let cycles = (f64::from(AT) / FPS as f64 / frames as f64).ceil() as u64;
        let phases = sweep.of(2, cycles.min(8));
        for &output_at in &starts {
            let twin = play_from(Device::new(AT, frames), seconds, output_at);
            for &loss in &losses {
                let (event, lost) = match loss {
                    Loss::Cycles(n) => (Event::Lose(n), n * frames),
                    Loss::Ms(ms) => {
                        let lost = (ms * f64::from(AT) / 1000.0).round() as u64;
                        (Event::Skip(lost), lost)
                    }
                };
                if lost * 1000 >= 100 * u64::from(AT) {
                    continue;
                }
                for phase in 0..phases {
                    let start = buffer_index(at, frames, AT) + phase;
                    let events = [(buffer_at(start, frames, AT), event)];
                    let run = play_from(Device::new(AT, frames).with(&events), seconds, output_at);
                    let notices = Notices::of(&run);
                    let ms = lost as f64 * 1000.0 / f64::from(AT);
                    let mut judge = Judge::default();
                    judge.require(
                        format!("one gap of {ms:.2} ms"),
                        one_of(&notices.gaps, ms, about(JITTER)) && notices.len() == 1,
                    );
                    let (view, pacer) = (&run.view, &run.pacer);
                    judge.require(
                        "made up once",
                        (view.drift_steps, view.drift_resets) == (1, 0)
                            && pacer.silence == pacer.dropped,
                    );
                    let left = lasting_offset(&run, &twin).abs();
                    judge.at_most("lasting offset (ms)", left, 0.5);
                    let case = format!(
                        "{loss:?} at {frames} frames, phase {phase}, output at tick {output_at}"
                    );
                    card.add(case, judge, numbers(&run, &twin));
                }
            }
        }
    }
    card.finish();
}
