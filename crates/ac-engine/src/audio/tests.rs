//! Tests of the audio pacer: unit cases, then a simulated source that
//! stalls and bursts, measuring lip sync from the blocks' timestamps.

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
