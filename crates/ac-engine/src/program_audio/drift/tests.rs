//! Tests of drift correction in program audio, through the output's pacer:
//! a board 100 ppm off the engine's clock kept in sync for three hours
//! without silence or cuts, steps in its PTS taken for breaks or made up
//! once (in a connection's first second too) and never taken for drift,
//! timestamps that stray and come back left alone, and audio on the
//! engine's clock (a file input, a camera's) passed on untouched.

use std::sync::Arc;
use std::time::Duration;

use ac_core::SourceId;
use ac_sources::{FakeAudio, FakeFeed};

use super::*;
use crate::audio::{AudioPacer, AudioPacerStats};
use crate::output::AUDIO_MAX_LEAD;
use crate::program_audio::tests::{camera, scripted};
use crate::program_audio::{AudioInputView, Origin, ProgramAudio};

/// The board's sample rate: low, so that three hours of it simulate in
/// about a second; the resampler holds back 3 ms at it.
const RATE: u32 = 8_000;

/// Frames per block (10 ms).
const BLOCK: u64 = 80;

/// Video frames per second.
const FPS: u64 = 30;

/// The spec's bound on A/V sync over a show (C.3), in ms.
const SYNC_MS: i64 = 20;

/// A fixed sequence of numbers in `0.0..1.0`.
struct Noise(u32);

impl Noise {
    /// The next number.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        f64::from(self.0) / (f64::from(u32::MAX) + 1.0)
    }
}

/// A soundboard on its own clock, as the test plays it: stereo blocks of
/// [`BLOCK`] frames at [`RATE`] from a clock `ppm` fast
/// ([`FakeFeed::time_of`]), each stamped with the master-clock time of its
/// first frame give or take `stamp` (a driver's timestamps) and delivered
/// when it is complete plus up to `arrival` (its scheduling), in order.
/// Mostly silence, with a tone each second, so that hours of it resample
/// quickly.
struct Board {
    /// The board's clock.
    clock: FakeFeed,
    /// The next block.
    next: u64,
    /// Blocks completed in these windows never come: a reconnection, lost
    /// audio.
    lost: Vec<(Duration, Duration)>,
    /// Blocks completed in this window come at its end: a stall.
    held: Option<(Duration, Duration)>,
    /// The PTS of the blocks in this range of block numbers are moved by
    /// this many seconds (below zero earlier): timestamps that stray.
    stray: Option<(std::ops::Range<u64>, f64)>,
    /// Most a block is delivered after it is complete.
    arrival: Duration,
    /// Most a block's PTS is off its first frame's time, either way.
    stamp: Duration,
    /// Draws the jitter.
    noise: Noise,
    /// The next block and when it is delivered, once drawn.
    pending: Option<(AudioBlock, Duration)>,
    /// Delivery time of the latest block.
    latest: Duration,
    /// A block of silence.
    silence: Arc<[f32]>,
    /// A block of tone.
    tone: Arc<[f32]>,
}

impl Board {
    /// A board `ppm` fast with 3 ms of arrival jitter and half a
    /// millisecond of PTS jitter.
    fn new(ppm: f64) -> Self {
        let plan = FakeAudio {
            drift_ppm: ppm,
            block_frames: BLOCK as usize,
            ..FakeAudio::steady(RATE, 2)
        };
        let tone = (0..BLOCK)
            .flat_map(|n| {
                let v = (n as f32 * 0.3).sin() * 0.5;
                [v, -v]
            })
            .collect();
        Self {
            clock: FakeFeed::new(SourceId("board".into()), plan).unwrap(),
            next: 0,
            lost: Vec::new(),
            held: None,
            stray: None,
            arrival: Duration::from_millis(3),
            stamp: Duration::from_micros(500),
            noise: Noise(0x2545_f491),
            pending: None,
            latest: Duration::ZERO,
            silence: vec![0.0; 2 * BLOCK as usize].into(),
            tone,
        }
    }

    /// True when `samples` are one of the board's own blocks, shared, not
    /// a copy.
    fn shares(&self, samples: &Arc<[f32]>) -> bool {
        Arc::ptr_eq(samples, &self.silence) || Arc::ptr_eq(samples, &self.tone)
    }

