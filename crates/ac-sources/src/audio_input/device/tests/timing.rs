//! The stamper on made-up host times: sequence numbers and PTS of a
//! steady, a drifting and a jittery stream; jumps that last (a gap kept in
//! PTS, a rewind that moves the map, however small beyond a millisecond),
//! reported once their window has passed; and excursions that come back
//! (glitches of one block or several, either way, at the blocks' sizes a
//! device delivers), which leave the PTS on the stream's old timeline and
//! are reported once, when they went beyond their tolerance. A loss is
//! kept whole after a first block stamped off and after a rewind that
//! lasted, which is reported when the loss comes. Last, the capture
//! latency held for a connection, so that callbacks of another length do
//! not move the PTS.

use super::*;
use crate::audio_input::device::timing::{GLITCH_WINDOW, REWIND_BLOCKS};

/// A captured block of `frames` stereo frames at `host`, nothing lost
/// before it.
fn captured(host: Duration, frames: usize) -> Captured {
    Captured {
        host_time: host,
        samples: vec![0.0; frames * 2],
        lost_blocks: 0,
        lost_frames: 0,
    }
}

/// A stereo stamper at 48 kHz mapping host time `origin` to PTS 0.
fn stamper(origin: Duration) -> Stamper {
    Stamper::new(SourceId("board".into()), 48_000, 2, origin)
}

/// Stamps `blocks` (host time, frames) in turn; returns every PTS and
/// every discontinuity, with the index of the block it came with.
fn run(
    s: &mut Stamper,
    blocks: &[(Duration, usize)],
) -> (Vec<Duration>, Vec<(usize, Discontinuity)>) {
    let mut pts = Vec::new();
    let mut found = Vec::new();
    for (i, &(host, frames)) in blocks.iter().enumerate() {
        let (block, with) = s.stamp(&captured(host, frames));
        assert!(
            pts.last().is_none_or(|&last| block.pts >= last),
            "PTS went back at block {i}"
        );
        pts.push(block.pts);
        found.extend(with.into_iter().map(|d| (i, d)));
    }
    (pts, found)
}

/// `n` contiguous blocks of `frames` frames from host time `start`, each
/// starting exactly where the stamper expects it (the duration of a block
/// rounded down to the nanosecond, as the stamper rounds it).
fn steady(start: Duration, frames: usize, n: usize) -> Vec<(Duration, usize)> {
    let length = Duration::from_nanos(frames as u64 * 1_000_000_000 / 48_000);
    (0..n)
        .map(|k| (start + length * k as u32, frames))
        .collect()
}

/// `blocks` with the host times of `range` moved by `by` (ahead when
/// `ahead`).
fn moved(
    blocks: &[(Duration, usize)],
    range: std::ops::Range<usize>,
    by: Duration,
    ahead: bool,
) -> Vec<(Duration, usize)> {
    let mut out = blocks.to_vec();
    for b in &mut out[range] {
        b.0 = if ahead { b.0 + by } else { b.0 - by };
    }
    out
}

#[test]
fn contiguous_blocks_get_numbers_and_their_capture_time_as_pts() {
    let mut s = stamper(ms(1000));
    for k in 0..50u64 {
        let (block, found) = s.stamp(&captured(ms(1000 + 10 * k), 480));
        assert!(found.is_empty(), "{found:?}");
        assert_eq!((block.seq, block.pts), (k, ms(10 * k)));
        assert_eq!((block.sample_rate, block.channels), (48_000, 2));
        assert_eq!(block.frames(), 480);
    }
    assert_eq!(s.next_seq(), 50);
}

#[test]
fn a_drifting_device_keeps_its_host_times_and_reports_nothing() {
    // 1000 ppm fast: 480 frames take 479.5 frames' time on the host clock.
    let step = Duration::from_secs_f64(480.0 / 48_048.0);
    let mut s = stamper(Duration::ZERO);
    let mut last = Duration::ZERO;
    for k in 0..1000u32 {
        let (block, found) = s.stamp(&captured(step * k, 480));
        assert!(found.is_empty(), "block {k}: {found:?}");
        assert_eq!(block.pts, step * k, "the host time, not the sample count");
        last = block.pts;
    }
    assert!(
        last < ms(9990),
        "{last:?}: PTS advance slower than the samples"
    );
}

