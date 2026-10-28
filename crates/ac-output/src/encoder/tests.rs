//! Unit tests for the encoder paths that fail before or without ffmpeg
//! (the real encodes live in `tests/ffmpeg.rs` and `tests/lifecycle.rs`).

use super::*;
use std::path::PathBuf;

/// A config whose ffmpeg cannot exist, for paths that fail before spawn.
fn config() -> OutputConfig {
    OutputConfig {
        ffmpeg: "/nonexistent/ffmpeg-for-ac-output-tests".into(),
        record_path: Some(std::env::temp_dir().join("ac-output-never-written.mp4")),
        ..OutputConfig::default()
    }
}

#[test]
fn handle_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<ProgramEncoder>();
}

#[test]
fn start_validates_before_spawning() {
    let err = ProgramEncoder::start(OutputConfig {
        width: 641,
        ..config()
    })
    .unwrap_err();
    assert!(matches!(err, OutputError::InvalidConfig(_)), "{err:?}");
}

#[test]
fn start_reports_missing_ffmpeg() {
    let err = ProgramEncoder::start(config()).unwrap_err();
    assert!(
        matches!(&err, OutputError::FfmpegNotFound(p) if p == &config().ffmpeg),
        "{err:?}"
    );
    let err = detect_encoder(&config().ffmpeg, Encoder::Auto).unwrap_err();
    assert!(matches!(err, OutputError::FfmpegNotFound(_)), "{err:?}");
}

#[test]
fn start_refuses_missing_directory_or_existing_file() {
    let missing = OutputConfig {
        record_path: Some(PathBuf::from("/nonexistent-ac-output-dir/program.mp4")),
        ..config()
    };
    let err = ProgramEncoder::start(missing).unwrap_err();
    assert!(
        matches!(&err, OutputError::InvalidConfig(m) if m.contains("does not exist")),
        "{err:?}"
    );
    let existing = std::env::temp_dir().join(format!("ac-output-exists-{}", std::process::id()));
    std::fs::write(&existing, b"keep me").unwrap();
    let err = ProgramEncoder::start(OutputConfig {
        record_path: Some(existing.clone()),
        ..config()
    })
    .unwrap_err();
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
    std::fs::remove_file(&existing).unwrap();
    assert!(
        matches!(&err, OutputError::InvalidConfig(m) if m.contains("refusing to overwrite")),
        "{err:?}"
    );
}

#[test]
fn a_missing_ffmpeg_cannot_encode() {
    assert!(!encoder_works(&config().ffmpeg, EncoderChoice::Libx264));
    assert!(!encoder_works(
        &config().ffmpeg,
        EncoderChoice::VideoToolbox
    ));
}

#[test]
fn relative_record_path_in_current_directory_is_allowed() {
    let c = OutputConfig {
        record_path: Some(PathBuf::from("ac-output-relative-never-written.mp4")),
        ..config()
    };
    check_record_path(&c).unwrap();
}

#[test]
fn help_listings_are_searched_for_whole_options() {
    let help = "Advanced per-stream options:\n\
                -stats_enc_post     write encoding stats after encoding\n\
                -stats_mux_pre      write packets stats before muxing\n\
                -stats_mux_pre_fmt  format of the stats written with -stats_mux_pre\n";
    assert!(lists_option(help, "-stats_mux_pre"));
    assert!(lists_option(help, "-stats_mux_pre_fmt"));
    // Named only in another option's description, or only as a prefix.
    assert!(!lists_option(
        "-other  see -stats_mux_pre\n",
        "-stats_mux_pre"
    ));
    assert!(!lists_option("-stats_mux_pre_fmt  x\n", "-stats_mux_pre"));
    assert!(!lists_option("", "-stats_mux_pre"));
    assert!(!mux_stats_supported(&config().ffmpeg));
}
