//! Audio-only file inputs against real ffmpeg, on WAV and RF64 files the
//! tests write ([`write_fake_wav`]), so every decoded sample is checked
//! against the signal that made it, bit for bit.
//!
//! Every test skips (passes with a note) when `ffmpeg`/`ffprobe` are not
//! installed, unless `AC_REQUIRE_FFMPEG` is set, which makes a missing tool
//! a failure. No media file lives in the repository.

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{AudioBlock, AudioInput, AudioInputInfo, ChannelPick, MediaError, SourceState};
use ac_sources::{
    AudioFileInput, AudioFileOptions, AudioInputConfig, Backoff, FfmpegTools, SupervisedAudio,
    VirtualClock, WallClock, WavEncoding, fake_sample, open_supervised_audio, probe_audio,
    write_fake_wav,
};
use common::{TempDir, make_clip, poll, tools};

/// The id every test input has.
fn board() -> SourceId {
    SourceId("board".into())
}

/// Options that keep every block of a short file (nothing is dropped
/// however fast an unpaced input plays).
fn keep_all(tools: &FfmpegTools, loop_playback: bool) -> AudioFileOptions {
    AudioFileOptions {
        loop_playback,
        queue_blocks: 100_000,
        tools: tools.clone(),
    }
}

/// Opens `path` unpaced (as fast as ffmpeg decodes).
fn open_unpaced(path: &Path, options: AudioFileOptions) -> AudioFileInput {
    AudioFileInput::open_with_clock(board(), path, options, Arc::new(VirtualClock::new())).unwrap()
}

/// Plays `input` to its end and returns every block it delivered.
fn play_out(input: &AudioFileInput) -> Vec<AudioBlock> {
    let mut blocks = Vec::new();
    assert!(poll(Duration::from_secs(30), || {
        blocks.extend(input.drain());
        input.state() == SourceState::Stopped
    }));
    blocks.extend(input.drain());
    blocks
}

/// The samples of `blocks`, in order.
fn samples(blocks: &[AudioBlock]) -> Vec<f32> {
    blocks
        .iter()
        .flat_map(|b| b.samples.iter().copied())
        .collect()
}

/// Asserts that `blocks` are numbered `first..` without a gap and follow
/// each other in time without a gap or an overlap.
fn assert_contiguous(blocks: &[AudioBlock], first: u64) {
    for (i, b) in blocks.iter().enumerate() {
        assert_eq!(b.seq, first + i as u64, "block {i}");
    }
    for w in blocks.windows(2) {
        let end = w[0].pts.as_secs_f64() + w[0].duration().as_secs_f64();
        let gap = w[1].pts.as_secs_f64() - end;
        assert!(gap.abs() < 1e-6, "gap of {gap}s after {:?}", w[0].pts);
        assert!(w[0].pts < w[1].pts, "PTS went back after {:?}", w[0].pts);
    }
}

/// Asserts that `got` is the fake signal of `channels` channels from frame
/// 0, bit for bit, over `frames` frames.
fn assert_fake_signal(got: &[f32], channels: u16, frames: u64) {
    assert_eq!(got.len() as u64, frames * u64::from(channels));
    for (i, &v) in got.iter().enumerate() {
        let (n, c) = (
            i as u64 / u64::from(channels),
            i as u64 % u64::from(channels),
        );
        let want = fake_sample(c as u16, n);
        assert_eq!(v.to_bits(), want.to_bits(), "frame {n} channel {}", c + 1);
    }
}

/// True while `pid` exists (zombies included).
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Kills process `pid` with `SIGKILL`.
#[cfg(unix)]
fn kill(pid: u32) {
    let killed = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success(), "kill {pid}");
}

#[test]
fn a_sixteen_channel_wav_decodes_bit_exact() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("wav16");
    let wav = dir.0.join("board.wav");
    write_fake_wav(&wav, 48_000, 16, 2_400, WavEncoding::Pcm16, false).unwrap();
    let stream = probe_audio(&tools, &wav).unwrap();
    assert_eq!((stream.sample_rate, stream.channels), (48_000, 16));
    let input = open_unpaced(&wav, keep_all(&tools, false));
    assert_eq!(input.info().format, Some((48_000, 16)));
    let blocks = play_out(&input);
    assert_eq!(input.error(), None);
    assert_eq!(input.dropped(), 0);
    assert!(
        blocks
            .iter()
            .all(|b| b.source == board() && (b.sample_rate, b.channels) == (48_000, 16))
    );
    assert_eq!(blocks[0].pts, Duration::ZERO);
    assert_contiguous(&blocks, 0);
    assert_fake_signal(&samples(&blocks), 16, 2_400);
}