#[test]
fn a_block_captured_before_the_open_starts_pts_at_zero() {
    let mut s = stamper(ms(1000));
    let (first, found) = s.stamp(&captured(ms(993), 480));
    assert!(found.is_empty());
    assert_eq!(first.pts, Duration::ZERO);
    let (second, _) = s.stamp(&captured(ms(1003), 480));
    assert_eq!(second.pts, ms(10), "contiguous from the moved origin");
}

#[test]
fn jitter_within_tolerance_is_not_a_jump_and_pts_never_go_back() {
    let mut s = stamper(Duration::ZERO);
    let (_, found) = s.stamp(&captured(ms(0), 480));
    assert!(found.is_empty());
    // Due at 10 ms; half a block (5 ms) either way is jitter.
    let (late, found) = s.stamp(&captured(Duration::from_micros(14_900), 480));
    assert!(found.is_empty(), "{found:?}");
    let (early, found) = s.stamp(&captured(Duration::from_micros(20_000), 480));
    assert!(found.is_empty(), "{found:?}");
    assert!(early.pts >= late.pts);
    // A tiny block's tolerance is still a millisecond.
    let mut t = stamper(Duration::ZERO);
    t.stamp(&captured(ms(0), 4));
    let (_, found) = t.stamp(&captured(Duration::from_micros(900), 4));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_jump_ahead_that_lasts_is_a_gap_kept_in_pts_reported_after_the_window() {
    let mut s = stamper(Duration::ZERO);
    s.stamp(&captured(ms(0), 480));
    s.stamp(&captured(ms(10), 480));
    // Due at 20 ms, captured at 41.3 ms: 21.3 ms missing, and the stream
    // stays there.
    let jumped = Duration::from_micros(41_300);
    let (block, found) = s.stamp(&captured(jumped, 480));
    assert!(found.is_empty(), "not reported yet: {found:?}");
    assert_eq!(block.seq, 2, "nothing was dropped on the way");
    assert_eq!(block.pts, jumped, "the gap is kept at once");
    // 50 blocks (500 ms) on, the window has not passed; the next one is
    // after it.
    for k in 1..=50u32 {
        let (block, found) = s.stamp(&captured(jumped + ms(10) * k, 480));
        assert!(found.is_empty(), "block {}: {found:?}", k + 2);
        assert_eq!(block.pts, jumped + ms(10) * k);
    }
    let (block, found) = s.stamp(&captured(jumped + ms(510), 480));
    assert_eq!(
        found,
        [Discontinuity::Gap {
            seq: 2,
            by: Duration::from_micros(21_300)
        }],
        "once, at the block that jumped"
    );
    assert_eq!(block.pts, jumped + ms(510));
    assert_eq!(
        found[0].to_string(),
        "21.3 ms of audio missing before block 2 (its host time jumped ahead)"
    );
    let (_, found) = s.stamp(&captured(jumped + ms(520), 480));
    assert!(found.is_empty(), "reported once: {found:?}");
}

#[test]
fn a_rewind_that_lasts_moves_the_map_so_pts_carry_on() {
    let mut s = stamper(Duration::ZERO);
    s.stamp(&captured(ms(1000), 480));
    s.stamp(&captured(ms(1010), 480));
    // Due at 1020 ms, stamped 400 ms: the timestamps went back 620 ms.
    let (block, found) = s.stamp(&captured(ms(400), 480));
    assert!(found.is_empty(), "not reported yet: {found:?}");
    assert_eq!(block.pts, ms(1020), "where it was due");
    let mut last = Vec::new();
    for k in 1..=51u64 {
        let (next, found) = s.stamp(&captured(ms(400 + 10 * k), 480));
        assert_eq!(next.pts, ms(1020 + 10 * k), "the map stays moved");
        last = found;
    }
    let rewind = Discontinuity::Rewind {
        seq: 2,
        by: ms(620),
    };
    assert_eq!(last, std::slice::from_ref(&rewind), "after the window");
    assert_eq!(
        rewind.to_string(),
        "host time went back 620.0 ms at block 2"
    );
}

#[test]
fn dropped_blocks_skip_their_numbers_and_their_time_and_are_reported_at_once() {
    let mut s = stamper(Duration::ZERO);
    s.stamp(&captured(ms(0), 480));
    // Three 10 ms blocks dropped; the next is captured where they ended.
    let after = Captured {
        lost_blocks: 3,
        lost_frames: 1440,
        ..captured(ms(40), 480)
    };
    let (block, found) = s.stamp(&after);
    let overflow = Discontinuity::Overflow {
        seq: 4,
        blocks: 3,
        frames: 1440,
        lasting: ms(30),
    };
    assert_eq!(found, std::slice::from_ref(&overflow), "no gap besides");
    assert_eq!((block.seq, block.pts), (4, ms(40)));
    assert_eq!(
        overflow.to_string(),
        "3 block(s) (1440 sample frames, 30.0 ms) dropped before block 4: \
         the input fell behind its device"
    );
    // Dropped blocks and a jump on top: the overflow now, the gap once its
    // window has passed (the dropped audio counts towards it).
    let worse = Captured {
        lost_blocks: 1,
        lost_frames: 480,
        ..captured(ms(100), 480)
    };
    let (block, found) = s.stamp(&worse);
    assert_eq!(block.seq, 6);
    assert!(
        matches!(found[..], [Discontinuity::Overflow { blocks: 1, .. }]),
        "{found:?}"
    );
    let mut reported = Vec::new();
    for k in 1..=60u64 {
        let (_, found) = s.stamp(&captured(ms(100 + 10 * k), 480));
        reported.extend(found.into_iter().map(|d| (k, d)));
    }
    assert_eq!(
        reported,
        [(51, Discontinuity::Gap { seq: 6, by: ms(40) })],
        "510 ms of audio after the jump"
    );
}

#[test]
fn a_glitch_of_one_block_or_several_either_way_leaves_pts_on_the_old_timeline() {
    for frames in [128usize, 512, 1024] {
        // Half a block is the tolerance: 1.3, 5.3 and 10.7 ms.
        let tolerance = Duration::from_nanos(frames as u64 * 1_000_000_000 / 96_000);
        let clean = steady(Duration::from_secs(1000), frames, 2000);
        let (want, none) = run(&mut stamper(Duration::from_secs(1000)), &clean);
        assert!(none.is_empty(), "{none:?}");
        let length = want[1] - want[0];
        for by in [tolerance + ms(1), ms(30)] {
            for blocks in [1usize, 2, 4] {
                for ahead in [true, false] {
                    let at = 400;
                    let case = format!("{frames} frames, {by:?} ahead={ahead}, {blocks} block(s)");
                    let glitched = moved(&clean, at..at + blocks, by, ahead);
                    let (pts, found) = run(&mut stamper(Duration::from_secs(1000)), &glitched);
                    assert_eq!(
                        found,
                        [(
                            at + blocks,
                            Discontinuity::Glitch {
                                seq: at as u64,
                                blocks: blocks as u64,
                                by
                            }
                        )],
                        "{case}: one glitch, when the stream is back"
                    );
                    for (i, (&got, &clean)) in pts.iter().zip(&want).enumerate() {
                        let off = (at..at + blocks).contains(&i);
                        let expected = if off && ahead {
                            // Ahead, the blocks went out ahead; back, the
                            // map moved them to where they were due.
                            clean + by
                        } else if i >= at + blocks && ahead {
                            // Back on the old timeline, held at the newest
                            // PTS until it catches up.
                            clean.max(want[at + blocks - 1] + by)
                        } else {
                            clean
                        };
                        assert_eq!(got, expected, "{case}: block {i}");
                    }
                    let tail = (by.as_nanos() / length.as_nanos()) as usize + 1;
                    assert_eq!(
                        pts[at + blocks + tail..],
                        want[at + blocks + tail..],
                        "{case}: no lasting offset"
                    );
                }
            }
        }
    }
}

/// xorshift32: the same jitter in every run.
struct Jitter(u32);

impl Jitter {
    /// The next offset, up to `max` either way.
    fn next(&mut self, max: Duration) -> i64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        let unit = f64::from(self.0) / f64::from(u32::MAX) * 2.0 - 1.0;
        (unit * max.as_nanos() as f64) as i64
    }
}

