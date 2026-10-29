//! Tests of program audio: an input's pick to stereo (bit for bit), a
//! video source's audio taken as it is, the origin's format and phase
//! reference, draining every input, and a drifting, stalling input kept in
//! step with video by the output's pacer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ac_media::{AudioInputInfo, SourceInfo, VideoFrame};
use ac_sources::{FakeAudio, FakeFeed, Gap, fake_sample};

use super::*;
use crate::audio::{AudioPacer, PacedAudio, RECOVERY_TOLERANCE};
use crate::output::{AUDIO_MAX_LEAD, AUDIO_SLACK};

/// Blocks waiting to be drained, shared with the test.
#[derive(Clone, Default)]
struct Queue(Arc<Mutex<Vec<AudioBlock>>>);

impl Queue {
    /// Queues `blocks`.
    fn push(&self, blocks: impl IntoIterator<Item = AudioBlock>) {
        self.0.lock().unwrap().extend(blocks);
    }

    /// Takes everything queued.
    fn take(&self) -> Vec<AudioBlock> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// An audio input that delivers what the test queues; connected (with
/// `format`) or not yet (`None`).
struct Scripted {
    /// The description it started with.
    info: AudioInputInfo,
    /// The newest connection's format.
    format: Option<(u32, u16)>,
    /// What `drain` returns.
    queue: Queue,
    /// Raised by `stop`.
    stopped: Arc<AtomicBool>,
}

impl AudioInput for Scripted {
    fn info(&self) -> &AudioInputInfo {
        &self.info
    }

    fn current_info(&self) -> AudioInputInfo {
        AudioInputInfo {
            id: self.info.id.clone(),
            format: self.format,
        }
    }

    fn state(&self) -> SourceState {
        match self.format {
            Some(_) => SourceState::Running,
            None => SourceState::Offline,
        }
    }

    fn drain(&self) -> Vec<AudioBlock> {
        self.queue.take()
    }

    fn wait(&self, _timeout: Duration) -> bool {
        !self.queue.0.lock().unwrap().is_empty()
    }

    fn dropped(&self) -> u64 {
        3
    }

    fn error(&self) -> Option<String> {
        self.format
            .is_none()
            .then(|| "board.wav does not exist".into())
    }

    fn stop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

/// A video source whose embedded audio is what the test queues.
struct Camera {
    /// Its description.
    info: SourceInfo,
    /// What `drain_audio` returns.
    queue: Queue,
}

impl VideoSource for Camera {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        SourceState::Running
    }

    fn latest(&self) -> Option<VideoFrame> {
        None
    }

    fn wait_newer(&self, _after: Option<u64>, _timeout: Duration) -> Option<VideoFrame> {
        None
    }

    fn drain_audio(&self) -> Vec<AudioBlock> {
        self.queue.take()
    }

    fn error(&self) -> Option<String> {
        None
    }

