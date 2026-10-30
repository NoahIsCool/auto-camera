//! Cross-checks of drift correction and the output's pacer for audio lost
//! close to the pacer's slack: a board's blocks with timestamps shaped as
//! a device input stamps them, through [`ProgramAudio`] (its input's
//! drift correction) and the [`AudioPacer`].
//!
//! # Theory
//!
//! A device input keeps a real loss in its PTS at once: the first block
//! after it is stamped its length later than the block before it ended.
//! Drift correction makes the loss up with that much silence only once
//! the blocks after it confirm the step, a few blocks later
//! ([`ac_audio::drift::CONFIRM_BLOCKS`]). Meanwhile the pacer sees the
//! audio at hand short of the video's share by the loss and by what the
//! board's blocks have yet to deliver (up to a block): close to the slack
//! (100 ms) that runs it out of slack first, and it fills the gap up to
//! the target. The corrector's silence then comes after it as late audio,
//! in a block longer than the loss, and the pacer cuts the silence it owes
//! from it. Cut by whole blocks, that cut overshot by up to the block,
//! leaving the board's audio early for good (by up to 35 ms with blocks
//! of 1024 frames); cut to the frame, it is exactly the silence owed, and
//! the gap is made up once.
//!
//! A run is compared with its *twin*, the same board without the loss:
//! each block the pacer pushes has an *offset*, the encoder's position at
//! it less its PTS, which only silence and cuts move (and, at ±100 ppm,
//! the drift correction, alike in both). The run's mean offset over the
//! last second, less the twin's, is what the loss left behind.
//!
//! # Invariants checked
//!
//! * Blocks of 128, 256, 512 and 1024 frames at 48 kHz, 0 and ±100 ppm,
//!   85.3, 96 and 99.9 ms lost, the board's blocks at 34 phases against
//!   the ticks, and the output started while the board was running or as
//!   it opened (its first frame within 5 ms): one step made up, no reset,
//!   any silence the pacer put in cut exactly, and the board's audio
//!   where its twin's is within 0.5 ms.

use std::sync::Arc;
use std::time::Duration;

use ac_core::SourceId;
use ac_media::AudioBlock;

use crate::audio::{AudioPacer, AudioPacerStats, PacedAudio};
use crate::output::{AUDIO_MAX_LEAD, AUDIO_SLACK};
use crate::program_audio::tests::scripted;
use crate::program_audio::{AudioInputView, Origin, ProgramAudio};

/// The board's sample rate.
const RATE: u32 = 48_000;

/// Video frames per second.
const FPS: u64 = 30;

/// Most a block's PTS is off its first frame's time, either way, in
/// seconds: CoreAudio's host-time jitter.
const JITTER: f64 = 30e-6;

/// Most a block is delivered after its last frame, in seconds.
const DELAY: f64 = 150e-6;

/// How long a run plays, in seconds.
const SECONDS: u64 = 7;

/// When the loss starts, in seconds: after the drift correction of a board
/// 100 ppm off has taken hold.
const LOSS_AT: f64 = 5.0;

/// A number in `-1.0..1.0` that depends on `key` alone (SplitMix64), so
/// that a block jitters alike in a run and its twin.
fn spread(key: u64) -> f64 {
    let mut z = key.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 52) as f64 - 1.0
}

/// A board on its own clock as a device input delivers it: stereo blocks
/// of silence, each stamped with the master-clock time of its first frame
/// give or take [`JITTER`] and delivered once its last frame is in, plus
/// up to [`DELAY`].
#[derive(Debug, Clone, Copy)]
struct Board {
    /// Frames per block.
    frames: u64,
    /// How fast its clock runs, in ppm.
    ppm: f64,
    /// Master time of its first frame, in seconds.
    start: f64,
    /// From this block on, its frames come this many seconds later: the
    /// audio between was lost.
    loss: Option<(u64, f64)>,
}

impl Board {
    /// A board with blocks of `frames`, `ppm` fast, whose first frame is at
    /// `start` seconds, to which nothing happens.
    fn new(frames: u64, ppm: f64, start: f64) -> Self {
        Self {
            frames,
            ppm,
            start,
            loss: None,
        }
    }

    /// The board losing `seconds` of audio from [`LOSS_AT`] on.
    fn losing(self, seconds: f64) -> Self {
        let block = self.frames as f64 / f64::from(RATE);
        let from = ((LOSS_AT - self.start) / block).ceil() as u64;
        Self {
            loss: Some((from, seconds)),
            ..self
        }
    }

    /// Its frames per second of master time.
    fn rate(&self) -> f64 {
        f64::from(RATE) * (1.0 + self.ppm * 1e-6)
    }

    /// Block `k` (its samples `samples`) and when it is delivered, in
    /// seconds.
    fn block(&self, k: u64, samples: &Arc<[f32]>) -> (AudioBlock, f64) {
        let lost = match self.loss {
            Some((from, by)) if k >= from => by,
            _ => 0.0,
        };
        let first = self.start + (k * self.frames) as f64 / self.rate() + lost;
        let pts = (first + spread(2 * k) * JITTER).max(0.0);
        let late = (spread(2 * k + 1) + 1.0) / 2.0 * DELAY;
        let block = AudioBlock {
            source: SourceId("board".into()),
            seq: k,
            pts: Duration::from_secs_f64(pts),
            sample_rate: RATE,
            channels: 2,
            samples: samples.clone(),
        };
        (block, first + self.frames as f64 / self.rate() + late)
    }
}