    /// The next block that is not lost, and when it is delivered.
    fn draw(&mut self) -> (AudioBlock, Duration) {
        let within = |w: Option<(Duration, Duration)>, t| w.is_some_and(|(a, b)| a <= t && t < b);
        loop {
            let k = self.next;
            self.next += 1;
            let complete = self.clock.time_of((k + 1) * BLOCK);
            if self.lost.iter().any(|&w| within(Some(w), complete)) {
                continue;
            }
            let ready = match self.held {
                Some((_, until)) if within(self.held, complete) => until,
                _ => complete,
            };
            let late = self.arrival.mul_f64(self.noise.next());
            let at = (ready + late).max(self.latest);
            let first = self.clock.time_of(k * BLOCK);
            let off = self.stamp.mul_f64(self.noise.next() * 2.0);
            let mut pts = (first + off).saturating_sub(self.stamp);
            if let Some((blocks, by)) = &self.stray
                && blocks.contains(&k)
            {
                pts = Duration::from_secs_f64(pts.as_secs_f64() + by);
            }
            let samples = if k.is_multiple_of(100) {
                self.tone.clone()
            } else {
                self.silence.clone()
            };
            let block = AudioBlock {
                source: SourceId("board".into()),
                seq: k,
                pts,
                sample_rate: RATE,
                channels: 2,
                samples,
            };
            return (block, at);
        }
    }

    /// The blocks delivered by master-clock time `now`, oldest first.
    fn until(&mut self, now: Duration) -> Vec<AudioBlock> {
        let mut out = Vec::new();
        loop {
            let (block, at) = match self.pending.take() {
                Some(next) => next,
                None => self.draw(),
            };
            if at > now {
                self.pending = Some((block, at));
                return out;
            }
            self.latest = at;
            out.push(block);
        }
    }
}

/// What a run of a board as program audio reports.
struct Run {
    /// The pacer's lead after each tick from the watched time on, at its
    /// lowest and highest, in ms.
    low: i64,
    /// See `low`.
    high: i64,
    /// Program blocks that came out resampled.
    resampled: u64,
    /// The pacer's counters at the end.
    pacer: AudioPacerStats,
    /// The board as the operator sees it at the end.
    view: AudioInputView,
}

/// Plays `board` as program audio for `seconds` of video through the
/// output's pacer, watching the lead from `watch` seconds on.
fn play(board: &mut Board, seconds: u64, watch: u64) -> Run {
    let (slot, handles) = scripted("board", Some((RATE, 2)), None);
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    let mut pacer = AudioPacer::new(RATE, 2, FPS as u32, AUDIO_SLACK, AUDIO_MAX_LEAD);
    let (mut low, mut high, mut resampled) = (i64::MAX, i64::MIN, 0);
    for tick in 1..=seconds * FPS {
        let now = Duration::from_nanos(tick * 1_000_000_000 / FPS);
        handles.queue.push(board.until(now));
        let blocks = audio.take(&[]);
        resampled += blocks.iter().filter(|b| !board.shares(&b.samples)).count() as u64;
        pacer.plan(tick, blocks);
        if tick >= watch * FPS {
            let lead = pacer.stats().lead_ms;
            (low, high) = (low.min(lead), high.max(lead));
        }
    }
    Run {
        low,
        high,
        resampled,
        pacer: pacer.stats(),
        view: audio.views().remove(0),
    }
}

/// Three hours of a board `ppm` fast: in sync throughout, with no silence
/// and no cuts, and the correction cancelling the drift.
fn three_hours(ppm: f64) {
    // Uncorrected, 100 ppm is 1.08 s over the show, which the pacer would
    // take up by inserting silence or cutting blocks, the lead wandering
    // as far as its 100 ms slack.
    let run = play(&mut Board::new(ppm), 3 * 3_600, 0);
    let (pacer, view) = (&run.pacer, &run.view);
    assert_eq!((pacer.silence, pacer.dropped), (0, 0), "{ppm}: {pacer:?}");
    assert!(
        -SYNC_MS <= run.low && run.high <= SYNC_MS,
        "{ppm}: lead from {} to {} ms",
        run.low,
        run.high
    );
    let cancels = -ppm / (1.0 + ppm * 1e-6);
    assert!(
        (view.correction_ppm - cancels).abs() < 1.0,
        "{ppm}: {view:?}"
    );
    assert_eq!((view.drift_resets, view.drift_steps), (0, 0));
    assert!(run.resampled > 0);
}

