//! The stamper where two readings of the timestamps fit and the blocks
//! after them tell which was right: a connection's second block that
//! glitched or a first block stamped off (and a loss right after it); a
//! glitch back that returns in steps or a rewind that lasted; jitter beyond
//! the jump threshold or a step back (the blocks of one buffer cut into
//! several sharing its host time's error); and an excursion back still
//! open when its window passes, scattered or a rewind just begun.

use super::*;

/// Blocks as [`run`] takes them: each one's host time and frames.
type Blocks = Vec<(Duration, usize)>;

/// The length of a block of `frames` frames at 48 kHz, rounded down to
/// the nanosecond as the stamper rounds it.
fn length(frames: usize) -> Duration {
    Duration::from_nanos(frames as u64 * 1_000_000_000 / 48_000)
}

/// The tolerance a jump after a block of `frames` frames is reported by.
fn tolerance(frames: usize) -> Duration {
    (length(frames) / 2).max(ms(1))
}

/// `d` in nanoseconds, signed.
fn ns(d: Duration) -> i128 {
    d.as_nanos() as i128
}

/// Where the clean run's block `i` would be with every PTS `by`
/// nanoseconds later, in nanoseconds.
fn shifted(want: &[Duration], by: i128) -> Vec<i128> {
    want.iter().map(|&p| ns(p) + by).collect()
}

#[test]
fn a_second_block_that_glitched_is_a_glitch_and_its_pts_run_on_from_the_first() {
    let start = Duration::from_secs(1000);
    for frames in [128usize, 512, 1024] {
        let clean = steady(start, frames, 200);
        // The open after the first block's capture (a device already
        // running: the origin moves back to it), or 30 ms before it.
        for open in [start + ms(5), start - ms(30)] {
            let (want, none) = run(&mut stamper(open), &clean);
            assert!(none.is_empty(), "{none:?}");
            for by in [3, 8, 15] {
                for ahead in [true, false] {
                    let case = format!("{frames} frames, open {open:?}, {by} ms ahead={ahead}");
                    let host = moved(&clean, 1..2, ms(by), ahead);
                    let (pts, found) = run(&mut stamper(open), &host);
                    if ms(by) > tolerance(frames) {
                        let glitch = Discontinuity::Glitch {
                            seq: 1,
                            blocks: 1,
                            by: ms(by),
                        };
                        assert_eq!(found, [(2, glitch)], "{case}");
                    } else {
                        assert!(found.is_empty(), "{case}: {found:?}");
                    }
                    assert_eq!(pts, want, "{case}: every block where it belongs");
                }
            }
        }
    }
}

#[test]
fn a_first_block_off_and_a_loss_right_after_it_keep_the_loss_whole_and_the_pts_contiguous() {
    // The first frame captured 30 ms after the open began, the first block
    // stamped off, and a block lost before the third to sixth block.
    let start = Duration::from_secs(1000);
    let open = start - ms(30);
    for frames in [256usize, 512, 1024] {
        let clean = steady(start, frames, 300);
        let (want, _) = run(&mut stamper(open), &clean);
        let lost = length(frames);
        for by in [3, 8, 15, 30] {
            for ahead in [true, false] {
                for at in 2..=5 {
                    let case =
                        format!("{frames} frames, first {by} ms ahead={ahead}, loss at {at}");
                    let host = moved(&moved(&clean, at..300, lost, true), 0..1, ms(by), ahead);
                    let (pts, found) = run(&mut stamper(open), &host);
                    assert!(
                        matches!(found[..], [(_, Discontinuity::Gap { seq, by })]
                            if seq == at as u64 && by == lost),
                        "{case}: the loss alone, whole: {found:?}"
                    );
                    // The first block's error a constant in every PTS, and
                    // the loss a step of its own.
                    let error = ns(pts[0]) - ns(want[0]);
                    let pts: Vec<i128> = pts.iter().map(|&p| ns(p)).collect();
                    assert_eq!(pts[..at], shifted(&want[..at], error), "{case}");
                    assert_eq!(pts[at..], shifted(&want[at..], error + ns(lost)), "{case}");
                }
            }
        }
    }
}

