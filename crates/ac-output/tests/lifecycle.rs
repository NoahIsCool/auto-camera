//! Lifecycle and back-pressure tests against a fake ffmpeg.
//!
//! The fake is a shell script that answers `-encoders` like ffmpeg and then
//! either hangs without reading its input or fails at once. That makes the
//! hard cases deterministic and needs no media tooling: a hung encoder must
//! be killed by `finish` and by `Drop`, a full queue must drop and count
//! without blocking, and an encoder that dies must be reported. Unix only,
//! because the fake is a `/bin/sh` script.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{AudioBlock, VideoFrame};
use ac_output::{AudioFormat, Encoder, OutputConfig, OutputError, Push};

/// How the fake ffmpeg behaves after the encoder probe.
#[derive(Clone, Copy)]
enum Fake {
    /// Never reads its input and never exits on its own.
    Hang,
    /// Prints an error and exits with status 3.
    Fail,
}

/// Writes the fake ffmpeg script and returns a config that uses it.
fn fake_config(fake: Fake, name: &str) -> (OutputConfig, PathBuf) {
    let dir = common::scratch_dir(name);
    let script = dir.join("ffmpeg");
    let behaviour = match fake {
        Fake::Hang => "exec sleep 30",
        Fake::Fail => "echo 'fake ffmpeg failed on purpose' >&2; exit 3",
    };
    let body = format!(
        "#!/bin/sh\ncase \" $* \" in *\" -encoders \"*) \
         echo ' V....D libx264   libx264 H.264'; exit 0;; esac\n{behaviour}\n"
    );
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = OutputConfig {
        ffmpeg: script,
        width: 320,
        height: 180,
        encoder: Encoder::Libx264,
        record_path: Some(dir.join("program.mp4")),
        ..OutputConfig::default()
    };
    (config, dir)
}

/// The same config with 48 kHz stereo audio.
fn with_audio(config: OutputConfig) -> OutputConfig {
    OutputConfig {
        audio: Some(AudioFormat {
            sample_rate: 48_000,
            channels: 2,
        }),
        ..config
    }
}