/// Ten seconds of a 48 kHz stereo device in `frames`-frame callbacks, its
/// host times 30 µs off either way, through the hand-off and the stamper;
/// the callbacks from `glitch.0` on (`glitch.1` of them) stamped
/// `glitch.2` nanoseconds off. Returns the PTS and the discontinuities.
fn device(frames: u64, glitch: (u64, u64, i64)) -> (Vec<Duration>, Vec<Discontinuity>) {
    let (mut sink, reader) = handoff(2, 48_000, 1024, 64);
    let host0 = Duration::from_secs(5000);
    let mut s = stamper(host0);
    let mut jitter = Jitter(0x2545_f491);
    let samples = vec![0.25f32; frames as usize * 2];
    let (mut pts, mut found) = (Vec::new(), Vec::new());
    for k in 0..480_000 / frames {
        let true_ns = (k * frames * 1_000_000_000 / 48_000) as i64;
        let mut ns = true_ns + jitter.next(Duration::from_micros(30));
        if (glitch.0..glitch.0 + glitch.1).contains(&k) {
            ns += glitch.2;
        }
        let host = host0 + Duration::from_nanos(ns.max(0) as u64);
        sink.deliver(host, &samples);
        while let Some(c) = reader.take() {
            let (block, with) = s.stamp(&c);
            reader.recycle(c);
            pts.push(block.pts);
            found.extend(with);
        }
    }
    (pts, found)
}