#[test]
fn a_glitch_back_that_returns_in_steps_is_one_glitch() {
    let start = Duration::from_secs(1000);
    let at = 100;
    for frames in [512usize, 1024] {
        let clean = steady(start, frames, 400);
        let (want, _) = run(&mut stamper(start), &clean);
        for back in [15u64, 30] {
            // Back for three to six blocks, then half way for one, then
            // where it was.
            let mut cases: Vec<(String, Blocks, usize, usize)> = [3usize, 4, 6]
                .iter()
                .map(|&n| {
                    let host = moved(&clean, at..at + n, ms(back), false);
                    let host = moved(&host, at + n..at + n + 1, ms(back) / 2, false);
                    (format!("{n} blocks, then half"), host, n, n + 1)
                })
                .collect();
            // Or in three steps of a third.
            let third = ms(back) / 3;
            let mut ramp = moved(&clean, at..at + 4, ms(back), false);
            ramp = moved(&ramp, at + 4..at + 5, third * 2, false);
            ramp = moved(&ramp, at + 5..at + 6, third, false);
            cases.push(("in thirds".into(), ramp, 4, 6));
            for (what, host, all_back, off) in cases {
                let case = format!("{frames} frames, {back} ms back, {what}");
                let (pts, found) = run(&mut stamper(start), &host);
                let glitch = Discontinuity::Glitch {
                    seq: at as u64,
                    blocks: off as u64,
                    by: ms(back),
                };
                assert_eq!(found, [(at + off, glitch)], "{case}");
                assert_eq!(
                    pts[..at + all_back],
                    want[..at + all_back],
                    "{case}: back on time"
                );
                let tail = at + off + (ms(back).as_nanos() / length(frames).as_nanos()) as usize;
                assert_eq!(pts[tail..], want[tail..], "{case}: no lasting offset");
            }
        }
    }
}

/// `seconds` of a 48 kHz stereo device in `frames`-frame callbacks (cut
/// into blocks of at most 1024 frames by the hand-off), each callback's
/// host time off by up to `jitter` either way, through the hand-off and
/// the stamper (whose origin is the first frame's time). Returns, for each
/// block, the time its first frame was captured at and its PTS less that
/// time, in nanoseconds, and the discontinuities.
fn jittery(frames: u64, jitter: Duration, seconds: u64) -> (Vec<(i128, i128)>, Vec<Discontinuity>) {
    let (mut sink, reader) = handoff(2, 48_000, 1024, 64);
    let host0 = Duration::from_secs(5000);
    let mut s = stamper(host0);
    let mut noise = Jitter(0x9e37_79b9);
    let samples = vec![0.0f32; frames as usize * 2];
    let (mut late, mut found) = (Vec::new(), Vec::new());
    for k in 0..seconds * 48_000 / frames {
        let at = k * frames * 1_000_000_000 / 48_000;
        let host = (at as i64 + noise.next(jitter)).max(0) as u64;
        sink.deliver(host0 + Duration::from_nanos(host), &samples);
        let mut frame = 0;
        while let Some(c) = reader.take() {
            let (block, with) = s.stamp(&c);
            reader.recycle(c);
            let captured = (at + frame * 1_000_000_000 / 48_000) as i128;
            late.push((captured, ns(block.pts) - captured));
            frame += block.frames() as u64;
            found.extend(with);
        }
    }
    (late, found)
}

/// The mean of `late`'s second values over the blocks captured in `from`
/// to `to` seconds, in nanoseconds.
fn mean_late(late: &[(i128, i128)], from: i128, to: i128) -> i128 {
    let second = 1_000_000_000;
    let within: Vec<i128> = late
        .iter()
        .filter(|(at, _)| (from * second..to * second).contains(at))
        .map(|&(_, l)| l)
        .collect();
    within.iter().sum::<i128>() / within.len().max(1) as i128
}

