//! Cross-PR tests: host times that two readings fit, which the device
//! input tells apart by the blocks after them, through program audio's
//! drift correction to the pacer: a connection's second buffer that
//! glitched, or its first stamped off with a cycle lost right after it; a
//! glitch back that comes home in steps; and host times that jitter by
//! more than the device input's jump threshold.
//!
//! # Theory
//!
//! A connection's second block that jumped from its first opens the
//! device input's first excursion, whose map follows it so that the PTS
//! run on from the first block's either way ([`Stamper`]'s module docs).
//! Back on the first block's timeline, it was the second block's glitch;
//! a new jump says the first block was off, and is kept whole from there,
//! a loss right after it too. A steady excursion back that a jump ahead
//! ends stays open until a block lands on the timeline from before it (a
//! glitch coming home in steps) or the new excursion's window passes (a
//! rewind that lasted). And an excursion back is a rewind only when its
//! blocks stay at one level, which jitter never does. What the drift
//! correction gets is then what it handles on its own: blocks contiguous
//! but for a glitch of a block or two, or a loss after a few contiguous
//! blocks, and PTS that jitter about a map that does not move.
//!
//! # Invariants checked
//!
//! * A glitch of ±3, 8 or 15 ms of the second buffer, at 128, 512 or 1024
//!   frames, the first frame captured before the open (a device already
//!   running) or 30 ms after it: one glitch reported when it was beyond
//!   the tolerance, else nothing; nothing filled or cut; the lead the
//!   twin's within 1 ms.
//! * A first buffer off by ±3 to 30 ms and a cycle lost before the third
//!   to sixth, at 256, 512 or 1024 frames, the first frame captured 30 ms
//!   after the open: one gap, of the loss; one step made up, at the loss's
//!   size within 3 frames, and no break; the lead that of the loss alone
//!   within 0.2 ms (made up from the level of two blocks, the loss alone
//!   can itself be a few frames over).
//! * A glitch back of 15 or 30 ms for three to six buffers that comes home
//!   through half its size, or for four in thirds, at 512 or 1024 frames:
//!   one glitch; nothing filled or cut; the lead the twin's within 1 ms.
//! * White jitter of ±1 or ±1.5 ms on every buffer's host time, at 512 or
//!   1024 frames for three minutes, and of ±1 ms at 512 frames for half an
//!   hour: no rewind or gap reported, no step or break, the correction
//!   within its dead band, nothing for the pacer, and the lead that of
//!   CoreAudio's own jitter within 2 ms (the jitter left in the PTS can
//!   tip the drift estimate past its dead band for a while).

use ac_audio::DriftSettings;

use super::*;

/// A case of a run: what it is, the device's events, and the buffers
/// whose host time is off.
type Case = (String, Vec<(f64, Event)>, u64);

