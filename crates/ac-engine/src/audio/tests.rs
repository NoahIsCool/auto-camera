//! Tests of the audio pacer: unit cases (owed silence cut to the frame),
//! then a simulated source that stalls and bursts, measuring lip sync from
//! the blocks' timestamps.

use super::*;
use ac_core::SourceId;

/// Sample rate of the tests.
const RATE: u32 = 48_000;
/// Video frame rate of the tests.
const FPS: u32 = 30;
/// Sample frames per source block (10 ms).
const BLOCK: u64 = 480;

/// A block of `frames` sample frames.
fn block(frames: usize, rate: u32, channels: u16) -> AudioBlock {
    timed(frames, rate, channels, Duration::ZERO)
}

/// A block of `frames` sample frames starting at `pts`.
fn timed(frames: usize, rate: u32, channels: u16, pts: Duration) -> AudioBlock {
    AudioBlock {
        source: SourceId("a".into()),
        seq: 0,
        pts,
        sample_rate: rate,
        channels,
        samples: vec![0.1f32; frames * channels as usize].into(),
    }
}

/// A 48 kHz stereo pacer for 30 fps video with the engine's limits.
fn pacer() -> AudioPacer {
    AudioPacer::new(
        RATE,
        2,
        FPS,
        Duration::from_millis(100),
        Duration::from_millis(100),
    )
}

#[test]
fn steady_audio_passes_through_without_silence() {
    let mut p = pacer();
    for tick in 1..=300u64 {
        // 1600 frames per 30 fps tick, delivered as one block per tick.
        let out = p.plan(tick, vec![block(1600, RATE, 2)]);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], PacedAudio::Block(_)));
    }
    assert_eq!(p.stats().pushed, p.target(300));
    assert_eq!(p.stats().silence, 0);
    assert_eq!(p.stats().lead_ms, 0);
}

#[test]
fn a_stalled_source_is_filled_with_silence_to_the_target() {
    let mut p = pacer();
    // 100 ms of slack: three ticks (100 ms) of nothing is tolerated.
    assert!(p.plan(3, vec![]).is_empty());
    let out = p.plan(4, vec![]);
    assert!(matches!(out.as_slice(), [PacedAudio::Silence(6400)]));
    assert_eq!(p.stats().silence, p.target(4));
}

#[test]
fn silence_for_a_gap_goes_before_the_blocks_that_end_it() {
    let mut p = pacer();
    // Half a second of nothing, then one fresh tick of audio: the gap is in
    // the past, so the silence comes first.
    let out = p.plan(15, vec![block(1600, RATE, 2)]);
    match out.as_slice() {
        [PacedAudio::Silence(gap), PacedAudio::Block(_)] => {
            assert_eq!(*gap, p.target(15) - 1600);
        }
        other => panic!("silence then the block, got {other:?}"),
    }
    assert_eq!(p.stats().lead_ms, 0);
}

#[test]
fn a_burst_beyond_the_lead_limit_keeps_the_newest_audio() {
    let mut p = pacer();
    let burst: Vec<AudioBlock> = (0..100u64)
        .map(|i| timed(480, RATE, 2, Duration::from_millis(10 * i)))
        .collect();
    let out = p.plan(1, burst);
    let s = p.stats();
    // At most target + 100 ms may be pushed, and what is kept is the end
    // of the burst.
    assert!(s.pushed <= p.target(1) + 4_800, "{s:?}");
    assert!(s.dropped > 0);
    assert_eq!(s.pushed + s.dropped, 48_000);
    let PacedAudio::Block(last) = out.last().unwrap() else {
        panic!("blocks only");
    };
    assert_eq!(last.pts, Duration::from_millis(990));
    assert!(s.lead_ms <= 100, "{s:?}");
}