#[test]
fn a_board_100_ppm_fast_stays_in_sync_for_three_hours() {
    three_hours(100.0);
}

#[test]
fn a_board_100_ppm_slow_stays_in_sync_for_three_hours() {
    three_hours(-100.0);
}

#[test]
fn a_step_in_the_pts_is_a_break_not_drift() {
    // A board on the engine's clock loses half a second at 30 s: one
    // reset, no correction, every block passed on as it came, and the
    // pacer fills the gap and is back in step.
    let mut board = Board::new(0.0);
    board.lost = vec![(Duration::from_secs(30), Duration::from_millis(30_500))];
    let run = play(&mut board, 60, 31);
    assert_eq!(run.view.drift_resets, 1);
    assert_eq!((run.view.correction_ppm, run.resampled), (0.0, 0));
    assert!(
        run.pacer.silence > 0 && run.pacer.dropped == 0,
        "{:?}",
        run.pacer
    );
    assert!(
        -SYNC_MS <= run.low && run.high <= SYNC_MS,
        "{} to {}",
        run.low,
        run.high
    );
    // A board 100 ppm fast that reconnects two seconds later at five
    // minutes: the correction starts again from zero, and the second epoch
    // is corrected like the first.
    let mut board = Board::new(100.0);
    board.lost = vec![(Duration::from_secs(300), Duration::from_secs(302))];
    let run = play(&mut board, 600, 303);
    assert_eq!(run.view.drift_resets, 1);
    assert!(
        run.pacer.silence > 0 && run.pacer.dropped == 0,
        "{:?}",
        run.pacer
    );
    assert!(
        -SYNC_MS <= run.low && run.high <= SYNC_MS,
        "{} to {}",
        run.low,
        run.high
    );
    assert!(
        (run.view.correction_ppm + 100.0).abs() < 2.0,
        "{:?}",
        run.view
    );
}

#[test]
fn a_stall_whose_audio_comes_late_is_not_a_break() {
    // Half a second held up and then delivered at once: its PTS run on, so
    // nothing is reset or resampled; the pacer fills and cuts as ever.
    let mut board = Board::new(0.0);
    board.held = Some((Duration::from_secs(20), Duration::from_millis(20_500)));
    let run = play(&mut board, 40, 21);
    assert_eq!((run.view.drift_resets, run.resampled), (0, 0));
    assert!(
        run.pacer.silence > 0 && run.pacer.dropped > 0,
        "{:?}",
        run.pacer
    );
    assert!(
        -SYNC_MS <= run.low && run.high <= SYNC_MS,
        "{} to {}",
        run.low,
        run.high
    );
}

#[test]
fn a_file_input_on_the_engines_clock_passes_through_untouched() {
    // Twenty minutes at 48 kHz stamped as a file input stamps them, by its
    // sample count rounded down to the nanosecond: every block reaches the
    // pacer as the very block, and the pacer has nothing to do.
    let (slot, handles) = scripted("board", Some((48_000, 2)), None);
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    let mut pacer = AudioPacer::new(48_000, 2, FPS as u32, AUDIO_SLACK, AUDIO_MAX_LEAD);
    let samples: Arc<[f32]> = (0..960).map(|i| (i as f32 * 0.01).sin()).collect();
    let mut k = 0;
    for tick in 1..=20 * 60 * FPS {
        let mut queued = Vec::new();
        while (k + 1) * 480 * FPS <= tick * 48_000 {
            queued.push(AudioBlock {
                source: SourceId("board".into()),
                seq: k,
                pts: Duration::from_nanos(k * 480 * 1_000_000_000 / 48_000),
                sample_rate: 48_000,
                channels: 2,
                samples: samples.clone(),
            });
            k += 1;
        }
        handles.queue.push(queued.clone());
        let taken = audio.take(&[]);
        assert_eq!(taken.len(), queued.len());
        for (got, sent) in taken.iter().zip(&queued) {
            assert!(
                Arc::ptr_eq(&got.samples, &sent.samples),
                "block {}",
                sent.seq
            );
            assert_eq!((got.seq, got.pts), (sent.seq, sent.pts));
        }
        pacer.plan(tick, taken);
    }
    let view = &audio.views()[0];
    assert_eq!((view.correction_ppm, view.drift_resets), (0.0, 0));
    let stats = pacer.stats();
    assert_eq!((stats.silence, stats.dropped), (0, 0), "{stats:?}");
}

