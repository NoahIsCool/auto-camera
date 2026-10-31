//! Startup histories whose audio loss is not identifiable from the
//! Stamper's inputs. They are reported separately from M8's exact-loss
//! recoveries, never counted as successful reconstruction of lost audio.
//!
//! Let `C` be the first true capture, `T` one IO cycle, `E` the first
//! timestamp's error, and `n` the callback before which one cycle is lost.
//! Ignoring ordinary jitter for the notation, the observed timestamps are
//! `C+E, C+T, ..., C+(n-1)T, C+(n+1)T, ...`. The same observations describe
//! a device starting at `C+T`, with first-buffer noise `E-T` and its next
//! `n-1` buffers stamped one cycle early, then returning to normal. Its
//! samples and hand-off loss counters can be identical too. The tests
//! below construct both histories from the actual jittered fixtures and
//! compare every observable exactly, including the stream-open origin.
//!
//! When `E-T` fits M1's permitted startup noise, the battery cannot demand
//! exact loss recovery for one history and no inserted audio for the
//! other. The conservative policy treats this as a timestamp glitch: no
//! invented gap or drift step, at most the filter's small net edit, and a
//! bounded initial offset with no subsequent lead or PTS walk. This can
//! leave a real loss uncompensated by about one cycle. It does not prove
//! which history occurred and is not an exact-loss success.

use ac_sources::audio_input::device::handoff::Captured;

use super::*;

/// Largest first-buffer error still ordinary noise in M1, in ms.
const STARTUP_NOISE_MS: f64 = 0.5;

/// First-buffer error whose competing histories occur in the full grid.
const FIRST_ERROR_MS: f64 = 3.0;

/// Formats whose cycle is within startup noise of [`FIRST_ERROR_MS`].
const FORMATS: [(u32, u64); 3] = [(44_100, 128), (48_000, 128), (96_000, 256)];

/// Host, capture-latency and cycle timestamps round independently to ns.
const ROUNDING_NS: i128 = 3;

/// True when the first error and a later lost cycle also describe an
/// ordinary first-buffer error followed by a short startup glitch.
pub(super) fn collides(rate: u32, frames: u64, error_ms: f64, loss: Option<u64>) -> bool {
    let cycle_ms = buffer_at(1, frames, rate) * 1000.0;
    error_ms > 0.0
        && (error_ms - cycle_ms).abs() + JITTER * 1000.0 <= STARTUP_NOISE_MS
        && loss.is_some_and(|n| n >= 2 && buffer_at(n - 1, frames, rate) < 0.5)
}