#[test]
fn audio_dropped_at_the_lead_limit_comes_back_when_the_silence_that_lifted_it_goes() {
    // A board 15 or 40 ms ahead of the target. A drift corrector makes up
    // a step in its PTS with 95 ms of silence, which lifts the lead over
    // the limit, and takes it out again over three ticks (the step was a
    // glitch). What the limit dropped is given back as silence on the
    // first tick with audio after that: the lead is where it was.
    for ahead in [720usize, 1920] {
        let mut p = pacer();
        p.plan(1, vec![block(1600 + ahead, RATE, 2)]);
        for tick in 2..10 {
            p.plan(tick, vec![block(1600, RATE, 2)]);
        }
        let before = p.stats().lead_ms;
        p.plan(10, vec![block(1600, RATE, 2), block(4560, RATE, 2)]);
        let dropped = p.stats().dropped;
        assert!(dropped > 0 && p.stats().lead_ms <= 100, "{:?}", p.stats());
        for tick in 11..13 {
            p.plan(tick, vec![block(1600, RATE, 2)]);
        }
        assert!(p.plan(13, vec![]).is_empty());
        assert!(p.plan(14, vec![]).is_empty());
        let out = p.plan(15, vec![block(3 * 1600 - 4560, RATE, 2)]);
        assert!(
            matches!(out[..], [PacedAudio::Silence(n), PacedAudio::Block(_)] if n == dropped),
            "{ahead}: {out:?}"
        );
        for tick in 16..=90 {
            p.plan(tick, vec![block(1600, RATE, 2)]);
        }
        let s = p.stats();
        assert_eq!(s.lead_ms, before, "{ahead}: {s:?}");
        assert_eq!(s.silence, s.dropped, "{ahead}: {s:?}");
    }
}

#[test]
fn a_bursts_credit_is_kept_while_its_audio_holds_the_lead_up_then_forgotten() {
    // 100 ms of backlog beyond the limit at once: a block is dropped, and
    // the source's audio keeps the lead at the limit, so nothing is given
    // back. Two seconds later the source sends nothing for three ticks and
    // goes on fresh, on the target: the credit, which would fit now, is
    // long forgotten, and no silence goes in.
    let mut p = pacer();
    p.plan(1, (0..5).map(|_| block(1600, RATE, 2)).collect());
    assert_eq!((p.stats().dropped, p.stats().lead_ms), (1600, 100));
    for tick in 2..62 {
        p.plan(tick, vec![block(1600, RATE, 2)]);
        assert_eq!(p.stats().silence, 0, "tick {tick}");
    }
    p.plan(65, vec![block(1600, RATE, 2)]);
    for tick in 66..=90 {
        p.plan(tick, vec![block(1600, RATE, 2)]);
    }
    let s = p.stats();
    assert_eq!((s.silence, s.dropped, s.lead_ms), (0, 1600, 0), "{s:?}");
}

#[test]
fn mismatched_and_empty_blocks_are_dropped() {
    let mut p = pacer();
    let out = p.plan(
        1,
        vec![
            block(480, 44_100, 2),
            block(480, RATE, 1),
            block(0, RATE, 2),
        ],
    );
    assert!(out.is_empty());
    assert_eq!(p.stats().mismatched_blocks, 2);
    assert_eq!(p.stats().pushed, 0);
}

/// A block of `frames` frames starting at `pts` whose samples count up
/// from `first` (both channels alike), so a cut shows where it fell.
fn counted(frames: usize, first: usize, pts: Duration) -> AudioBlock {
    AudioBlock {
        samples: (first..first + frames)
            .flat_map(|n| [n as f32; 2])
            .collect::<Vec<_>>()
            .into(),
        seq: first as u64,
        ..timed(frames, RATE, 2, pts)
    }
}

#[test]
fn owed_silence_is_cut_to_the_frame_splitting_a_block() {
    let mut p = pacer();
    // Four ticks of nothing: 6400 frames of silence owed.
    p.plan(4, vec![]);
    assert_eq!(p.stats().silence, 6400);
    // The stall's 150 ms arrive late in three blocks of 2400 frames: the
    // first 6400 frames are cut, ending 1600 frames into the third block,
    // whose rest is pushed with its PTS moved on by them.
    let ms = Duration::from_millis;
    let late = (0..3).map(|k| counted(2400, 2400 * k, ms(50 * k as u64)));
    let out = p.plan(5, late.collect());
    let [PacedAudio::Block(rest)] = out.as_slice() else {
        panic!("one block, got {out:?}");
    };
    assert_eq!(rest.frames(), 800);
    assert_eq!((rest.seq, rest.samples[0]), (4800, 6400.0));
    assert_eq!(rest.pts, ms(100) + Duration::from_nanos(33_333_333));
    let s = p.stats();
    assert_eq!((s.silence, s.dropped, s.pushed), (6400, 6400, 800));
    // Nothing is owed any more: a later block early by more than the
    // tolerance (and less than the lead limit) is not cut.
    p.plan(6, vec![block(4000, RATE, 2)]);
    assert_eq!(p.stats().dropped, 6400);
}