    fn stop(&mut self) {}
}

/// Handles on a [`Scripted`] input: its queue and its stop flag.
struct Handles {
    /// Its queue.
    queue: Queue,
    /// Raised when it is stopped.
    stopped: Arc<AtomicBool>,
}

/// A slot for a scripted input `id` with `format`, giving `pick`.
fn scripted(
    id: &str,
    format: Option<(u32, u16)>,
    pick: Option<ChannelPick>,
) -> (InputSlot, Handles) {
    let (queue, stopped) = (Queue::default(), Arc::new(AtomicBool::new(false)));
    let input = Scripted {
        info: AudioInputInfo {
            id: SourceId(id.into()),
            format: None,
        },
        format,
        queue: queue.clone(),
        stopped: stopped.clone(),
    };
    (
        InputSlot::new(id, pick, Box::new(input)),
        Handles { queue, stopped },
    )
}

/// A camera `id` whose audio is `audio`, and its queue.
fn camera(id: &str, audio: Option<(u32, u16)>) -> (Box<dyn VideoSource>, Queue) {
    let queue = Queue::default();
    let info = SourceInfo {
        id: SourceId(id.into()),
        width: 64,
        height: 36,
        fps: 30.0,
        audio,
    };
    let source = Camera {
        info,
        queue: queue.clone(),
    };
    (Box::new(source), queue)
}

/// Block `seq` of the fake signal from `source`: `frames` frames of
/// `channels` channels at 48 kHz, from frame `seq * frames`.
fn block(source: &str, seq: u64, channels: u16, frames: u64) -> AudioBlock {
    let first = seq * frames;
    let samples: Vec<f32> = (first..first + frames)
        .flat_map(|n| (0..channels).map(move |c| fake_sample(c, n)))
        .collect();
    AudioBlock {
        source: SourceId(source.into()),
        seq,
        pts: Duration::from_secs_f64(first as f64 / 48_000.0),
        sample_rate: 48_000,
        channels,
        samples: samples.into(),
    }
}

/// The left and right samples of stereo `blocks`, frame by frame.
fn frames_of(blocks: &[AudioBlock]) -> Vec<(f32, f32)> {
    blocks
        .iter()
        .inspect(|b| assert_eq!(b.channels, 2))
        .flat_map(|b| {
            b.samples
                .chunks(2)
                .map(|f| (f[0], f[1]))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn a_sixteen_channel_input_gives_its_picked_pair_bit_for_bit() {
    let (slot, board) = scripted("board", Some((48_000, 16)), Some(ChannelPick::Pair(2, 3)));
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    board.queue.push((0..3).map(|k| block("board", k, 16, 480)));
    let out = audio.take(&[]);
    assert_eq!(out.len(), 3);
    for (k, b) in out.iter().enumerate() {
        assert_eq!((b.seq, b.source.0.as_str()), (k as u64, "board"));
        assert_eq!(b.pts, Duration::from_millis(10 * k as u64));
        assert_eq!(b.sample_rate, 48_000);
    }
    for (n, (l, r)) in frames_of(&out).into_iter().enumerate() {
        let n = n as u64;
        assert_eq!(
            l.to_bits(),
            fake_sample(2, n).to_bits(),
            "left of frame {n}"
        );
        assert_eq!(
            r.to_bits(),
            fake_sample(3, n).to_bits(),
            "right of frame {n}"
        );
    }
    let view = &audio.views()[0];
    assert_eq!((view.blocks, view.unusable, view.dropped), (3, 0, 3));
    assert!(view.program);
    assert_eq!(view.pick, Some(ChannelPick::Pair(2, 3)));
    assert_eq!(view.format, Some((48_000, 16)));
}

#[test]
fn a_mono_input_is_on_both_sides_by_default() {
    let (slot, board) = scripted("board", Some((48_000, 1)), None);
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    board.queue.push([block("board", 0, 1, 480)]);
    let frames = frames_of(&audio.take(&[]));
    assert_eq!(frames.len(), 480);
    for (n, (l, r)) in frames.into_iter().enumerate() {
        let want = fake_sample(0, n as u64).to_bits();
        assert_eq!((l.to_bits(), r.to_bits()), (want, want), "frame {n}");
    }
    // One channel of many, on both sides, at full level.
    let (slot, board) = scripted("board", Some((48_000, 8)), Some(ChannelPick::Mono(4)));
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    board.queue.push([block("board", 0, 8, 480)]);
    for (n, (l, r)) in frames_of(&audio.take(&[])).into_iter().enumerate() {
        let want = fake_sample(4, n as u64).to_bits();
        assert_eq!((l.to_bits(), r.to_bits()), (want, want), "frame {n}");
    }
}

#[test]
fn a_block_without_the_picked_channels_is_dropped_and_counted() {
    let (slot, board) = scripted("board", Some((48_000, 2)), Some(ChannelPick::Pair(2, 3)));
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    board
        .queue
        .push([block("board", 0, 2, 480), block("board", 1, 4, 480)]);
    let out = audio.take(&[]);
    assert_eq!(out.iter().map(|b| b.seq).collect::<Vec<_>>(), [1]);
    let view = &audio.views()[0];
    assert_eq!((view.blocks, view.unusable), (2, 1));
}

#[test]
fn every_input_and_source_is_drained_and_only_the_origin_kept() {
    let (spare, spare_handles) = scripted("spare", Some((48_000, 2)), None);
    let (board, board_handles) = scripted("board", Some((48_000, 2)), None);
    let (cam, cam_audio) = camera("cam", Some((48_000, 2)));
    let sources = vec![cam];
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(1)), vec![spare, board]);
    let program = block("board", 0, 2, 480);
    spare_handles.queue.push([block("spare", 0, 2, 480)]);
    board_handles.queue.push([program.clone()]);
    cam_audio.push([block("cam", 0, 2, 480)]);
    let out = audio.take(&sources);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].source, SourceId("board".into()));
    // A stereo block with the default pick is passed on as it is.
    assert!(Arc::ptr_eq(&out[0].samples, &program.samples));
    // The others were drained too, so nothing builds up.
    assert!(cam_audio.take().is_empty() && spare_handles.queue.take().is_empty());
    let views = audio.views();
    assert_eq!(
        views
            .iter()
            .map(|v| (v.blocks, v.program))
            .collect::<Vec<_>>(),
        [(1, false), (1, true)]
    );
    audio.stop();
    assert!(spare_handles.stopped.load(Ordering::SeqCst));
    assert!(board_handles.stopped.load(Ordering::SeqCst));
}

