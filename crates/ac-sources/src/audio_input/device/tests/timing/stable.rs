//! Regressions for a stable phase reference, noisy real steps, and callback
//! boundaries. Assertions inspect raw PTS as well as notices, so downstream
//! resampling cannot conceal a permanent movement of the timestamp map.

use super::*;

/// Nanoseconds as a signed integer.
fn ns(value: Duration) -> i128 {
    value.as_nanos() as i128
}

/// A noisy timeline with a lasting rewind and an optional subsequent gap.
/// Returns the notices and the largest late-tail PTS error in nanoseconds.
fn noisy_rewind(
    frames: usize,
    back: u64,
    peak: u64,
    loss_after: Option<usize>,
) -> (Vec<Discontinuity>, i128) {
    let start = Duration::from_secs(100);
    let mut stamp = stamper(start);
    let mut noise = Jitter(0x2545_f491);
    let length = Duration::from_nanos(frames as u64 * 1_000_000_000 / 48_000);
    let (mut found, mut largest) = (Vec::new(), 0i128);
    for i in 0..1000 {
        let lost = loss_after.is_some_and(|n| i >= 300 + n);
        let expected = length * (i as u32 + u32::from(lost));
        let offset = noise.next(Duration::from_micros(peak)) as i128
            - if i >= 300 { i128::from(back) * 1000 } else { 0 };
        let host = Duration::from_nanos((ns(start + expected) + offset) as u64);
        let (block, notices) = stamp.stamp(&captured(host, frames));
        found.extend(notices);
        if i >= 800 {
            largest = largest.max((ns(block.pts) - ns(expected)).abs());
        }
    }
    (found, largest)
}

/// Existing timestamp noise cannot turn a genuine rewind into an audio cut,
/// and an independent loss shortly after it keeps its own gap.
#[test]
fn lasting_rewinds_keep_their_map_under_existing_noise() {
    for frames in [256, 512, 1024] {
        for back in [2500, 15_000, 30_000] {
            for peak in [300, 500] {
                for after in [None, Some(3), Some(30)] {
                    let (found, error) = noisy_rewind(frames, back, peak, after);
                    let case =
                        format!("{frames} frames, {back} us back, ±{peak} us, loss {after:?}");
                    let rewinds = found
                        .iter()
                        .filter(|d| matches!(d, Discontinuity::Rewind { .. }))
                        .count();
                    let gaps = found
                        .iter()
                        .filter(|d| matches!(d, Discontinuity::Gap { .. }))
                        .count();
                    assert_eq!(
                        (rewinds, gaps),
                        (1, usize::from(after.is_some())),
                        "{case}: {found:?}"
                    );
                    assert_eq!(found.len(), rewinds + gaps, "{case}: {found:?}");
                    assert!(error <= 1_500_000, "{case}: {error} ns");
                }
            }
        }
    }
}

/// A late ramp's snap and short noisy bursts return to a stable reference,
/// rather than turning each burst's last late callback into a rewind.
#[test]
fn repeated_ramps_and_bursts_do_not_walk_the_map() {
    for frames in [128usize, 1024] {
        for burst in [false, true] {
            let length = frames as u64 * 1_000_000_000 / 48_000;
            let mut stamp = stamper(Duration::from_secs(100));
            let mut noise = Jitter(0x2545_f491);
            let mut found = Vec::new();
            let (mut first_sum, mut last_sum, mut first_n, mut last_n) =
                (0i128, 0i128, 0i128, 0i128);
            for i in 0..120_000_000_000u64 / length {
                let at = i * length;
                let phase = at % 5_000_000_000;
                let mut offset = noise.next(Duration::from_micros(30)) as i128;
                if at > 2_000_000_000 && phase < 300_000_000 {
                    if burst {
                        offset += i128::from(noise.next(ms(2)));
                    } else {
                        let step = phase / length;
                        if step < 12 {
                            offset += i128::from(step + 1) * 250_000;
                        }
                    }
                }
                let host =
                    Duration::from_nanos((100_000_000_000i128 + i128::from(at) + offset) as u64);
                let (block, notices) = stamp.stamp(&captured(host, frames));
                found.extend(notices);
                let error = ns(block.pts) - i128::from(at);
                if (1_000_000_000..2_000_000_000).contains(&at) {
                    first_sum += error;
                    first_n += 1;
                }
                if at >= 119_000_000_000 {
                    last_sum += error;
                    last_n += 1;
                }
            }
            assert!(
                !found
                    .iter()
                    .any(|d| matches!(d, Discontinuity::Rewind { .. } | Discontinuity::Gap { .. })),
                "{frames}, burst {burst}: {found:?}"
            );
            let walk = last_sum / last_n - first_sum / first_n;
            assert!(walk.abs() < 100_000, "{frames}, burst {burst}: {walk} ns");
        }
    }
}