#[test]
fn an_rf64_float_file_reads_exactly() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("rf64");
    let wav = dir.0.join("board.wav");
    write_fake_wav(&wav, 44_100, 1, 1_000, WavEncoding::Float32, true).unwrap();
    let input = open_unpaced(&wav, keep_all(&tools, false));
    assert_eq!(input.info().format, Some((44_100, 1)));
    let blocks = play_out(&input);
    assert_eq!(input.error(), None);
    assert_contiguous(&blocks, 0);
    assert_fake_signal(&samples(&blocks), 1, 1_000);
}

/// Acceptance: an audio-only file that loops keeps its PTS monotonic and
/// gap-free and its sequence numbers counting across passes, and every
/// pass plays the whole file again (its last, partial block included).
#[test]
fn an_audio_only_file_loops_with_monotonic_pts() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("loop");
    let wav = dir.0.join("board.wav");
    // 8 blocks of 480 frames and a partial one of 160.
    let frames = 4_000u64;
    write_fake_wav(&wav, 48_000, 1, frames, WavEncoding::Pcm16, false).unwrap();
    let mut input = open_unpaced(&wav, keep_all(&tools, true));
    let mut blocks = Vec::new();
    let passes = 3;
    assert!(poll(Duration::from_secs(30), || {
        blocks.extend(input.drain());
        blocks.iter().map(|b| b.samples.len() as u64).sum::<u64>() >= passes * frames
    }));
    assert_eq!(input.state(), SourceState::Running);
    input.stop();
    assert_eq!((input.dropped(), input.error()), (0, None));
    assert_contiguous(&blocks, 0);
    let got = samples(&blocks);
    for (g, &v) in got.iter().enumerate() {
        let want = fake_sample(0, g as u64 % frames);
        assert_eq!(v.to_bits(), want.to_bits(), "frame {g}");
    }
    // The pass boundary is where a partial block ends.
    let ends: Vec<usize> = blocks
        .iter()
        .scan(0u64, |at, b| {
            *at += b.samples.len() as u64;
            Some(*at)
        })
        .enumerate()
        .filter(|&(_, at)| at % frames == 0)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(&ends[..2], &[8, 17]);
    assert_eq!(blocks[8].samples.len(), 160);
    let third = blocks[18].pts.as_secs_f64();
    assert!((third - 2.0 * 4_000.0 / 48_000.0).abs() < 1e-6, "{third}");
}

#[test]
fn a_paced_file_plays_in_real_time() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("paced");
    let wav = dir.0.join("board.wav");
    write_fake_wav(&wav, 48_000, 2, 24_000, WavEncoding::Pcm16, false).unwrap();
    let started = Instant::now();
    let input = AudioFileInput::open_with_clock(
        board(),
        &wav,
        keep_all(&tools, false),
        Arc::new(WallClock::new()),
    )
    .unwrap();
    let blocks = play_out(&input);
    let took = started.elapsed();
    assert_contiguous(&blocks, 0);
    assert!(took >= Duration::from_millis(480), "{took:?} for 0.5 s");
    assert!(took < Duration::from_secs(5), "{took:?} for 0.5 s");
}

#[test]
fn unusable_audio_files_fail_to_open() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("bad");
    let missing = dir.0.join("missing.wav");
    let text = dir.0.join("notes.wav");
    std::fs::write(&text, "not audio").unwrap();
    let silent = make_clip(&tools, &dir.0, 0.2, None);
    for (path, needle) in [
        (&missing, "missing.wav"),
        (&text, "ffprobe failed"),
        (&silent, "no audio stream"),
    ] {
        match AudioFileInput::open(board(), path, keep_all(&tools, false)) {
            Err(MediaError::Open(msg)) => assert!(msg.contains(needle), "{msg}"),
            Err(other) => panic!("{path:?}: unexpected error {other}"),
            Ok(_) => panic!("{path:?} opened"),
        }
    }
}

/// A description of [`board`] before it opens.
fn offline() -> AudioInputInfo {
    AudioInputInfo {
        id: board(),
        format: None,
    }
}

/// The check the engine makes of a connection: that it has the channels
/// `pick` takes.
fn has_channels(
    pick: ChannelPick,
) -> impl Fn(&AudioInputInfo) -> Result<(), String> + Send + 'static {
    move |info| match info.format {
        Some((_, channels)) => pick.check(channels),
        None => Err("no format".into()),
    }
}