#[test]
fn a_video_sources_audio_is_taken_as_it_is() {
    // Its decoder mixes it to stereo (`ac_sources::FileSource`), so the
    // engine neither picks nor mixes it: a stereo block passes as it is,
    // and a block of another count is not put on both sides but left for
    // the pacer to drop as another format than the encoder's.
    let (cam, cam_audio) = camera("cam", Some((48_000, 2)));
    let sources = vec![cam];
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Source(0)), Vec::new());
    let stereo = block("cam", 0, 2, 480);
    cam_audio.push([stereo.clone(), block("cam", 1, 1, 480)]);
    let out = audio.take(&sources);
    assert_eq!(out.len(), 2);
    assert!(Arc::ptr_eq(&out[0].samples, &stereo.samples));
    assert_eq!(
        (out[1].seq, out[1].channels),
        (1, 1),
        "not picked to stereo"
    );
    let format = audio.format(&sources).unwrap();
    let mut pacer = AudioPacer::new(format.sample_rate, 2, 30, AUDIO_SLACK, AUDIO_MAX_LEAD);
    pacer.plan(1, out);
    assert_eq!(
        (pacer.stats().pushed, pacer.stats().mismatched_blocks),
        (480, 1)
    );
    assert_eq!(audio.phase_reference(), 0);
    assert!(audio.views().is_empty());
}

#[test]
fn the_output_format_is_stereo_at_the_origins_rate() {
    let stereo = |sample_rate| {
        Some(AudioFormat {
            sample_rate,
            channels: 2,
        })
    };
    let (cam, _) = camera("cam", Some((44_100, 2)));
    let (silent, _) = camera("silent", None);
    let sources = vec![cam, silent];
    let of = |origin, inputs| ProgramAudio::with_inputs(origin, inputs).format(&sources);
    assert_eq!(of(Some(Origin::Source(0)), Vec::new()), stereo(44_100));
    assert_eq!(
        of(Some(Origin::Source(1)), Vec::new()),
        None,
        "no audio described"
    );
    assert_eq!(of(None, Vec::new()), None, "a silent program");
    let (connected, _) = scripted("board", Some((96_000, 16)), None);
    assert_eq!(of(Some(Origin::Input(0)), vec![connected]), stereo(96_000));
    let (waiting, _) = scripted("board", None, None);
    assert_eq!(
        of(Some(Origin::Input(0)), vec![waiting]),
        stereo(STAND_IN_RATE),
        "not connected yet"
    );
}