#[test]
fn a_device_whose_host_time_glitches_is_stamped_as_if_it_had_not() {
    for (frames, by_ms) in [(128u64, 3i64), (512, 8), (1024, 15), (512, 30)] {
        let (clean, none) = device(frames, (0, 0, 0));
        assert!(none.is_empty(), "{frames}: {none:?}");
        // Halfway through the ten seconds.
        let at = 240_000 / frames;
        for blocks in [1u64, 4] {
            for sign in [1i64, -1] {
                let by = sign * by_ms * 1_000_000;
                let case = format!("{frames} frames, {by} ns, {blocks} block(s)");
                let (pts, found) = device(frames, (at, blocks, by));
                assert!(
                    matches!(found[..], [Discontinuity::Glitch { seq, blocks: b, .. }]
                        if seq == at && b == blocks),
                    "{case}: {found:?}"
                );
                assert_eq!(pts.len(), clean.len(), "{case}");
                // From well after the glitch on, the very same PTS.
                let after = (at + blocks) as usize + 64;
                assert_eq!(pts[after..], clean[after..], "{case}");
            }
        }
    }
}

#[test]
fn a_jump_back_within_the_window_cancels_a_jump_ahead_up_to_its_size() {
    let blocks = steady(Duration::ZERO, 480, 200);
    let (want, _) = run(&mut stamper(Duration::ZERO), &blocks);
    // 20 ms ahead at block 10, then 30 ms back at block 11 and on: a net
    // rewind of 10 ms, from block 11 on where the old timeline has it.
    let mut host = moved(&blocks, 10..11, ms(20), true);
    host = moved(&host, 11..200, ms(10), false);
    let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
    assert_eq!(pts[10], want[10] + ms(20));
    assert_eq!(pts[11], want[10] + ms(20), "held: PTS never go back");
    assert_eq!(pts[13..], want[13..], "no lasting offset");
    assert_eq!(
        found,
        [(
            61,
            Discontinuity::Rewind {
                seq: 10,
                by: ms(10)
            }
        )],
        "the net jump, once, after the window"
    );
    // 30 ms back, then 10 ms ahead: a net rewind of 20 ms.
    let mut host = moved(&blocks, 10..11, ms(30), false);
    host = moved(&host, 11..200, ms(20), false);
    let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
    assert_eq!(pts, want, "every block where the old timeline has it");
    assert_eq!(
        found,
        [(
            61,
            Discontinuity::Rewind {
                seq: 10,
                by: ms(20)
            }
        )]
    );
    // 10 ms back, then 30 ms ahead: a net gap of 20 ms, kept.
    let mut host = moved(&blocks, 10..11, ms(10), false);
    host = moved(&host, 11..200, ms(20), true);
    let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
    assert_eq!(pts[10], want[10]);
    assert_eq!(
        pts[11..],
        want[11..].iter().map(|&p| p + ms(20)).collect::<Vec<_>>()
    );
    assert_eq!(
        found,
        [(
            61,
            Discontinuity::Gap {
                seq: 10,
                by: ms(20)
            }
        )]
    );
}

#[test]
fn an_excursion_longer_than_the_window_lasts() {
    let blocks = steady(Duration::ZERO, 480, 300);
    let (want, _) = run(&mut stamper(Duration::ZERO), &blocks);
    // 8 ms ahead for 60 blocks (600 ms), then back: a gap, then a rewind,
    // each reported after its own window, and the PTS ahead for good.
    let host = moved(&blocks, 10..70, ms(8), true);
    let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
    assert_eq!(
        found,
        [
            (61, Discontinuity::Gap { seq: 10, by: ms(8) }),
            (121, Discontinuity::Rewind { seq: 70, by: ms(8) }),
        ]
    );
    assert_eq!(pts[299], want[299] + ms(8));
    assert_eq!(GLITCH_WINDOW, ms(500));
}