#[test]
fn a_cameras_audio_is_never_resampled() {
    // A camera's embedded audio as program audio, its PTS running 300 ppm
    // off: taken as it is, block for block. A board beside it that is not
    // program audio is drained but not measured.
    let (cam, queue) = camera("cam", Some((48_000, 2)));
    let sources = vec![cam];
    let (slot, board) = scripted("board", Some((48_000, 2)), None);
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Source(0)), vec![slot]);
    let samples: Arc<[f32]> = vec![0.25; 960].into();
    for k in 0..6_000u64 {
        let block = AudioBlock {
            source: SourceId("cam".into()),
            seq: k,
            pts: Duration::from_secs_f64((k * 480) as f64 / (48_000.0 * 1.0003)),
            sample_rate: 48_000,
            channels: 2,
            samples: samples.clone(),
        };
        queue.push([block.clone()]);
        board.queue.push([AudioBlock {
            source: SourceId("board".into()),
            ..block.clone()
        }]);
        let taken = audio.take(&sources);
        assert_eq!(taken.len(), 1);
        assert!(Arc::ptr_eq(&taken[0].samples, &samples));
        assert_eq!(taken[0].pts, block.pts);
    }
    let view = &audio.views()[0];
    assert_eq!(
        (view.correction_ppm, view.drift_resets, view.blocks),
        (0.0, 0, 6_000)
    );
}

#[test]
fn a_new_format_starts_a_new_measurement() {
    // A board 400 ppm fast is corrected; it reconnects at 44.1 kHz: a
    // reset, and its first block at the new rate passes as it is.
    let mut drift = InputDrift::default();
    let block = |rate: u32, k: u64, ppm: f64| AudioBlock {
        source: SourceId("board".into()),
        seq: k,
        pts: Duration::from_secs_f64((k * 480) as f64 / (f64::from(rate) * (1.0 + ppm * 1e-6))),
        sample_rate: rate,
        channels: 2,
        samples: vec![0.0; 960].into(),
    };
    for k in 0..3_000 {
        drift.correct(block(48_000, k, 400.0));
    }
    assert!(
        drift.correction_ppm() < -100.0,
        "{}",
        drift.correction_ppm()
    );
    let first = block(44_100, 0, 0.0);
    let out = drift.correct(first.clone()).unwrap();
    assert!(Arc::ptr_eq(&out.samples, &first.samples));
    assert_eq!((drift.correction_ppm(), drift.resets()), (0.0, 1));
    // A format no corrector takes is passed on and not measured.
    let odd = AudioBlock {
        sample_rate: 0,
        ..first
    };
    let out = drift.correct(odd.clone()).unwrap();
    assert!(Arc::ptr_eq(&out.samples, &odd.samples));
    assert_eq!(drift.resets(), 2, "the 44.1 kHz epoch ended");
    assert_eq!(settings().discontinuity, AUDIO_SLACK);
}

#[test]
fn audio_lost_up_to_the_slack_is_made_up_once() {
    // A ring that overflowed loses 40, 90 or 100 ms of a board, drift-free
    // or 100 ppm fast. The corrector fills a step that size with silence;
    // close to 100 ms the pacer's slack can run out first, and it fills the
    // gap too (then cuts the corrector's silence as late audio), or the
    // step is a break, the pacer's alone. Either way the gap is made up
    // once: the lead is back within the spec's bound at once, and the
    // drift's correction is what it was.
    for ppm in [0.0, 100.0] {
        for lost in [40, 90, 100] {
            let mut board = Board::new(ppm);
            let from = Duration::from_secs(30);
            board.lost = vec![(from, from + Duration::from_millis(lost))];
            let run = play(&mut board, 240, 31);
            let what = format!("{ppm} ppm, {lost} ms lost: {:?}, {:?}", run.pacer, run.view);
            assert!(
                -SYNC_MS <= run.low && run.high <= SYNC_MS,
                "{what}: lead from {} to {} ms",
                run.low,
                run.high
            );
            let cancels = -ppm / (1.0 + ppm * 1e-6);
            assert!((run.view.correction_ppm - cancels).abs() < 1.0, "{what}");
            // A step made up once, or a break the pacer filled.
            let (steps, resets) = (run.view.drift_steps, run.view.drift_resets);
            assert_eq!(steps + resets, 1, "{what}");
            assert!(lost == 100 || steps == 1, "{what}");
        }
    }
}