#[test]
fn late_audio_well_short_of_the_debt_is_cut_only_down_to_the_target() {
    // A second of nothing, then a fresh source: aligned to the target, it
    // owes nothing late. A tick of it 1500 frames early (jitter beyond the
    // tolerance) is cut down to the target, not by the second owed.
    let mut p = pacer();
    p.plan(30, vec![]);
    assert_eq!(p.stats().silence, 48_000);
    p.plan(31, vec![block(1600, RATE, 2)]);
    assert_eq!(p.stats().lead_ms, 0);
    let ms = Duration::from_millis;
    let out = p.plan(
        32,
        vec![counted(1550, 0, ms(0)), counted(1550, 1550, ms(32))],
    );
    let s = p.stats();
    assert_eq!((s.dropped, s.lead_ms), (1500, 0), "{s:?}");
    let [PacedAudio::Block(a), PacedAudio::Block(b)] = out.as_slice() else {
        panic!("two blocks, got {out:?}");
    };
    assert_eq!((a.frames(), a.samples[0], b.frames()), (50, 1500.0, 1550));
}

#[test]
fn a_debt_repaid_in_full_is_cut_from_the_next_ticks_audio_too() {
    // 1600 frames, then a gap: at tick 5 the source is more than the slack
    // short, and 5400 frames of silence fill it up to the target ahead of
    // its 1000 frames. Its late 4800 frames at tick 6 cover the debt to
    // within half the slack: they are its backlog, all cut, and the 600
    // frames still owed are cut from the start of the next tick's audio,
    // nothing more.
    let mut p = pacer();
    p.plan(1, vec![block(1600, RATE, 2)]);
    p.plan(4, vec![]);
    let out = p.plan(5, vec![block(1000, RATE, 2)]);
    assert!(matches!(
        out[..],
        [PacedAudio::Silence(5400), PacedAudio::Block(_)]
    ));
    assert!(p.plan(6, vec![block(4800, RATE, 2)]).is_empty());
    let out = p.plan(7, vec![counted(1600, 0, Duration::from_secs(1))]);
    let [PacedAudio::Block(rest)] = out.as_slice() else {
        panic!("one block, got {out:?}");
    };
    assert_eq!((rest.frames(), rest.samples[0]), (1000, 600.0));
    assert_eq!(
        rest.pts,
        Duration::from_millis(1_012) + Duration::from_micros(500)
    );
    let s = p.stats();
    assert_eq!((s.silence, s.dropped), (5400, 5400), "{s:?}");
    p.plan(8, vec![block(1600, RATE, 2)]);
    assert_eq!(p.stats().dropped, 5400);
}

#[test]
fn a_cut_splits_blocks_at_the_frame() {
    let ms = Duration::from_millis;
    let blocks = || vec![counted(480, 0, ms(0)), counted(480, 480, ms(10))];
    let frames = |b: &[AudioBlock]| {
        b.iter()
            .map(|b| (b.frames(), b.samples[0]))
            .collect::<Vec<_>>()
    };
    // Nothing, a whole block, part of the next, everything.
    let mut b = blocks();
    cut_start(&mut b, 0);
    assert_eq!(frames(&b), [(480, 0.0), (480, 480.0)]);
    let mut b = blocks();
    cut_start(&mut b, 480);
    assert_eq!(frames(&b), [(480, 480.0)]);
    assert_eq!(b[0].pts, ms(10));
    let mut b = blocks();
    cut_start(&mut b, 481);
    assert_eq!(frames(&b), [(479, 481.0)]);
    assert_eq!(
        (b[0].pts, b[0].seq),
        (ms(10) + Duration::from_nanos(20_833), 480)
    );
    let mut b = blocks();
    cut_start(&mut b, 960);
    assert!(b.is_empty());
    // A block's PTS that would overflow saturates rather than panics.
    let late = AudioBlock {
        pts: Duration::MAX,
        ..counted(4, 0, ms(0))
    };
    assert_eq!(without_start(&late, 1).pts, Duration::MAX);
}