#[test]
fn a_glitch_describes_itself() {
    let glitch = Discontinuity::Glitch {
        seq: 11_250,
        blocks: 1,
        by: Duration::from_micros(3_020),
    };
    assert_eq!(
        glitch.to_string(),
        "host time of 1 block(s) from block 11250 was up to 3.0 ms off, then came back: \
         no audio missing"
    );
}

#[test]
fn the_first_buffers_latency_is_held_for_the_connection() {
    // cpal's capture time when it could read the buffer size: one latency
    // for every buffer, kept as it is.
    let mut steady = CaptureLatency::default();
    for k in 0..10u64 {
        let host = ms(1000 + 10 * k);
        assert_eq!(steady.capture(host, host - ms(12)), host - ms(12));
    }
    // When it could not, its latency follows each callback's length; the
    // first one's is held.
    let mut held = CaptureLatency::default();
    assert_eq!(held.capture(ms(5000), ms(4989)), ms(4989));
    assert_eq!(held.capture(ms(5010), ms(5007)), ms(4999), "11 ms, not 3");
    // A capture time after the host time is no latency; a host time below
    // the latency comes out as zero.
    let mut none = CaptureLatency::default();
    assert_eq!(none.capture(ms(10), ms(20)), ms(10));
    let mut early = CaptureLatency::default();
    early.capture(ms(50), ms(10));
    assert_eq!(early.capture(ms(30), ms(0)), Duration::ZERO);
}

/// Four seconds of a 48 kHz device whose callbacks are 512 frames, then
/// 128 from the second second on, then 512 again, stamped with cpal's
/// capture time for a device whose buffer size it could not read (the
/// host time less the callback's length and 64 frames more), through
/// `latency` when given; returns the PTS and the discontinuities.
fn cpal_without_buffer_size(
    mut latency: Option<CaptureLatency>,
) -> (Vec<(u64, Duration)>, Vec<Discontinuity>) {
    let (mut sink, reader) = handoff(2, 48_000, 1024, 64);
    let host0 = Duration::from_secs(100);
    let mut s = stamper(host0 - ms(20));
    let nanos = |frames: u64| Duration::from_nanos(frames * 1_000_000_000 / 48_000);
    let (mut frame, mut pts, mut found) = (0u64, Vec::new(), Vec::new());
    while frame < 4 * 48_000 {
        let len = if (48_000..2 * 48_000).contains(&frame) {
            128
        } else {
            512
        };
        let host = host0 + nanos(frame);
        let capture = host - nanos(len + 64);
        let at = match &mut latency {
            Some(l) => l.capture(host, capture),
            None => capture,
        };
        sink.deliver(at, &vec![0.0; len as usize * 2]);
        while let Some(c) = reader.take() {
            let (block, with) = s.stamp(&c);
            reader.recycle(c);
            pts.push((frame, block.pts));
            found.extend(with);
        }
        frame += len;
    }
    (pts, found)
}

#[test]
fn callbacks_of_another_length_do_not_move_the_pts() {
    let (pts, found) = cpal_without_buffer_size(Some(CaptureLatency::default()));
    assert!(found.is_empty(), "{found:?}");
    // Every block's PTS is its first frame's time, from the first one on.
    let (first_frame, first) = pts[0];
    assert_eq!(first_frame, 0);
    for &(frame, at) in &pts {
        let want = first + Duration::from_nanos(frame * 1_000_000_000 / 48_000);
        let err = at.as_nanos().abs_diff(want.as_nanos());
        assert!(err < 1_000, "frame {frame}: {at:?}, not {want:?}");
    }
    // cpal's own capture times would have jumped 8 ms ahead at the shorter
    // callbacks and back at the longer ones.
    let (_, found) = cpal_without_buffer_size(None);
    let eight = |by: &Duration| by.as_nanos().abs_diff(8_000_000) < 1_000;
    assert!(
        matches!(&found[..], [
            Discontinuity::Gap { seq: 94, by: ahead },
            Discontinuity::Rewind { seq: 468, by: back },
        ] if eight(ahead) && eight(back)),
        "{found:?}"
    );
}