#[test]
fn an_input_that_is_not_there_yet_shows_why() {
    let (slot, _) = scripted("board", None, Some(ChannelPick::Pair(2, 3)));
    let audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    assert_eq!(audio.phase_reference(), 0, "the first source");
    assert_eq!(audio.origin(), Some(Origin::Input(0)));
    let view = &audio.views()[0];
    assert_eq!(view.state, SourceState::Offline);
    assert_eq!(view.error.as_deref(), Some("board.wav does not exist"));
    assert_eq!(view.format, None);
    assert_eq!(view.name, "board");
}

/// Plays `plan` (16 channels, picking 3 and 4) as program audio for
/// `seconds` of 30 fps video, through the output's pacer, and returns the
/// pacer's lead after every tick (in ms) and its final counters.
fn pace(plan: FakeAudio, seconds: u64) -> (Vec<i64>, crate::audio::AudioPacerStats) {
    let mut feed = FakeFeed::new(SourceId("board".into()), plan).unwrap();
    let (slot, board) = scripted("board", Some((48_000, 16)), Some(ChannelPick::Pair(2, 3)));
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    let format = audio.format(&[]).unwrap();
    let mut pacer = AudioPacer::new(format.sample_rate, 2, 30, AUDIO_SLACK, AUDIO_MAX_LEAD);
    let mut leads = Vec::new();
    for tick in 1..=seconds * 30 {
        board
            .queue
            .push(feed.until(Duration::from_secs_f64(tick as f64 / 30.0)));
        let items = pacer.plan(tick, audio.take(&[]));
        assert!(items.iter().all(|i| match i {
            PacedAudio::Block(b) => b.channels == 2,
            PacedAudio::Silence(_) => true,
        }));
        leads.push(pacer.stats().lead_ms);
    }
    (leads, pacer.stats())
}

/// A 16-channel plan at 48 kHz drifting by `ppm`, with `gaps`.
fn board_plan(ppm: f64, gaps: Vec<Gap>) -> FakeAudio {
    FakeAudio {
        drift_ppm: ppm,
        gaps,
        ..FakeAudio::steady(48_000, 16)
    }
}

#[test]
fn a_drifting_stalling_input_stays_in_step_with_video() {
    let limit = |leads: &[i64]| {
        let bound = AUDIO_MAX_LEAD.as_millis() as i64;
        let floor = -(AUDIO_SLACK.as_millis() as i64);
        assert!(
            leads.iter().all(|l| (floor..=bound).contains(l)),
            "lead out of bounds: {leads:?}"
        );
    };
    // A sample clock 1% fast: the surplus is cut, never left to pile up.
    let (leads, fast) = pace(board_plan(10_000.0, Vec::new()), 20);
    limit(&leads);
    assert!(fast.dropped > 0 && fast.silence == 0, "{fast:?}");
    // 1% slow: the shortfall is filled with silence.
    let (leads, slow) = pace(board_plan(-10_000.0, Vec::new()), 20);
    limit(&leads);
    assert!(slow.silence > 0 && slow.dropped == 0, "{slow:?}");
    // A stall of half a second whose audio arrives late in a burst:
    // silence fills the gap, the late backlog is cut, and the program is
    // back in step within a tick of the burst.
    let stall = Gap {
        from: Duration::from_secs(5),
        until: Duration::from_millis(5_500),
        held: true,
    };
    let (leads, stalled) = pace(board_plan(0.0, vec![stall]), 10);
    limit(&leads);
    assert!(stalled.silence > 0 && stalled.dropped > 0, "{stalled:?}");
    let tolerance = RECOVERY_TOLERANCE.as_millis() as i64 + 10;
    let after = &leads[(5.6 * 30.0) as usize..];
    assert!(
        after.iter().all(|l| l.abs() <= tolerance),
        "not back in step: {after:?}"
    );
    // A stall whose audio is lost: silence, and nothing to cut.
    let lost = Gap {
        held: false,
        ..stall
    };
    let (leads, gone) = pace(board_plan(0.0, vec![lost]), 10);
    limit(&leads);
    assert!(gone.silence > 0, "{gone:?}");
    assert!(
        leads[(5.6 * 30.0) as usize..]
            .iter()
            .all(|l| l.abs() <= tolerance)
    );
}
