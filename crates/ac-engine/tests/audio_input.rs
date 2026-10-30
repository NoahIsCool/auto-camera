//! Audio inputs through the engine runtime (spec M12, "one stereo input"):
//! a soundboard recording, played as an audio-only input, is program
//! audio, picked to the stereo pair its config names before the encoder,
//! while a source's own mono track keeps the level it always had.
//!
//! The encoder's ffmpeg is a wrapper that runs the real one with one more
//! output: the program audio exactly as the engine sent it (raw `f32`,
//! before AAC), so the test checks it sample by sample against the signal
//! the WAV was written with ([`fake_sample`]). The WAV files are made by
//! the tests; none is kept in the repository. Skipped when ffmpeg is not
//! installed (unless `AC_REQUIRE_FFMPEG` is set).
//!
//! These engines run in real time ([`start_real_time`]), not on the
//! sped-up clock of the other engine tests: a looping file input starts
//! the decoder of its next pass one pass ahead, and four times real time
//! leaves ffmpeg a quarter of that to start, which a loaded machine misses
//! for a short file; the silence the pacer then inserts would read here as
//! lost audio.
//!
//! One engine runs on a wall clock that holds its engine thread up now and
//! then ([`Hitching`]), the board's input keeping real time meanwhile: an
//! engine that runs late must not cost the program any of the board's
//! audio (the pacer's counters say so, read from the last snapshot).

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ac_core::ShotId;
use ac_engine::{
    AudioInputEntry, DetectorSettings, Engine, EngineCommand, EngineHandle, EngineOptions,
    OutputState, StudioConfig,
};
use ac_media::{ChannelPick, SourceState};
use ac_sources::{
    AudioInputConfig, Clock, SourceConfig, WallClock, WavEncoding, fake_sample, write_fake_wav,
};

use common::{WAIT, have, studio, temp_path};

/// Sample frames per block of a 48 kHz file input: blocks are cut (and
/// dropped by the pacer) whole, so a jump in the program audio can only be
/// at a multiple of this.
const BLOCK: u64 = 480;

/// Starts `config` paced by the wall clock; see the module docs.
fn start_real_time(config: StudioConfig) -> Engine {
    Engine::start_with(config, EngineOptions::default()).expect("engine starts")
}