#[test]
fn a_first_block_stamped_off_is_not_a_jump_and_a_loss_after_it_is_kept_whole() {
    let blocks = steady(ms(1000), 480, 120);
    let (want, none) = run(&mut stamper(ms(1000)), &blocks);
    assert!(none.is_empty(), "{none:?}");
    // 20 ms lost before block 20: the blocks from there on 20 ms later.
    let lost = moved(&blocks, 20..120, ms(20), true);
    for (first, ahead) in [(ms(8), true), (ms(8), false), (ms(3), false)] {
        let case = format!("first block {first:?} ahead={ahead}");
        let host = moved(&lost, 0..1, first, ahead);
        let (pts, found) = run(&mut stamper(ms(1000)), &host);
        assert_eq!(
            found,
            [(
                71,
                Discontinuity::Gap {
                    seq: 20,
                    by: ms(20)
                }
            )],
            "{case}: the loss, whole, and nothing for the first block"
        );
        assert_eq!(pts[1..20], want[1..20], "{case}: on the open's timeline");
        assert_eq!(
            pts[20..],
            want[20..].iter().map(|&p| p + ms(20)).collect::<Vec<_>>(),
            "{case}: the whole loss in the PTS"
        );
    }
}

#[test]
fn a_rewind_that_lasted_is_reported_before_a_loss_after_it_and_the_loss_kept_whole() {
    let blocks = steady(Duration::ZERO, 480, 200);
    let (want, _) = run(&mut stamper(Duration::ZERO), &blocks);
    // The timestamps go back 10 ms at block 10 and stay there; `after`
    // blocks later 20 ms of audio is lost.
    for after in [REWIND_BLOCKS as usize, 10] {
        let at = 10 + after;
        let mut host = moved(&blocks, 10..200, ms(10), false);
        host = moved(&host, at..200, ms(20), true);
        let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
        assert_eq!(
            found,
            [
                (
                    at,
                    Discontinuity::Rewind {
                        seq: 10,
                        by: ms(10)
                    }
                ),
                (
                    at + 51,
                    Discontinuity::Gap {
                        seq: at as u64,
                        by: ms(20)
                    }
                ),
            ],
            "{after} blocks on: the rewind when the loss comes, the loss after its window"
        );
        assert_eq!(pts[..at], want[..at], "{after}: the rewind absorbed");
        assert_eq!(
            pts[at..],
            want[at..].iter().map(|&p| p + ms(20)).collect::<Vec<_>>(),
            "{after}: the whole loss in the PTS"
        );
    }
}

#[test]
fn a_rewind_smaller_than_half_a_block_that_lasts_moves_the_map_and_is_reported() {
    let blocks = steady(Duration::ZERO, 480, 200);
    let (want, _) = run(&mut stamper(Duration::ZERO), &blocks);
    for by in [ms(2), ms(4)] {
        let host = moved(&blocks, 10..200, by, false);
        let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
        assert_eq!(
            pts, want,
            "{by:?}: every block where the old timeline has it"
        );
        assert_eq!(
            found,
            [(61, Discontinuity::Rewind { seq: 10, by })],
            "{by:?}: once, after the window"
        );
    }
    // Within a millisecond it is jitter: the PTS follow the host times.
    let host = moved(&blocks, 10..200, Duration::from_micros(900), false);
    let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(pts[10], want[10] - Duration::from_micros(900));
}

#[test]
fn an_excursion_within_half_a_block_that_comes_back_is_not_reported() {
    let blocks = steady(Duration::ZERO, 480, 200);
    let (want, _) = run(&mut stamper(Duration::ZERO), &blocks);
    for blocks_off in [1usize, 2, 4] {
        for ahead in [true, false] {
            let case = format!("3 ms ahead={ahead} for {blocks_off} block(s)");
            let host = moved(&blocks, 10..10 + blocks_off, ms(3), ahead);
            let (pts, found) = run(&mut stamper(Duration::ZERO), &host);
            assert!(found.is_empty(), "{case}: {found:?}");
            let back = 10 + blocks_off;
            assert_eq!(pts[back..], want[back..], "{case}: on the old timeline");
            for i in 10..back {
                let off = if ahead { want[i] + ms(3) } else { want[i] };
                assert_eq!(pts[i], off, "{case}: block {i}");
            }
        }
    }
}