#[test]
fn a_supervised_input_refuses_a_file_without_the_picked_channels() {
    let Some(_tools) = tools() else { return };
    let dir = TempDir::new("refused");
    let wav = dir.0.join("board.wav");
    write_fake_wav(&wav, 48_000, 2, 4_800, WavEncoding::Pcm16, false).unwrap();
    let config = AudioInputConfig::File {
        path: wav.clone(),
        loop_playback: true,
    };
    let clock = Arc::new(WallClock::new());
    let mut refused = open_supervised_audio(
        &config,
        offline(),
        has_channels(ChannelPick::Pair(2, 3)),
        clock.clone(),
        Backoff::default(),
    )
    .unwrap();
    let message = "channels 3 and 4 need an input of at least 4 channels; it has 2";
    assert!(refused.start_error().is_some_and(|e| e.contains(message)));
    assert!(matches!(
        refused.state(),
        SourceState::Offline | SourceState::Reconnecting
    ));
    assert_eq!(refused.current_info().format, None);
    assert!(refused.drain().is_empty());
    let started = Instant::now();
    refused.stop();
    assert!(started.elapsed() < Duration::from_secs(2));

    let mut accepted = open_supervised_audio(
        &config,
        offline(),
        has_channels(ChannelPick::Pair(0, 1)),
        clock,
        Backoff::default(),
    )
    .unwrap();
    assert_eq!(accepted.start_error(), None);
    let mut blocks = Vec::new();
    assert!(poll(Duration::from_secs(20), || {
        blocks.extend(accepted.drain());
        blocks.len() >= 3 && accepted.state() == SourceState::Running
    }));
    assert_eq!(accepted.current_info().format, Some((48_000, 2)));
    accepted.stop();
    assert_contiguous(&blocks, 0);
}

/// A file input whose ffmpeg is killed fails with the reason; under a
/// supervisor it is opened again, its blocks carrying on, and every
/// decoder is reaped.
#[cfg(unix)]
#[test]
fn a_killed_decoder_is_reopened_and_leaves_no_child_process() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("killed");
    let wav = dir.0.join("board.wav");
    // 4 s of audio, longer than the pipe holds, so the decoder is alive.
    write_fake_wav(&wav, 48_000, 2, 192_000, WavEncoding::Pcm16, false).unwrap();
    let decoders: Arc<Mutex<Vec<Vec<u32>>>> = Arc::default();
    let (opened, path, options) = (decoders.clone(), wav.clone(), keep_all(&tools, false));
    let opener = move || -> Result<Box<dyn AudioInput>, MediaError> {
        let input = AudioFileInput::open(board(), &path, options.clone())?;
        opened.lock().unwrap().push(input.child_pids());
        Ok(Box::new(input))
    };
    let clock = Arc::new(WallClock::new());
    let mut input = SupervisedAudio::start(opener, offline(), clock, Backoff::default()).unwrap();
    let connection = |n: usize| decoders.lock().unwrap().get(n).cloned();
    let mut blocks = Vec::new();
    assert!(poll(Duration::from_secs(20), || {
        blocks.extend(input.drain());
        blocks.len() >= 5 && input.state() == SourceState::Running
    }));
    let pids = connection(0).unwrap();
    assert_eq!(pids.len(), 1, "one decoder without looping: {pids:?}");
    kill(pids[0]);
    assert!(poll(Duration::from_secs(20), || {
        blocks.extend(input.drain());
        input.state() == SourceState::Offline
            && input
                .error()
                .is_some_and(|e| e.contains("the audio decoder exited during playback"))
    }));
    assert!(!alive(pids[0]), "the killed decoder was not reaped");
    let before = blocks.len();
    assert!(poll(Duration::from_secs(20), || {
        blocks.extend(input.drain());
        blocks.len() > before + 3 && input.state() == SourceState::Running
    }));
    assert_eq!(input.error(), None);
    input.stop();
    assert!(
        blocks
            .windows(2)
            .all(|w| w[1].seq == w[0].seq + 1 && w[0].pts < w[1].pts),
        "numbers and PTS carry on across connections"
    );
    let all: Vec<u32> = decoders.lock().unwrap().concat();
    assert_eq!(all.len(), 2, "two connections");
    for pid in all {
        assert!(!alive(pid), "ffmpeg {pid} outlived its input");
    }
}

#[cfg(unix)]
#[test]
fn stop_and_drop_leave_no_child_processes() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("stop");
    let wav = dir.0.join("board.wav");
    write_fake_wav(&wav, 48_000, 2, 9_600, WavEncoding::Pcm16, false).unwrap();
    for use_drop in [false, true] {
        let mut input = AudioFileInput::open(board(), &wav, keep_all(&tools, true)).unwrap();
        assert!(poll(Duration::from_secs(20), || input.state()
            == SourceState::Running));
        let pids = input.child_pids();
        assert!(!pids.is_empty() && pids.iter().all(|&p| alive(p)));
        if use_drop {
            drop(input);
        } else {
            let started = Instant::now();
            input.stop();
            assert!(started.elapsed() < Duration::from_secs(3));
            assert_eq!(input.state(), SourceState::Stopped);
            assert!(input.child_pids().is_empty());
            input.stop();
        }
        for pid in pids {
            assert!(!alive(pid), "ffmpeg {pid} outlived its input");
        }
    }
}