/// A fresh folder for one test.
fn folder(name: &str) -> PathBuf {
    let dir = temp_path(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes, in `dir`, an ffmpeg that also writes program audio as it
/// arrives (input 1, raw `f32` stereo) to the returned capture file. A
/// listing or probe (`-encoders`, `-h`, `-version`) runs ffmpeg as it is.
fn capturing_ffmpeg(dir: &Path) -> (PathBuf, PathBuf) {
    let (wrapper, capture) = (dir.join("ffmpeg"), dir.join("program.f32"));
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do\n  case \"$a\" in -encoders|-h|-version) exec ffmpeg \"$@\" ;; esac\ndone\n\
         exec ffmpeg \"$@\" -map 1:a:0 -c:a pcm_f32le -f f32le -y '{}'\n",
        capture.display()
    );
    std::fs::write(&wrapper, script).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    (wrapper, capture)
}

/// A one-source studio without detection whose program audio is audio
/// input `entry`, encoded by `ffmpeg`.
fn board_studio(entry: AudioInputEntry, ffmpeg: PathBuf) -> StudioConfig {
    let mut config = studio(1);
    config.detector = DetectorSettings::None;
    config.queries.clear();
    config.output.audio_source = Some(entry.id.clone());
    config.output.ffmpeg = ffmpeg;
    config.audio_inputs.push(entry);
    config
}

/// Audio input "board" looping `path`, picking `channels`.
fn board(path: &Path, channels: Option<Vec<u16>>, channel_count: Option<u16>) -> AudioInputEntry {
    AudioInputEntry {
        id: "board".into(),
        name: Some("Board".into()),
        channels,
        channel_count,
        input: AudioInputConfig::File {
            path: path.into(),
            loop_playback: true,
        },
    }
}

/// Records `seconds` of program to `record` once the source and every
/// audio input deliver, then stops the output; returns the last
/// snapshot's loudness sample peak.
fn record_program(h: &EngineHandle, record: &Path, seconds: u64) -> f64 {
    let wide = ShotId("stage/wide".into());
    h.wait_until(WAIT, |s| {
        s.shot(&wide).is_some_and(|v| v.available)
            && s.audio_inputs
                .iter()
                .all(|a| a.state == SourceState::Running)
    })
    .expect("the source and the audio inputs deliver");
    h.send(EngineCommand::StartOutput {
        rtmp_url: None,
        record_path: Some(record.into()),
    })
    .unwrap();
    let running = h
        .wait_until(WAIT, |s| s.output.state == OutputState::Running)
        .expect("the encoder starts");
    let first = running.output.frames_pushed;
    let live = h
        .wait_until(WAIT, |s| {
            s.output.frames_pushed >= first + seconds * 30
                && s.output.loudness.is_some_and(|l| l.momentary.is_some())
        })
        .expect("program audio reaches the encoder and its meter");
    h.send(EngineCommand::StopOutput).unwrap();
    let done = h
        .wait_until(WAIT, |s| {
            !matches!(s.output.state, OutputState::Stopping | OutputState::Running)
        })
        .expect("the encoder finishes");
    assert_eq!(
        done.output.state,
        OutputState::Finished,
        "{:?}",
        done.output
    );
    live.output.loudness.unwrap().sample_peak.expect("a peak")
}

/// The stereo frames of a raw `f32` capture.
fn read_capture(path: &Path) -> Vec<(f32, f32)> {
    let bytes = std::fs::read(path).expect("the capture was written");
    assert_eq!(bytes.len() % 8, 0, "whole stereo frames");
    bytes
        .chunks_exact(8)
        .map(|f| {
            let at = |i: usize| f32::from_le_bytes(f[i..i + 4].try_into().unwrap());
            (at(0), at(4))
        })
        .collect()
}

/// The frame of a `frames`-frame file of the fake signal whose channel
/// `channel` holds `v`, if one does.
fn frame_of(channel: u16, v: f32, frames: u64) -> Option<u64> {
    let step = (f64::from(v) * 32_768.0 + 32_768.0) as i64;
    let n = (step - i64::from(channel) * 4_099).rem_euclid(65_536) as u64;
    (n < frames && fake_sample(channel, n).to_bits() == v.to_bits()).then_some(n)
}

/// Checks that every frame of `capture` that is not inserted silence is
/// frame `n` of a looping `frames`-frame file, its channel `left` on the
/// left and `right` on the right, bit for bit, and that the audio runs on
/// frame by frame except where whole blocks were left out. Returns how
/// many frames were audio.
fn check_program(capture: &[(f32, f32)], left: u16, right: u16, frames: u64) -> u64 {
    let mut previous: Option<u64> = None;
    let (mut audio, mut jumps) = (0, 0);
    for (i, &(l, r)) in capture.iter().enumerate() {
        if (l, r) == (0.0, 0.0) {
            // Silence the pacer inserted (the fake signal never has both
            // channels at zero), between whole blocks.
            if let Some(p) = previous.take() {
                assert!(
                    (p + 1).is_multiple_of(BLOCK),
                    "silence inside a block, at {i}"
                );
            }
            continue;
        }
        let n = frame_of(left, l, frames)
            .unwrap_or_else(|| panic!("frame {i}: {l} is not a sample of channel {}", left + 1));
        assert_eq!(
            r.to_bits(),
            fake_sample(right, n).to_bits(),
            "frame {i}: the right side is not channel {} of the same frame",
            right + 1
        );
        match previous {
            Some(p) if (p + 1) % frames == n => {}
            Some(p) => {
                assert!(
                    (p + 1).is_multiple_of(BLOCK) && n.is_multiple_of(BLOCK),
                    "a cut inside a block at {i}"
                );
                jumps += 1;
            }
            None => assert!(
                n.is_multiple_of(BLOCK),
                "audio resumes inside a block at {i}"
            ),
        }
        previous = Some(n);
        audio += 1;
    }
    assert!(jumps < 20, "{jumps} cuts");
    audio
}

/// The channels of the first audio stream of `path`, by ffprobe.
fn recorded_channels(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0"])
        .args([
            "-show_entries",
            "stream=channels,sample_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Acceptance: a 16-channel board recording with channels 3 and 4 picked
/// reaches the encoder as stereo, bit-exact; the meters read it, and the
/// recording is stereo.
#[test]
fn a_sixteen_channel_board_reaches_the_encoder_as_its_picked_pair() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let dir = folder("board16");
    let (frames, wav) = (48_000, dir.join("board.wav"));
    write_fake_wav(&wav, 48_000, 16, frames, WavEncoding::Pcm16, false).unwrap();
    let (ffmpeg, capture) = capturing_ffmpeg(&dir);
    let entry = board(&wav, Some(vec![3, 4]), Some(16));
    let engine = start_real_time(board_studio(entry, ffmpeg));
    let record = dir.join("program.mp4");
    let peak = record_program(&engine.handle(), &record, 3);
    let report = engine.shutdown().unwrap();
    let view = &report.final_snapshot.audio_inputs[0];
    assert_eq!(view.name, "Board");
    assert_eq!(view.format, Some((48_000, 16)));
    assert_eq!(view.pick, Some(ChannelPick::Pair(2, 3)));
    assert!(view.program && view.blocks > 0, "{view:?}");
    assert_eq!((view.unusable, view.error.clone()), (0, None));
    assert_eq!(view.state, SourceState::Stopped);

    let audio = check_program(&read_capture(&capture), 2, 3, frames);
    assert!(audio >= 2 * 48_000, "only {audio} frames of audio");
    // The meter reads the picked pair: its loudest sample is theirs.
    let loudest = (0..frames)
        .flat_map(|n| [fake_sample(2, n), fake_sample(3, n)])
        .fold(0f32, |m, v| m.max(v.abs()));
    let want = 20.0 * f64::from(loudest).log10();
    assert!((peak - want).abs() < 0.05, "peak {peak} dBFS, want {want}");
    assert_eq!(recorded_channels(&record), "48000,2");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance: a mono input is program audio on both sides, at full level.
#[test]
fn a_mono_input_is_on_both_sides_of_the_program() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let dir = folder("mono");
    // Under 32768 frames, so no frame of the signal is zero.
    let (frames, wav) = (60 * BLOCK, dir.join("mono.wav"));
    write_fake_wav(&wav, 48_000, 1, frames, WavEncoding::Pcm16, false).unwrap();
    let (ffmpeg, capture) = capturing_ffmpeg(&dir);
    let engine = start_real_time(board_studio(board(&wav, None, None), ffmpeg));
    let record = dir.join("program.mp4");
    record_program(&engine.handle(), &record, 3);
    let report = engine.shutdown().unwrap();
    let view = &report.final_snapshot.audio_inputs[0];
    assert_eq!((view.format, view.pick), (Some((48_000, 1)), None));
    let audio = check_program(&read_capture(&capture), 0, 0, frames);
    assert!(audio >= 2 * 48_000, "only {audio} frames of audio");
    assert_eq!(recorded_channels(&record), "48000,2");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Writes, in `dir`, a 1 s 320x180 30 fps clip whose embedded audio is
/// ffmpeg's 48 kHz 440 Hz `sine` tone (crest 1/8) on one channel.
fn mono_clip(dir: &Path) -> PathBuf {
    let path = dir.join("mono.mkv");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg("testsrc=size=320x180:rate=30:duration=1")
        .args(["-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=48000:duration=1")
        .args(["-ac", "1", "-c:a", "pcm_s16le"])
        .args(["-c:v", "mpeg4", "-q:v", "4"])
        .arg(&path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not make the clip");
    path
}

/// A source's mono track as program audio is on both sides at −3 dB, as
/// the encoder spread it when it was given the source's own channel
/// count: a config without audio inputs sounds and meters as it did.
#[test]
fn a_sources_mono_track_keeps_its_level_in_the_program() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let dir = folder("embedded");
    let (ffmpeg, capture) = capturing_ffmpeg(&dir);
    let mut config = studio(1);
    config.sources[0].input = SourceConfig::File {
        path: mono_clip(&dir),
        loop_playback: true,
        max_width: None,
        audio: true,
        start_s: None,
        duration_s: None,
    };
    config.detector = DetectorSettings::None;
    config.queries.clear();
    config.output.audio_source = Some("stage".into());
    config.output.ffmpeg = ffmpeg;
    let engine = start_real_time(config);
    let record = dir.join("program.mp4");
    let peak = record_program(&engine.handle(), &record, 2);
    engine.shutdown().unwrap();

    let mut loudest = 0f32;
    for (i, (l, r)) in read_capture(&capture).into_iter().enumerate() {
        assert_eq!(l.to_bits(), r.to_bits(), "frame {i}: both sides alike");
        loudest = loudest.max(l.abs());
    }
    let want = 0.125 * std::f64::consts::FRAC_1_SQRT_2;
    let got = f64::from(loudest);
    assert!((got - want).abs() < 1e-4, "peak {got}, want {want} (−3 dB)");
    let want = 20.0 * want.log10();
    assert!((peak - want).abs() < 0.05, "meter {peak} dBFS, want {want}");
    assert_eq!(recorded_channels(&record), "48000,2");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A board recording without the picked channels is refused when it opens:
/// the input stays offline with the reason (and is retried), and the rest
/// of the studio runs.
#[test]
fn an_input_without_the_picked_channels_stays_offline_with_the_reason() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let dir = folder("refused");
    let wav = dir.join("stereo.wav");
    write_fake_wav(&wav, 48_000, 2, 4_800, WavEncoding::Pcm16, false).unwrap();
    let engine = start_real_time(board_studio(
        board(&wav, Some(vec![3, 4]), None),
        "ffmpeg".into(),
    ));
    let h = engine.handle();
    let wide = ShotId("stage/wide".into());
    let snap = h
        .wait_until(WAIT, |s| s.shot(&wide).is_some_and(|v| v.available))
        .expect("the video source runs");
    let view = &snap.audio_inputs[0];
    assert!(
        matches!(view.state, SourceState::Offline | SourceState::Reconnecting),
        "{view:?}"
    );
    let why = view.error.as_deref().unwrap_or_default();
    assert!(
        why.contains("channels 3 and 4 need an input of at least 4 channels; it has 2"),
        "{why}"
    );
    assert_eq!((view.blocks, view.format), (0, None));
    engine.shutdown().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The wall clock, except that the engine thread, waiting for its next
/// tick, oversleeps by `stall` once every `every`: an engine that hitches.
/// Its ticks then run late, and back to back as it catches up, while the
/// sources and audio inputs keep real time on their own threads.
struct Hitching {
    /// Real time.
    wall: WallClock,
    /// How long each hitch holds the engine thread.
    stall: Duration,
    /// Time from one hitch to the next.
    every: Duration,
    /// Clock reading of the next hitch, and the hitches so far.
    next: Mutex<(Duration, u64)>,
}

impl Hitching {
    /// An engine thread that hitches for `stall` every `every`.
    fn new(stall: Duration, every: Duration) -> Self {
        Self {
            wall: WallClock::new(),
            stall,
            every,
            next: Mutex::new((every, 0)),
        }
    }

    /// The hitches so far.
    fn hitches(&self) -> u64 {
        self.next.lock().unwrap().1
    }
}

impl Clock for Hitching {
    fn now(&self) -> Duration {
        self.wall.now()
    }

    fn sleep(&self, duration: Duration) {
        let mut extra = Duration::ZERO;
        if std::thread::current().name() == Some("ac-engine") {
            let now = self.wall.now();
            let mut next = self.next.lock().unwrap();
            if now >= next.0 {
                *next = (now + self.every, next.1 + 1);
                extra = self.stall;
            }
        }
        std::thread::sleep(duration + extra);
    }
}

/// An engine thread that hitches for 150 ms every 2 s takes each late
/// tick's audio with 150 ms more of the board's than the tick's share,
/// over the pacer's 100 ms lead limit: none of it is dropped, as it is the
/// engine that is late, not the audio, and so no silence goes in to give
/// it back. The video source runs at 25 fps, so that the phase lock
/// (which takes the frames a late tick skips at the tick rate for a
/// collision, and delays the schedule) stays out of the measurement.
#[test]
fn an_engine_that_hitches_drops_none_of_the_boards_audio() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let dir = folder("hitches");
    let wav = dir.join("board.wav");
    write_fake_wav(&wav, 48_000, 2, 48_000 * 30, WavEncoding::Pcm16, false).unwrap();
    let clock = Arc::new(Hitching::new(
        Duration::from_millis(150),
        Duration::from_secs(2),
    ));
    let options = EngineOptions {
        clock: clock.clone(),
        ..EngineOptions::default()
    };
    let mut config = board_studio(board(&wav, None, None), "ffmpeg".into());
    config.sources[0].input = SourceConfig::Synthetic {
        width: 320,
        height: 180,
        fps: 25.0,
        subjects: 1,
    };
    let engine = Engine::start_with(config, options).expect("engine starts");
    let before = clock.hitches();
    record_program(&engine.handle(), &dir.join("program.mp4"), 8);
    let hitches = clock.hitches() - before;
    let report = engine.shutdown().unwrap();
    let snap = &report.final_snapshot;
    let audio = snap.output.audio.expect("program audio");
    assert!(
        hitches >= 3 && snap.timing.late_ticks >= 3,
        "{hitches}, {:?}",
        snap.timing
    );
    assert_eq!((audio.dropped, audio.silence), (0, 0), "{audio:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