/// Every full-grid exclusion is one of the 24 histories proved below;
/// all 1416 other cases keep M8's original exact oracle.
#[test]
fn the_full_grid_partitions_into_exact_cases_and_proved_collisions() {
    let (mut exact, mut collisions) = (0, 0);
    for rate in [44_100, 48_000, 96_000] {
        for frames in [128, 256, 512, 1024, 2048, 4096] {
            for _ in OPENS {
                for loss in [None, Some(2), Some(3), Some(4), Some(5)] {
                    for ms in [3.0, 8.0, 15.0, 30.0] {
                        for error in [ms, -ms] {
                            if collides(rate, frames, error, loss) {
                                assert_eq!(error, FIRST_ERROR_MS);
                                assert!(FORMATS.contains(&(rate, frames)));
                                assert!(matches!(loss, Some(2..=5)));
                                collisions += 1;
                            } else {
                                exact += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!((exact, collisions), (1416, 24));
}

/// A timestamp in signed nanoseconds; fixtures are only about 100 s long.
fn nanos(at: Duration) -> i128 {
    i128::try_from(at.as_nanos()).expect("fixture timestamp fits i128")
}

/// Exact inverse of [`nanos`] for these positive, short fixture timestamps.
fn timestamp(at: i128) -> Duration {
    Duration::from_nanos(u64::try_from(at).expect("fixture timestamp fits u64"))
}

/// All inputs the Stamper observes in one history, with a fixed source ID.
#[derive(Debug, PartialEq)]
struct Observed {
    /// Host time the stream opened; the PTS origin supplied to the Stamper.
    origin: Duration,
    /// Nominal sample rate supplied to the Stamper.
    rate: u32,
    /// Stereo in both histories.
    channels: u16,
    /// Every hand-off field, including samples, cut markers and loss counts.
    blocks: Vec<Captured>,
}

/// Generates the actual M8 loss and a no-loss alternative with identical
/// observable inputs. Checks that the alternative needs only bounded
/// startup noise, a one-cycle glitch, and ordinary jitter thereafter.
fn histories(rate: u32, frames: u64, opened: Option<f64>, loss: u64) -> (Observed, Observed) {
    let events = [
        (0.0, Event::Off(1, FIRST_ERROR_MS / 1000.0)),
        (buffer_at(loss, frames, rate), Event::Lose(1)),
    ];
    let mut actual = device(rate, frames, opened, &events);
    let origin = secs(actual.opened);
    let cycle = secs(frames as f64 / (f64::from(rate) * (1.0 + actual.ppm * 1e-6)));
    let latency = secs((frames + EXTRA) as f64 / f64::from(rate));
    let (mut lost_sink, lost_reader) = handoff(2, rate, MAX_FRAMES, 1);
    let (mut glitch_sink, glitch_reader) = handoff(2, rate, MAX_FRAMES, 1);
    // Constant, nonzero samples are valid audio in either physical history.
    let samples = vec![0.25; frames as usize * 2];
    let mut lost = Observed {
        origin,
        rate,
        channels: 2,
        blocks: Vec::new(),
    };
    let mut glitch = Observed {
        origin,
        rate,
        channels: 2,
        blocks: Vec::new(),
    };
    for callback in 0..64 {
        let (_, at, _, length) = actual.callback();
        assert_eq!(length, frames);
        lost_sink.deliver(at, &samples);
        let captured = lost_reader.take().expect("loss history delivers one block");
        lost.blocks.push(captured.clone());
        lost_reader.recycle(captured);

        // Alternative: the first real capture is one cycle later, but
        // no cycle is ever lost. Its ordinary noise need not follow the
        // fixture RNG's arbitrary seed or frame-index convention.
        let true_capture = secs(actual.host_of((callback + 1) * frames)) - latency;
        let early = if (1..loss).contains(&callback) {
            nanos(cycle)
        } else {
            0
        };
        let noise = nanos(at) - nanos(true_capture) + early;
        let limit = if callback == 0 {
            nanos(secs(STARTUP_NOISE_MS / 1000.0))
        } else {
            nanos(secs(JITTER)) + ROUNDING_NS
        };
        assert!(noise.abs() <= limit, "callback {callback}: {noise} ns");
        let alternative_stamp = timestamp(nanos(true_capture) + noise - early);
        glitch_sink.deliver(alternative_stamp, &samples);
        let captured = glitch_reader
            .take()
            .expect("glitch history delivers one block");
        glitch.blocks.push(captured.clone());
        glitch_reader.recycle(captured);
    }
    (lost, glitch)
}

/// Each competing pair has exactly equal origins and complete Captured
/// records, so the pure Stamper necessarily makes the same decisions.
#[test]
fn startup_loss_and_glitch_histories_have_identical_observable_inputs() {
    for (rate, frames) in FORMATS {
        for opened in OPENS {
            for loss in 2..=5 {
                assert!(collides(rate, frames, FIRST_ERROR_MS, Some(loss)));
                let (lost, glitch) = histories(rate, frames, opened, loss);
                assert_eq!(lost, glitch);
                assert!(
                    lost.blocks
                        .iter()
                        .all(|b| { !b.cut && b.lost_blocks == 0 && b.lost_frames == 0 })
                );
                let make = || Stamper::new(SourceId("board".into()), rate, 2, lost.origin);
                let (mut a, mut b) = (make(), make());
                for (left, right) in lost.blocks.iter().zip(&glitch.blocks) {
                    let (a, notes_a) = a.stamp(left);
                    let (b, notes_b) = b.stamp(right);
                    assert_eq!(notes_a, notes_b);
                    assert_eq!(
                        (a.source, a.seq, a.pts, a.sample_rate, a.channels, a.samples),
                        (b.source, b.seq, b.pts, b.sample_rate, b.channels, b.samples)
                    );
                }
            }
        }
    }
}

/// The 24 collisions use a conservative glitch policy with bounded
/// initial error and no accumulating map or audio offset.
#[test]
fn m8_ambiguous_startups_are_glitches_with_bounded_initial_offset_and_no_walk() {
    let mut card = Card::new("M8 ambiguous startup: glitch policy, not exact loss recovery");
    for (rate, frames) in FORMATS {
        for opened in OPENS {
            for loss in 2..=5 {
                assert!(collides(rate, frames, FIRST_ERROR_MS, Some(loss)));
                let lost = (buffer_at(loss, frames, rate), Event::Lose(1));
                let events = [(0.0, Event::Off(1, FIRST_ERROR_MS / 1000.0)), lost];
                let twin = play(device(rate, frames, opened, &[lost]), 30);
                let run = play(device(rate, frames, opened, &events), 30);
                let mut judge = Judge::default();
                judge.require(
                    "one startup glitch and no inferred missing audio",
                    matches!(&run.found[..], [Discontinuity::Glitch {seq: 1, blocks, by}]
                        if *blocks == loss - 1
                            && (by.as_secs_f64() * 1000.0 - FIRST_ERROR_MS).abs() < 0.1),
                );
                untouched_by(&mut judge, &run);
                let filter_ms = (HALF_TAPS + 1) as f64 * 1000.0 / f64::from(rate);
                let noise_ms = 2.0 * JITTER * 1000.0;
                judge.at_most(
                    "net edit beyond filter/rounding (ms)",
                    net_edit(&run).abs() as f64 * 1000.0 / f64::from(rate),
                    filter_ms + noise_ms,
                );
                judge.at_most(
                    "bounded initial lead offset (ms)",
                    run.apart(&twin, 2.0),
                    FIRST_ERROR_MS + filter_ms + noise_ms,
                );
                no_walk(&mut judge, &run, &twin, 0.5);
                card.add(
                    format!(
                        "{rate} Hz, {frames} frames, {}, loss at {loss}",
                        open_name(opened)
                    ),
                    judge,
                    numbers(&run, &twin),
                );
            }
        }
    }
    assert_eq!(card.verdicts.len(), 24);
    card.finish();
}