#[test]
fn target_is_exact_for_long_runs() {
    let p = pacer();
    assert_eq!(p.target(0), 0);
    assert_eq!(p.target(30), 48_000);
    assert_eq!(p.target(30 * 3600 * 24), 48_000 * 3600 * 24);
    let odd = AudioPacer::new(44_100, 2, 30, Duration::ZERO, Duration::ZERO);
    assert_eq!(odd.target(1), 1470);
}

/// A simulated program: a real-time source producing 10 ms blocks, drained
/// once per 30 fps tick, whose lip sync is measured from block timestamps.
#[derive(Debug)]
struct Sim {
    /// The pacer under test.
    pacer: AudioPacer,
    /// Blocks produced so far.
    produced: u64,
    /// Blocks delivered (drained) so far.
    delivered: u64,
    /// Sample frames pushed so far (the encoder's audio position).
    position: u64,
    /// Encoder position minus content time of the first block pushed, in ms.
    first_offset: Option<f64>,
    /// The same for the newest block pushed.
    last_offset: f64,
}

impl Sim {
    /// A fresh simulation with the engine's limits.
    fn new() -> Self {
        Self {
            pacer: pacer(),
            produced: 0,
            delivered: 0,
            position: 0,
            first_offset: None,
            last_offset: 0.0,
        }
    }

    /// Audio late against video (ms) relative to the start of the run.
    fn lag_ms(&self) -> f64 {
        self.last_offset - self.first_offset.unwrap_or(0.0)
    }

    /// Runs tick `tick` (media time `tick / FPS`). The source makes every
    /// block that is due; `hold` keeps them in the source (a stall), and
    /// `limit` delivers at most that many blocks (a catch-up at finite
    /// speed). Blocks the source has dropped are never delivered.
    fn tick(&mut self, tick: u64, hold: bool, limit: Option<u64>) {
        let now_ms = tick * 1000 / u64::from(FPS);
        // Block k holds [10k, 10k + 10) ms and is ready when it is full.
        while (self.produced + 1) * 10 <= now_ms {
            self.produced += 1;
        }
        let mut blocks = Vec::new();
        if !hold {
            let ready = self.produced - self.delivered;
            let n = limit.map_or(ready, |l| ready.min(l));
            for k in self.delivered..self.delivered + n {
                blocks.push(timed(
                    BLOCK as usize,
                    RATE,
                    2,
                    Duration::from_millis(10 * k),
                ));
            }
            self.delivered += n;
        }
        self.push(tick, blocks);
    }

    /// Plans `blocks` at `video_frames` and records the encoder positions.
    fn push(&mut self, video_frames: u64, blocks: Vec<AudioBlock>) {
        for item in self.pacer.plan(video_frames, blocks) {
            match item {
                PacedAudio::Silence(n) => self.position += n,
                PacedAudio::Block(b) => {
                    let offset = self.position as f64 / 48.0 - b.pts.as_secs_f64() * 1e3;
                    self.first_offset.get_or_insert(offset);
                    self.last_offset = offset;
                    self.position += b.frames() as u64;
                }
            }
        }
    }

    /// Lets the source lose its undelivered blocks except the newest `keep`
    /// (its bounded queue overflowed).
    fn overflow(&mut self, keep: u64) {
        self.delivered = self.delivered.max(self.produced.saturating_sub(keep));
    }
}

/// Seconds to ticks.
fn ticks(seconds: f64) -> u64 {
    (seconds * f64::from(FPS)).round() as u64
}

#[test]
fn lip_sync_returns_after_a_source_stall_and_burst() {
    for stall_ms in [150u64, 300, 600, 800, 2000] {
        for catch_up in [None, Some(6)] {
            let mut sim = Sim::new();
            let (start, end) = (ticks(8.0), ticks(8.0) + stall_ms * 30 / 1000);
            for t in 1..ticks(20.0) {
                let hold = (start..end).contains(&t);
                // After the stall the source catches up at once, or at
                // twice real time (6 blocks per 3.3-block tick).
                let limit = if t >= end { catch_up } else { None };
                sim.tick(t, hold, limit);
                if t == start - 1 {
                    assert!(sim.lag_ms().abs() < 15.0, "before: {}", sim.lag_ms());
                }
            }
            let s = sim.pacer.stats();
            assert!(
                sim.lag_ms().abs() < 15.0,
                "stall {stall_ms} ms, catch-up {catch_up:?}: audio {:.0} ms late, {s:?}",
                sim.lag_ms()
            );
            assert!(s.silence > 0, "{s:?}");
            assert!(s.lead_ms.abs() <= 15, "{s:?}");
        }
    }
}