#[test]
fn jitter_beyond_the_jump_threshold_is_no_rewind_and_does_not_move_the_map() {
    // White jitter on every callback's host time, from just beyond the
    // millisecond a jump needs to twice it: the blocks jump either way all
    // the time, and every excursion back they open is scatter.
    let cases = [
        (512u64, 1250u64),
        (512, 1500),
        (512, 2000),
        (1024, 1500),
        (1024, 2000),
        (4096, 1500),
    ];
    for (frames, us) in cases {
        let jitter = Duration::from_micros(us);
        let (late, found) = jittery(frames, jitter, 120);
        let case = format!("{frames} frames, {us} us");
        assert!(
            !found
                .iter()
                .any(|d| matches!(d, Discontinuity::Rewind { .. } | Discontinuity::Gap { .. })),
            "{case}: {found:?}"
        );
        // The PTS as far from the capture times in the last second as in
        // the second one: the map never crept.
        let crept = mean_late(&late, 119, 120) - mean_late(&late, 1, 2);
        assert!(crept.abs() <= ns(jitter) / 2, "{case}: {crept} ns");
    }
}

#[test]
fn the_blocks_of_one_buffer_cut_into_several_are_one_reading() {
    // 4096-frame callbacks cut into four blocks: one callback stamped 2 ms
    // early, the rest 1.5 ms late for good. Its four blocks back are one
    // reading, too few for a rewind: the jump ahead from them is not taken
    // as the end of one, and the late host times are no jump that lasted
    // beyond half a block either.
    let (mut sink, reader) = handoff(2, 48_000, 1024, 64);
    let host0 = Duration::from_secs(5000);
    let mut s = stamper(host0);
    let samples = vec![0.0f32; 4096 * 2];
    let mut found = Vec::new();
    let mut last = (Duration::ZERO, Duration::ZERO);
    for k in 0..60u64 {
        let at = Duration::from_nanos(k * 4096 * 1_000_000_000 / 48_000);
        let host = match k {
            ..10 => host0 + at,
            10 => host0 + at - ms(2),
            _ => host0 + at + Duration::from_micros(1500),
        };
        sink.deliver(host, &samples);
        while let Some(c) = reader.take() {
            let (block, with) = s.stamp(&c);
            reader.recycle(c);
            found.extend(with);
            last = (block.pts, host - host0);
        }
    }
    assert!(found.is_empty(), "{found:?}");
    // The last block's PTS is its host time on the open's map, the last of
    // the four blocks of its callback.
    let three = Duration::from_nanos(3072 * 1_000_000_000 / 48_000);
    assert_eq!(last.0, last.1 + three, "the map not moved");
}

#[test]
fn an_excursion_back_that_scatters_for_its_window_is_put_back() {
    // 512-frame blocks, back by 2 and 3.5 ms in turn for a second: never at
    // one level, so it is not a rewind, and it went no further than half
    // a block, so it is not reported. Its map is put back after the window.
    let blocks = steady(Duration::ZERO, 512, 400);
    let (want, _) = run(&mut stamper(Duration::ZERO), &blocks);
    let mut host = blocks.clone();
    for (i, b) in host.iter_mut().enumerate().take(194).skip(100) {
        b.0 -= if i % 2 == 0 {
            ms(2)
        } else {
            Duration::from_micros(3500)
        };
    }
    let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(pts[200..], want[200..], "no lasting offset");
}

#[test]
fn a_rewind_that_comes_as_the_window_ends_is_still_a_rewind() {
    // 10 ms ahead at block 10 (a loss), then 14 ms back for good just
    // before its window ends: a net rewind of 4 ms, back for one block
    // when the window passes, decided once three blocks are back at one
    // level, and the map moved for it.
    let blocks = steady(Duration::ZERO, 480, 200);
    let (want, _) = run(&mut stamper(Duration::ZERO), &blocks);
    let mut host = moved(&blocks, 10..200, ms(10), true);
    host = moved(&host, 60..200, ms(14), false);
    let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
    assert_eq!(
        found,
        [(63, Discontinuity::Rewind { seq: 10, by: ms(4) })],
        "once three blocks were back"
    );
    assert_eq!(pts[63..], want[63..], "where the old timeline has them");
}