/// What a run reports.
#[derive(Debug)]
struct Run {
    /// Each pushed block's PTS and offset (encoder position less PTS), in
    /// seconds.
    offsets: Vec<(f64, f64)>,
    /// The pacer's counters at the end.
    pacer: AudioPacerStats,
    /// The board as the operator sees it at the end.
    view: AudioInputView,
}

impl Run {
    /// Mean offset of the blocks with PTS over the run's last second, in
    /// seconds.
    fn offset(&self) -> f64 {
        let last = (SECONDS - 1) as f64;
        let (sum, n) = self
            .offsets
            .iter()
            .filter(|(pts, _)| *pts >= last)
            .fold((0.0, 0u32), |(sum, n), (_, o)| (sum + o, n + 1));
        sum / f64::from(n.max(1))
    }
}

/// Plays `board` as program audio through the output's pacer, the output
/// starting (its frame count zero) at tick `output_at`: the board's blocks
/// drained before then are not heard.
fn play(board: &Board, output_at: u64) -> Run {
    let (slot, handles) = scripted("board", Some((RATE, 2)), None);
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    let mut pacer = AudioPacer::new(RATE, 2, FPS as u32, AUDIO_SLACK, AUDIO_MAX_LEAD);
    let samples: Arc<[f32]> = vec![0.0; 2 * board.frames as usize].into();
    let (mut k, mut position, mut offsets) = (0, 0u64, Vec::new());
    let mut next = board.block(k, &samples);
    for tick in 1..=SECONDS * FPS {
        let mut blocks = Vec::new();
        while next.1 <= tick as f64 / FPS as f64 {
            blocks.push(next.0);
            k += 1;
            next = board.block(k, &samples);
        }
        handles.queue.push(blocks);
        let taken = audio.take(&[]);
        if tick <= output_at {
            continue;
        }
        for item in pacer.plan(tick - output_at, taken) {
            match item {
                PacedAudio::Silence(n) => position += n,
                PacedAudio::Block(b) => {
                    let pts = b.pts.as_secs_f64();
                    offsets.push((pts, position as f64 / f64::from(RATE) - pts));
                    position += b.frames() as u64;
                }
            }
        }
    }
    Run {
        offsets,
        pacer: pacer.stats(),
        view: audio.views().remove(0),
    }
}

/// Plays a board with blocks of each of `frames`, at 0 and ±100 ppm and
/// each of `starts`, losing 85.3, 96 and 99.9 ms, with the output started
/// at tick `output_at`; returns what went wrong, and how many runs the
/// pacer filled a gap in.
fn lose_close_to_the_slack(frames: &[u64], starts: &[f64], output_at: u64) -> (Vec<String>, u32) {
    let (mut wrong, mut filled) = (Vec::new(), 0);
    for &frames in frames {
        for ppm in [0.0, 100.0, -100.0] {
            for &start in starts {
                let twin = Board::new(frames, ppm, start);
                let base = play(&twin, output_at).offset();
                for lost in [0.085_333, 0.096, 0.0999] {
                    let run = play(&twin.losing(lost), output_at);
                    let (pacer, view) = (&run.pacer, &run.view);
                    let left = (run.offset() - base) * 1e3;
                    filled += u32::from(pacer.silence > 0);
                    if left.abs() > 0.5
                        || (view.drift_steps, view.drift_resets) != (1, 0)
                        || pacer.dropped != pacer.silence
                    {
                        wrong.push(format!(
                            "{frames} frames, {ppm} ppm, first frame at {:.2} ms, {:.1} ms lost: \
                             {left:+.2} ms left, {pacer:?}, {view:?}",
                            start * 1e3,
                            lost * 1e3
                        ));
                    }
                }
            }
        }
    }
    (wrong, filled)
}

/// First-frame times a millisecond apart over a tick, so the board's blocks
/// fall at every phase of the ticks.
fn over_a_tick() -> Vec<f64> {
    (0..34).map(|i| 0.003 + f64::from(i) * 0.001).collect()
}

#[test]
fn audio_lost_close_to_the_slack_after_the_output_started_is_made_up_once() {
    // The output starts a second after the board, as when a show goes live
    // with its inputs running.
    let (wrong, filled) = lose_close_to_the_slack(&[128, 256, 512, 1024], &over_a_tick(), FPS);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(filled > 0, "the pacer never ran out of slack");
}

#[test]
fn audio_lost_close_to_the_slack_of_a_board_opened_with_the_output_is_made_up_once() {
    // The output starts as the board opens, its first frame up to 5 ms
    // later: the board runs that much behind the video's share, and the
    // pacer runs out of slack more often.
    let starts: Vec<f64> = (0..34).map(|i| f64::from(i) * 0.000_15).collect();
    let (wrong, filled) = lose_close_to_the_slack(&[512, 1024], &starts, 0);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(filled > 0, "the pacer never ran out of slack");
}
