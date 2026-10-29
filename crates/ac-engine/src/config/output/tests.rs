//! Tests for the `[output]` table's checks: they are the encoder's, so a
//! value the encoder would refuse at Start Output fails when the config
//! loads, and every value it takes still loads.

use std::path::{Path, PathBuf};

use super::*;
use crate::config::{ConfigError, StudioConfig};

/// A one-source studio whose `[output]` table holds `keys`.
fn studio(keys: &str) -> String {
    format!(
        "schema = 1\n\n[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 640\n\
         height = 360\nfps = 30\n\n[output]\n{keys}\n"
    )
}

/// A fresh, empty folder for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ac-engine-output-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Loads `text` from a file in `dir`, as the app does.
fn load(dir: &Path, text: &str) -> Result<StudioConfig, ConfigError> {
    let path = dir.join("studio.toml");
    std::fs::write(&path, text).unwrap();
    StudioConfig::load(&path)
}

#[test]
fn a_keyframe_interval_the_encoder_refuses_fails_at_load() {
    let dir = temp_dir("keyframe");
    match load(&dir, &studio("keyframe_seconds = 30")) {
        Err(ConfigError::Invalid(msg)) => {
            assert_eq!(msg, "output: keyframe_seconds = 30 is outside 1..=10");
        }
        other => panic!("expected invalid, got {other:?}"),
    }
    let c = load(&dir, &studio("keyframe_seconds = 10")).unwrap();
    assert_eq!(c.output.keyframe_seconds, 10);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn every_bad_value_in_the_table_is_listed() {
    let text = studio("keyframe_seconds = 30\nvideo_bitrate_kbps = 50\naudio_bitrate_kbps = 1");
    let c = StudioConfig::parse(&text, false).unwrap();
    assert_eq!(
        c.problems(),
        [
            "output: video_bitrate_kbps = 50 is outside 100..=100000",
            "output: keyframe_seconds = 30 is outside 1..=10",
            "output: audio_bitrate_kbps = 1 is outside 32..=512",
        ]
    );
    assert_eq!(
        c.output.validate(),
        Err("output: video_bitrate_kbps = 50 is outside 100..=100000".into()),
        "a load fails on the first"
    );
    assert!(OutputSettings::default().problems().is_empty());
}

/// A named change to valid settings and whether the encoder takes it.
type Case = (&'static str, fn(&mut OutputSettings), bool);

#[test]
fn the_ranges_are_the_encoders_at_both_ends() {
    let cases: [Case; 22] = [
        ("keyframe_seconds", |o| o.keyframe_seconds = 1, true),
        ("keyframe_seconds", |o| o.keyframe_seconds = 0, false),
        ("keyframe_seconds", |o| o.keyframe_seconds = 11, false),
        ("video_bitrate_kbps", |o| o.video_bitrate_kbps = 100, true),
        (
            "video_bitrate_kbps",
            |o| o.video_bitrate_kbps = 100_000,
            true,
        ),
        ("video_bitrate_kbps", |o| o.video_bitrate_kbps = 99, false),
        (
            "video_bitrate_kbps",
            |o| o.video_bitrate_kbps = 100_001,
            false,
        ),
        ("audio_bitrate_kbps", |o| o.audio_bitrate_kbps = 32, true),
        ("audio_bitrate_kbps", |o| o.audio_bitrate_kbps = 512, true),
        ("audio_bitrate_kbps", |o| o.audio_bitrate_kbps = 0, false),
        ("audio_bitrate_kbps", |o| o.audio_bitrate_kbps = 513, false),
        ("fps", |o| o.fps = 120, true),
        ("fps", |o| o.fps = 0, false),
        ("fps", |o| o.fps = 121, false),
        ("width", |o| o.width = 7680, true),
        ("width", |o| o.width = 7682, false),
        ("height", |o| o.height = 4320, true),
        ("height", |o| o.height = 4322, false),
        ("height", |o| o.height = 14, false),
        ("odd", |o| o.width = 1281, false),
        ("ffmpeg", |o| o.ffmpeg = PathBuf::new(), false),
        (
            "rtmp_url",
            |o| o.rtmp_url = Some("http://x/app/k".into()),
            false,
        ),
    ];
    for (key, change, takes) in cases {
        let mut o = OutputSettings::default();
        change(&mut o);
        let encoder = o
            .to_output_config(
                o.rtmp_url.clone(),
                Some("show.mp4".into()),
                Some(STAND_IN_AUDIO),
            )
            .validate();
        assert_eq!(encoder.is_ok(), takes, "{key}: the encoder's own answer");
        match o.validate() {
            Ok(()) => assert!(takes, "{key}: accepted"),
            Err(msg) => {
                assert!(!takes, "{key}: {msg}");
                assert!(msg.starts_with("output: ") && msg.contains(key), "{msg}");
            }
        }
    }
}

#[test]
fn the_audio_bitrate_is_checked_without_an_audio_source() {
    let o = OutputSettings {
        audio_bitrate_kbps: 0,
        audio_source: None,
        ..OutputSettings::default()
    };
    let msg = o.validate().unwrap_err();
    assert!(msg.contains("audio_bitrate_kbps = 0"), "{msg}");
}

#[test]
fn destinations_are_checked_for_form_only() {
    let defaults = OutputSettings::default();
    defaults.validate().unwrap();
    assert_eq!((&defaults.rtmp_url, &defaults.record_path), (&None, &None));
    let empty = OutputSettings {
        rtmp_url: Some("  ".into()),
        record_path: Some(PathBuf::new()),
        ..OutputSettings::default()
    };
    empty.validate().unwrap();
    let elsewhere = OutputSettings {
        record_path: Some("/no/such/folder/show.mp4".into()),
        rtmp_url: Some("rtmps://live.example.com/app/SECRET-KEY".into()),
        ..OutputSettings::default()
    };
    elsewhere.validate().unwrap();
    let typo = OutputSettings {
        rtmp_url: Some("rtmp:/live.example.com/app/SECRET-KEY".into()),
        ..OutputSettings::default()
    };
    let msg = typo.validate().unwrap_err();
    assert!(msg.contains("rtmp_url"), "{msg}");
    assert!(!msg.contains("SECRET"), "the stream key stays out: {msg}");
}

#[test]
fn a_bad_url_and_bitrate_fail_at_load_and_the_recording_stays_as_written() {
    let dir = temp_dir("load");
    match load(&dir, &studio("video_bitrate_kbps = 50")) {
        Err(ConfigError::Invalid(msg)) => assert!(msg.contains("video_bitrate_kbps"), "{msg}"),
        other => panic!("expected invalid, got {other:?}"),
    }
    match load(&dir, &studio("rtmp_url = \"live.example.com/app/key\"")) {
        Err(ConfigError::Invalid(msg)) => assert!(msg.contains("rtmp_url"), "{msg}"),
        other => panic!("expected invalid, got {other:?}"),
    }
    let c = load(&dir, &studio("record_path = \"../recordings/show.mp4\"")).unwrap();
    assert_eq!(
        c.output.record_path,
        Some("../recordings/show.mp4".into()),
        "taken from the working folder when the output starts"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