#[test]
fn audio_lost_in_a_connections_first_second_is_made_up_once() {
    // A board, drift-free or 37 ppm fast, loses 60 ms 0.4 s after it
    // connects, before its drift measurement has its timing reference:
    // the corrector makes the gap up once, and the lead stays within the
    // spec's bound for five minutes, the reference taken net of the step
    // so the silence is never taken back.
    for ppm in [0.0, 37.0] {
        let mut board = Board::new(ppm);
        board.lost = vec![(Duration::from_millis(400), Duration::from_millis(460))];
        let run = play(&mut board, 300, 1);
        let what = format!("{ppm} ppm: {:?}, {:?}", run.pacer, run.view);
        assert!(
            -SYNC_MS <= run.low && run.high <= SYNC_MS,
            "{what}: lead from {} to {} ms",
            run.low,
            run.high
        );
        assert_eq!(
            (run.view.drift_steps, run.view.drift_resets),
            (1, 0),
            "{what}"
        );
        assert_eq!((run.pacer.silence, run.pacer.dropped), (0, 0), "{what}");
        let cancels = -ppm / (1.0 + ppm * 1e-6);
        assert!((run.view.correction_ppm - cancels).abs() < 1.0, "{what}");
    }
}

#[test]
fn a_loss_half_a_second_after_a_break_is_made_up_once() {
    // 150 ms lost at a minute, a break the pacer fills, and another 40 ms
    // half a second later, in the new epoch's first second: the corrector
    // makes that one up, and the lead is back within the spec's bound.
    let mut board = Board::new(37.0);
    board.lost = vec![
        (Duration::from_secs(60), Duration::from_millis(60_150)),
        (Duration::from_millis(60_650), Duration::from_millis(60_690)),
    ];
    let run = play(&mut board, 300, 62);
    let what = format!("{:?}, {:?}", run.pacer, run.view);
    assert!(
        -SYNC_MS <= run.low && run.high <= SYNC_MS,
        "{what}: lead from {} to {} ms",
        run.low,
        run.high
    );
    assert_eq!(
        (run.view.drift_steps, run.view.drift_resets),
        (1, 1),
        "{what}"
    );
    assert!(run.pacer.silence > 0, "{what}");
}

#[test]
fn a_stray_timestamp_leaves_the_boards_audio_alone() {
    // One block stamped 3 ms late, one 30 ms early, three 8 ms late: a
    // drift-free board's blocks all reach the pacer as the very blocks,
    // nothing is made up, and the pacer has nothing to do; a board 100 ppm
    // fast is corrected as ever, in sync, with nothing made up either.
    for (blocks, by) in [
        (3_000..3_001, 0.003),
        (3_000..3_001, -0.03),
        (3_000..3_003, 0.008),
    ] {
        for ppm in [0.0, 100.0] {
            let mut board = Board::new(ppm);
            board.stray = Some((blocks.clone(), by));
            let run = play(&mut board, 120, 0);
            let what = format!(
                "{by} s at {blocks:?}, {ppm} ppm: {:?}, {:?}",
                run.pacer, run.view
            );
            assert_eq!(
                (run.view.drift_steps, run.view.drift_resets),
                (0, 0),
                "{what}"
            );
            assert_eq!((run.pacer.silence, run.pacer.dropped), (0, 0), "{what}");
            assert!(-SYNC_MS <= run.low && run.high <= SYNC_MS, "{what}");
            if ppm == 0.0 {
                assert_eq!(run.resampled, 0, "{what}");
            }
        }
    }
}