/// Cut subblocks do not consume the first callback's provisional status.
#[test]
fn a_first_large_callback_error_stays_a_constant() {
    for frames in [2048usize, 4096] {
        for shift in [-30_000_000i128, 30_000_000] {
            let (mut sink, reader) = handoff(2, 48_000, 1024, 64);
            let start = Duration::from_secs(100);
            let mut stamp = stamper(start - ms(50));
            let samples = vec![0.0; frames * 2];
            let mut frame = 0u64;
            for i in 0..30 {
                let at = i * frames as u64 * 1_000_000_000 / 48_000;
                let host = Duration::from_nanos(
                    (ns(start) + i128::from(at) + if i == 0 { shift } else { 0 }) as u64,
                );
                sink.deliver(host, &samples);
                while let Some(c) = reader.take() {
                    let (block, found) = stamp.stamp(&c);
                    reader.recycle(c);
                    assert!(found.is_empty(), "{frames}, {shift}: {found:?}");
                    let expected = 50_000_000 + shift + i128::from(frame * 1_000_000_000 / 48_000);
                    assert!(
                        (ns(block.pts) - expected).abs() <= 10,
                        "{frames}, {shift}: frame {frame}"
                    );
                    frame += block.frames() as u64;
                }
            }
        }
    }
}

/// A callback size change and a known hand-off drop during a noisy rewind
/// preserve both the rewind map and the separately reported overflow.
#[test]
fn overflow_and_callback_changes_do_not_become_an_extra_device_gap() {
    let mut stamp = stamper(Duration::from_secs(100));
    let mut noise = Jitter(0x2545_f491);
    let mut elapsed = 0u64;
    let mut found = Vec::new();
    for i in 0..400 {
        let frames = if i < 110 { 128 } else { 4096 };
        let lost_frames = if i == 115 { 4096 } else { 0 };
        elapsed += lost_frames;
        let jitter = i128::from(noise.next(Duration::from_micros(500)));
        let offset = jitter - if i >= 100 { 15_000_000 } else { 0 };
        for cut in (0..frames).step_by(1024) {
            let at = (elapsed + cut) * 1_000_000_000 / 48_000;
            let host = Duration::from_nanos((100_000_000_000i128 + i128::from(at) + offset) as u64);
            let mut c = captured(host, (frames - cut).min(1024) as usize);
            c.cut = cut > 0;
            if cut == 0 {
                c.lost_blocks = lost_frames / 1024;
                c.lost_frames = lost_frames;
            }
            let (block, notices) = stamp.stamp(&c);
            found.extend(notices);
            if i > 350 {
                assert!((ns(block.pts) - i128::from(at)).abs() < 1_500_000);
            }
        }
        elapsed += frames;
    }
    assert_eq!(
        found
            .iter()
            .filter(|d| matches!(d, Discontinuity::Rewind { .. }))
            .count(),
        1,
        "{found:?}"
    );
    assert_eq!(
        found
            .iter()
            .filter(|d| matches!(d, Discontinuity::Overflow { .. }))
            .count(),
        1,
        "{found:?}"
    );
    assert_eq!(found.len(), 2, "{found:?}");
}

/// Short, confirmed glitch outliers must not enlarge the next real step's
/// noise threshold, even with only a few startup readings in the history.
#[test]
fn a_recent_glitch_cannot_hide_a_lasting_rewind() {
    for (frames, first, count, spike, back) in [
        (128usize, 12, 3, 30, 15),
        (512, 12, 3, 30, 15),
        (1024, 12, 3, 30, 15),
        (512, 3, 4, 30, 15),
        (128, 12, 3, 4, 2),
        (128, 3, 4, 4, 2),
    ] {
        let clean = steady(Duration::from_secs(100), frames, 500);
        let host = moved(&clean, first..first + count, ms(spike), true);
        let host = moved(&host, first + count + 1..500, ms(back), false);
        let (pts, found) = run(&mut stamper(Duration::from_secs(100)), &host);
        assert!(
            matches!(
                found.as_slice(),
                [
                    (_, Discontinuity::Glitch { .. }),
                    (_, Discontinuity::Rewind { .. })
                ]
            ),
            "{frames}: {found:?}"
        );
        assert_eq!(
            pts[499],
            clean[499].0 - Duration::from_secs(100),
            "{frames}"
        );
    }
}