#[test]
fn a_stalls_backlog_repays_exactly_the_silence_owed() {
    // Whatever the stall and however fast its backlog comes, exactly the
    // silence inserted is cut, so the lag is zero to the frame.
    for stall_ms in [150u64, 300, 600, 800, 2000] {
        for catch_up in [None, Some(6)] {
            let mut sim = Sim::new();
            let (start, end) = (ticks(8.0), ticks(8.0) + stall_ms * 30 / 1000);
            for t in 1..ticks(20.0) {
                let limit = if t >= end { catch_up } else { None };
                sim.tick(t, (start..end).contains(&t), limit);
            }
            let s = sim.pacer.stats();
            let what = format!("stall {stall_ms} ms, catch-up {catch_up:?}: {s:?}");
            assert!(s.silence > 0 && s.dropped == s.silence, "{what}");
            assert!(sim.lag_ms().abs() < 1e-9, "{what}: {} ms", sim.lag_ms());
        }
    }
}

#[test]
fn a_short_hiccup_inside_the_slack_changes_nothing() {
    let mut sim = Sim::new();
    for t in 1..ticks(5.0) {
        sim.tick(t, (60..62).contains(&t), None);
    }
    let s = sim.pacer.stats();
    assert_eq!((s.silence, s.dropped), (0, 0), "{s:?}");
    assert!(sim.lag_ms().abs() < 1e-9);
}

#[test]
fn a_source_that_restarts_fresh_is_not_cut() {
    let mut sim = Sim::new();
    for t in 1..ticks(10.0) {
        let gone = (ticks(3.0)..ticks(6.0)).contains(&t);
        if gone {
            // Nothing is produced, and nothing will ever be delivered.
            sim.tick(t, true, None);
            sim.overflow(0);
        } else {
            sim.tick(t, false, None);
        }
    }
    let s = sim.pacer.stats();
    // The fresh audio is in sync and at most one block was cut.
    assert!(sim.lag_ms().abs() < 15.0, "{:.0} ms, {s:?}", sim.lag_ms());
    assert!(s.dropped <= BLOCK, "{s:?}");
}

#[test]
fn skipped_ticks_counted_as_video_keep_audio_in_sync() {
    // The engine freezes for 1.5 s and skips those ticks; the encoder is
    // owed a repeated frame for each, so the frame count still advances.
    let mut sim = Sim::new();
    let (start, end) = (ticks(4.0), ticks(5.5));
    for t in 1..ticks(10.0) {
        if (start..end).contains(&t) {
            continue;
        }
        sim.tick(t, false, None);
    }
    let s = sim.pacer.stats();
    assert_eq!((s.silence, s.dropped), (0, 0), "{s:?}");
    assert!(sim.lag_ms().abs() < 15.0, "{:.0} ms", sim.lag_ms());
}

#[test]
fn a_freeze_longer_than_the_source_queue_is_bridged_in_sync() {
    // A 4 s freeze while the source keeps only the newest 2.56 s of audio:
    // the lost audio is replaced by silence ahead of what survived.
    let mut sim = Sim::new();
    let (start, end) = (ticks(4.0), ticks(8.0));
    for t in 1..ticks(14.0) {
        if (start..end).contains(&t) {
            continue;
        }
        if t == end {
            sim.produced = (t * 1000 / u64::from(FPS)) / 10;
            sim.overflow(256);
        }
        sim.tick(t, false, None);
    }
    let s = sim.pacer.stats();
    assert!(s.silence > 0, "{s:?}");
    assert!(sim.lag_ms().abs() < 15.0, "{:.0} ms, {s:?}", sim.lag_ms());
}

#[test]
fn a_source_running_fast_stays_within_the_lead_limit() {
    // 1% too much audio: the lead creeps up to the limit, then the oldest
    // blocks are trimmed and the newest audio is kept.
    let mut p = pacer();
    let mut lead = 0;
    for tick in 1..=ticks(60.0) {
        p.plan(tick, vec![block(1616, RATE, 2)]);
        lead = p.stats().lead_ms;
    }
    assert!((0..=100).contains(&lead), "{lead}");
    assert!(p.stats().dropped > 0);
    assert_eq!(p.stats().silence, 0);
}