#[test]
fn finish_kills_a_hung_ffmpeg_after_the_timeout() {
    let (config, dir) = fake_config(Fake::Hang, "hang-finish");
    let enc = common::start(&with_audio(config)).unwrap();
    let pid = enc.pid();
    for i in 0..10u32 {
        enc.push_video(&common::frame(320, 180, i)).unwrap();
        enc.push_audio(&[0.0; 960]).unwrap();
    }
    let started = Instant::now();
    let err = enc
        .finish_with_timeout(Duration::from_millis(200))
        .unwrap_err();
    assert!(matches!(err, OutputError::Timeout(_)), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(!common::process_exists(pid), "fake ffmpeg {pid} survived");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn drop_kills_a_hung_ffmpeg() {
    let (config, dir) = fake_config(Fake::Hang, "hang-drop");
    let enc = common::start(&with_audio(config)).unwrap();
    let pid = enc.pid();
    assert!(common::process_exists(pid));
    assert!(enc.stats().ffmpeg_alive);
    let started = Instant::now();
    drop(enc);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!common::process_exists(pid), "fake ffmpeg {pid} survived");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn full_queues_drop_and_count_without_blocking() {
    let (config, dir) = fake_config(Fake::Hang, "backpressure");
    let enc = common::start(&with_audio(config)).unwrap();
    let frame = common::frame(320, 180, 0);
    let started = Instant::now();
    let mut dropped = 0;
    for _ in 0..100 {
        if enc.push_video(&frame).unwrap() == Push::Dropped {
            dropped += 1;
        }
    }
    // The fake never connects to the audio input, so its queue fills too.
    let mut audio_dropped = 0;
    for _ in 0..400 {
        if enc.push_audio(&[0.1; 480]).unwrap() == Push::Dropped {
            audio_dropped += 1;
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "pushing blocked"
    );
    let stats = enc.stats();
    assert_eq!(stats.frames_in, 100);
    // A frame is larger than any pipe buffer, so at most a handful get out.
    assert!(dropped >= 90, "only {dropped} dropped");
    assert_eq!(stats.frames_dropped, dropped);
    assert!(audio_dropped >= 100, "only {audio_dropped} audio drops");
    assert_eq!(stats.audio_samples_in, 400 * 480);
    assert_eq!(stats.audio_samples_dropped, audio_dropped * 480);
    assert!(!stats.audio_connected);
    drop(enc);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_encoder_that_dies_is_reported() {
    let (config, dir) = fake_config(Fake::Fail, "fail");
    let enc = common::start(&config).unwrap();
    let frame = common::frame(320, 180, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    let stopped = loop {
        match enc.push_video(&frame) {
            Err(OutputError::Stopped) => break true,
            Ok(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            other => panic!("expected Stopped, got {other:?}"),
        }
    };
    assert!(stopped);
    let deadline = Instant::now() + Duration::from_secs(10);
    while enc.stats().ffmpeg_alive && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let stats = enc.stats();
    assert!(!stats.ffmpeg_alive);
    assert_eq!(stats.exit_code, Some(3));
    match enc.finish() {
        Err(OutputError::Failed { last_error, .. }) => {
            assert_eq!(last_error.as_deref(), Some("fake ffmpeg failed on purpose"));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pushes_are_validated() {
    let (config, dir) = fake_config(Fake::Hang, "validate");
    let enc = common::start(&with_audio(config.clone())).unwrap();
    assert!(matches!(
        enc.push_video(&[0; 16]),
        Err(OutputError::FrameSize {
            expected: 230_400,
            actual: 16
        })
    ));
    let small = VideoFrame::new(
        SourceId("pgm".into()),
        0,
        Duration::ZERO,
        2,
        2,
        Arc::from(vec![0u8; 16]),
    )
    .unwrap();
    assert!(matches!(
        enc.push_frame(&small),
        Err(OutputError::FrameDimensions {
            expected: (320, 180),
            actual: (2, 2)
        })
    ));
    let full = VideoFrame::new(
        SourceId("pgm".into()),
        1,
        Duration::ZERO,
        320,
        180,
        Arc::from(common::frame(320, 180, 1)),
    )
    .unwrap();
    assert!(enc.push_frame(&full).is_ok());
    assert!(matches!(
        enc.push_audio(&[0.0; 3]),
        Err(OutputError::AudioLength {
            len: 3,
            channels: 2
        })
    ));
    assert_eq!(enc.push_audio(&[]).unwrap(), Push::Queued);
    let block = |sample_rate, channels| AudioBlock {
        source: SourceId("mix".into()),
        seq: 0,
        pts: Duration::ZERO,
        sample_rate,
        channels,
        samples: Arc::from(vec![0.0f32; 96]),
    };
    assert!(matches!(
        enc.push_audio_block(&block(44_100, 2)),
        Err(OutputError::AudioFormatMismatch { .. })
    ));
    assert!(enc.push_audio_block(&block(48_000, 2)).is_ok());
    // Only valid pushes are counted.
    let stats = enc.stats();
    assert_eq!(stats.frames_in, 1);
    assert_eq!(stats.audio_samples_in, 96);
    drop(enc);

    let silent = common::start(&OutputConfig {
        record_path: Some(dir.join("silent.mp4")),
        ..config
    })
    .unwrap();
    assert!(matches!(
        silent.push_audio(&[0.0; 2]),
        Err(OutputError::AudioNotConfigured)
    ));
    assert!(matches!(
        silent.push_audio_block(&block(48_000, 2)),
        Err(OutputError::AudioNotConfigured)
    ));
    drop(silent);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn repeats_for_skipped_ticks_are_owed_up_to_the_debt_cap() {
    let (config, dir) = fake_config(Fake::Hang, "repeats");
    let enc = common::start(&config).unwrap();
    assert_eq!(enc.repeat_frames(0), 0);
    assert_eq!(enc.repeat_frames(12), 12);
    // Ten seconds of video at 30 fps at most, whatever was owed before.
    assert_eq!(enc.repeat_frames(u64::MAX), 300 - 12);
    assert_eq!(enc.repeat_frames(1), 0);
    let stats = enc.stats();
    assert_eq!((stats.frames_dropped, stats.frames_in), (0, 0), "{stats:?}");
    drop(enc);
    std::fs::remove_dir_all(dir).unwrap();
}