#[test]
fn a_glitch_of_the_second_buffer_is_neither_filled_nor_cut() {
    for frames in [128u64, 512, 1024] {
        for after in [None, Some(0.030)] {
            let device = || match after {
                Some(s) => Device::new(RATE, frames).opened_before(s),
                None => Device::new(RATE, frames),
            };
            let twin = play(device(), 10);
            untouched(&twin, "twin");
            for ms in [3.0, 8.0, 15.0] {
                for by in [ms, -ms] {
                    let second = [(buffers(1, frames), Event::Off(1, by / 1000.0))];
                    let run = play(device().with(&second), 10);
                    let apart = run.apart(&twin, 0.0);
                    let what = format!(
                        "{by} ms at {frames} frames, open {after:?} s before: {apart:.3} ms \
                         apart, {:?}, {:?}, {:?}",
                        run.found, run.view, run.pacer
                    );
                    untouched(&run, &what);
                    assert_eq!(run.edited, 0, "{what}");
                    assert!(apart <= 1.0, "{what}");
                    if ms / 1000.0 > tolerance(frames, RATE) {
                        assert!(
                            matches!(
                                run.found[..],
                                [Discontinuity::Glitch {
                                    seq: 1,
                                    blocks: 1,
                                    ..
                                }]
                            ),
                            "{what}"
                        );
                    } else {
                        assert!(run.found.is_empty(), "{what}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_first_buffer_off_and_a_cycle_lost_right_after_it_are_made_up_once() {
    for frames in [256u64, 512, 1024] {
        let device = || Device::new(RATE, frames).opened_before(0.030);
        for before in 2..=5 {
            let lose = (buffers(before, frames), Event::Lose(1));
            let alone = play(device().with(&[lose]), 10);
            assert_eq!(alone.view.drift_steps, 1, "{:?}", alone.found);
            for ms in [3.0, 8.0, 15.0, 30.0] {
                for by in [ms, -ms] {
                    let first = (0.0, Event::Off(1, by / 1000.0));
                    let run = play(device().with(&[first, lose]), 10);
                    let apart = run.apart(&alone, 2.0);
                    let what = format!(
                        "first buffer {by} ms at {frames} frames, a cycle lost before callback \
                         {before}: {apart:.3} ms apart, {:?}, {:?}, {:?}",
                        run.found, run.view, run.pacer
                    );
                    assert!(
                        matches!(run.found[..], [Discontinuity::Gap { by, .. }]
                            if (by.as_secs_f64() - buffers(1, frames)).abs() <= 2.0 * JITTER),
                        "{what}"
                    );
                    assert_eq!(
                        (run.view.drift_steps, run.view.drift_resets),
                        (1, 0),
                        "{what}"
                    );
                    assert_eq!((run.pacer.silence, run.pacer.dropped), (0, 0), "{what}");
                    // Made up at its size; the loss alone, made up from the
                    // level of two blocks, can itself be a few frames over.
                    assert!((run.edited - frames as i64).abs() <= 3, "{what}");
                    assert!(apart <= 0.2, "{what}");
                }
            }
        }
    }
}

#[test]
fn a_glitch_back_that_comes_home_in_steps_is_neither_filled_nor_cut() {
    for frames in [512u64, 1024] {
        let twin = play(Device::new(RATE, frames), SECONDS);
        let after = |n| EVENT_AT + buffers(n, frames);
        for ms in [15.0, 30.0] {
            let back = -ms / 1000.0;
            // Back for three to six buffers, then half way for one; or
            // back for four, then in thirds.
            let mut cases: Vec<Case> = [3, 4, 6]
                .iter()
                .map(|&n| {
                    let events = vec![
                        (EVENT_AT, Event::Off(n, back)),
                        (after(n), Event::Off(1, back / 2.0)),
                    ];
                    (format!("{n} buffers, then half"), events, n + 1)
                })
                .collect();
            let thirds = vec![
                (EVENT_AT, Event::Off(4, back)),
                (after(4), Event::Off(1, back * 2.0 / 3.0)),
                (after(5), Event::Off(1, back / 3.0)),
            ];
            cases.push(("4 buffers, then in thirds".into(), thirds, 6));
            for (how, events, buffers) in cases {
                let run = play(Device::new(RATE, frames).with(&events), SECONDS);
                let apart = run.apart(&twin, 0.0);
                let what = format!(
                    "{ms} ms back at {frames} frames, {how}: {apart:.3} ms apart, {:?}, {:?}, {:?}",
                    run.found, run.view, run.pacer
                );
                let blocks = buffers * frames.div_ceil(MAX_FRAMES as u64);
                assert!(
                    matches!(run.found[..], [Discontinuity::Glitch { blocks: b, .. }] if b == blocks),
                    "{what}"
                );
                untouched(&run, &what);
                assert!(run.edited.abs() <= 3, "{what}");
                assert!(apart <= 1.0, "{what}");
            }
        }
    }
}

#[test]
fn host_times_jittering_beyond_the_jump_threshold_move_nothing() {
    // Three minutes, and half an hour of the jitter that a 1 ms threshold
    // leaves no margin above.
    for (frames, seconds) in [(512u64, 180u64), (1024, 180), (512, 1800)] {
        let twin = play(Device::new(RATE, frames), seconds);
        let jitters: &[f64] = if seconds > 180 {
            &[1.0e-3]
        } else {
            &[1.0e-3, 1.5e-3]
        };
        for &jitter in jitters {
            let device = Device {
                jitter,
                ..Device::new(RATE, frames)
            };
            let run = play(device, seconds);
            let apart = run.apart(&twin, 0.0);
            let what = format!(
                "±{} ms at {frames} frames for {seconds} s: {apart:.3} ms apart, {:?}, {:?}, {:?}",
                jitter * 1000.0,
                run.found,
                run.view,
                run.pacer
            );
            assert!(
                !run.found
                    .iter()
                    .any(|d| matches!(d, Discontinuity::Rewind { .. } | Discontinuity::Gap { .. })),
                "{what}"
            );
            untouched(&run, &what);
            let dead_band = DriftSettings::default().dead_band_ppm;
            assert!(run.view.correction_ppm.abs() <= dead_band, "{what}");
            // The jitter left in the PTS can tip the drift estimate past its
            // dead band for a while: the resampler's half kernel held back,
            // and a ppm or so for some minutes, far inside the 10 ms the
            // correction lets the timing wander.
            assert!(apart <= 2.0, "{what}");
        }
    }
}
