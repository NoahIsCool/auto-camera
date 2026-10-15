//! End-to-end encodes with the real ffmpeg, verified with ffprobe.
//!
//! Every test returns early (passing) when ffmpeg with libx264 or ffprobe is
//! not installed, so the suite needs no media tooling to pass; with the
//! tools present they prove the command line, the transports and the
//! lifecycle against the actual encoder. No network is used: the RTMP test
//! targets a closed loopback port.

mod common;

use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use ac_output::{AudioFormat, Encoder, EncoderChoice, OutputConfig, OutputError};

/// A small 30 fps libx264 config recording to `path`.
fn small(path: std::path::PathBuf) -> OutputConfig {
    OutputConfig {
        width: 320,
        height: 180,
        fps: 30,
        video_bitrate_kbps: 800,
        keyframe_seconds: 1,
        encoder: Encoder::Libx264,
        record_path: Some(path),
        ..OutputConfig::default()
    }
}

#[test]
fn encodes_frames_and_sine_to_a_playable_fragmented_mp4() {
    if !common::have_ffmpeg() {
        return;
    }
    let dir = common::scratch_dir("mp4");
    // Quote and space exercise the tee escaping.
    let path = dir.join("it's the program.mp4");
    let audio = AudioFormat {
        sample_rate: 48_000,
        channels: 2,
    };
    let config = OutputConfig {
        audio: Some(audio),
        audio_bitrate_kbps: 128,
        ..small(path.clone())
    };
    let enc = common::start(&config).unwrap();
    assert_eq!(enc.encoder(), EncoderChoice::Libx264);
    let sine = common::sine(48_000, 2, 440.0, 1.0);
    let per_frame = 48_000 / 30 * 2;
    // As fast as possible: drops are expected and must be repaid exactly.
    for i in 0..60u32 {
        enc.push_video(&common::frame(320, 180, i)).unwrap();
        let start = i as usize * per_frame;
        if let Some(block) = sine.get(start..start + per_frame) {
            enc.push_audio(block).unwrap();
        }
    }
    let stats = enc.finish().unwrap();
    assert_eq!(stats.frames_in, 60);
    assert_eq!(stats.frames_written, 60, "{stats:?}");
    assert_eq!(
        stats.frames_written - stats.frames_repeated + stats.frames_dropped,
        60
    );
    assert_eq!(stats.audio_samples_written, 96_000, "{stats:?}");
    assert!(stats.audio_connected);
    assert!(!stats.ffmpeg_alive);
    assert_eq!(stats.exit_code, Some(0));
    assert!(stats.progress.ended, "{stats:?}");
    assert_eq!(stats.progress.frame, 60);
    assert!(
        stats.progress.bitrate_kbps.is_some_and(|b| b > 0.0),
        "{stats:?}"
    );

    let video = common::probe(
        &path,
        &[
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,width,height,pix_fmt,color_space,color_primaries,\
             color_transfer,color_range,has_b_frames,nb_read_frames,duration",
        ],
    );
    assert_eq!(video["codec_name"], "h264");
    assert_eq!(
        (video["width"].as_str(), video["height"].as_str()),
        ("320", "180")
    );
    assert_eq!(video["pix_fmt"], "yuv420p");
    for key in ["color_space", "color_primaries", "color_transfer"] {
        assert_eq!(video[key], "bt709", "{key}");
    }
    assert_eq!(video["color_range"], "tv");
    assert_eq!(video["has_b_frames"], "0");
    assert_eq!(video["nb_read_frames"], "60");
    let vdur = common::num(&video, "duration");
    assert!((1.95..2.1).contains(&vdur), "video duration {vdur}");

    // keyframe_seconds = 1 at 30 fps: keyframes at frames 0 and 30.
    let keys = common::probe(
        &path,
        &[
            "-select_streams",
            "v:0",
            "-skip_frame",
            "nokey",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
        ],
    );
    assert_eq!(keys["nb_read_frames"], "2");

    let sound = common::probe(
        &path,
        &[
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,sample_rate,channels,duration",
        ],
    );
    assert_eq!(sound["codec_name"], "aac");
    assert_eq!(sound["sample_rate"], "48000");
    assert_eq!(sound["channels"], "2");
    let adur = common::num(&sound, "duration");
    assert!((0.95..1.1).contains(&adur), "audio duration {adur}");

    let format = common::probe(&path, &["-show_entries", "format=duration"]);
    let fdur = common::num(&format, "duration");
    assert!((1.95..2.1).contains(&fdur), "format duration {fdur}");
    // Fragmented MP4: movie fragments, so a crash keeps what was written.
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.windows(4).any(|w| w == b"moof"), "no moof box");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn repeated_frames_for_skipped_ticks_keep_the_timeline() {
    if !common::have_ffmpeg() {
        return;
    }
    let dir = common::scratch_dir("repeats");
    let path = dir.join("program.mp4");
    let enc = common::start(&small(path.clone())).unwrap();
    for i in 0..30u32 {
        enc.push_video(&common::frame(320, 180, i)).unwrap();
    }
    // The caller skipped 15 ticks: they are owed, then 15 more frames.
    assert_eq!(enc.repeat_frames(15), 15);
    for i in 30..45u32 {
        enc.push_video(&common::frame(320, 180, i)).unwrap();
    }
    let stats = enc.finish().unwrap();
    // Dropped frames are repaid too, so the count is exact either way.
    assert_eq!(stats.frames_written, 60, "{stats:?}");
    assert_eq!(
        stats.frames_repeated,
        stats.frames_dropped + 15,
        "{stats:?}"
    );
    let video = common::probe(
        &path,
        &[
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
        ],
    );
    assert_eq!(video["nb_read_frames"], "60");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_test_encode_tells_working_encoders_apart() {
    if !common::have_ffmpeg() {
        return;
    }
    let ffmpeg = std::path::Path::new("ffmpeg");
    assert!(ac_output::encoder_works(ffmpeg, EncoderChoice::Libx264));
    // Off macOS, Auto never tries VideoToolbox.
    if !cfg!(target_os = "macos") {
        assert_eq!(
            ac_output::detect_encoder(ffmpeg, Encoder::Auto).unwrap(),
            EncoderChoice::Libx264
        );
    }
}

/// A real VideoToolbox encode, when this Mac can open a session (hosted
/// runners and virtual machines usually cannot: then Auto must have fallen
/// back to libx264, which is checked instead).
#[cfg(target_os = "macos")]
#[test]
fn videotoolbox_encodes_a_playable_file_when_it_can() {
    if !common::have_ffmpeg() {
        return;
    }
    let ffmpeg = std::path::Path::new("ffmpeg");
    let auto = ac_output::detect_encoder(ffmpeg, Encoder::Auto).unwrap();
    if !ac_output::encoder_works(ffmpeg, EncoderChoice::VideoToolbox) {
        eprintln!("skipping: VideoToolbox cannot open a session here");
        assert_eq!(auto, EncoderChoice::Libx264);
        return;
    }
    assert_eq!(auto, EncoderChoice::VideoToolbox);
    let dir = common::scratch_dir("vt");
    let path = dir.join("program.mp4");
    let config = OutputConfig {
        encoder: Encoder::VideoToolbox,
        ..small(path.clone())
    };
    let enc = common::start(&config).unwrap();
    assert_eq!(enc.encoder(), EncoderChoice::VideoToolbox);
    for i in 0..30u32 {
        enc.push_video(&common::frame(320, 180, i)).unwrap();
        thread::sleep(Duration::from_millis(5));
    }
    let stats = enc.finish().unwrap();
    assert_eq!(stats.frames_written, 30, "{stats:?}");
    let video = common::probe(
        &path,
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height",
        ],
    );
    assert_eq!(video["codec_name"], "h264");
    assert_eq!(video["width"], "320");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unreachable_rtmp_does_not_stop_the_recording() {
    if !common::have_ffmpeg() {
        return;
    }
    let dir = common::scratch_dir("rtmp");
    let path = dir.join("program.mp4");
    // A port that was just free: connections to it are refused.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = OutputConfig {
        rtmp_url: Some(format!("rtmp://127.0.0.1:{port}/live/test-stream-key")),
        ..small(path.clone())
    };
    let enc = common::start(&config).unwrap();
    for i in 0..45u32 {
        enc.push_video(&common::frame(320, 180, i)).unwrap();
        thread::sleep(Duration::from_millis(5));
    }
    let stats = enc.finish().unwrap();
    assert_eq!(stats.frames_written, 45);
    let err = stats.last_error.expect("the RTMP failure is reported");
    assert!(!err.contains("test-stream-key"), "key leaked: {err}");

    let video = common::probe(
        &path,
        &[
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,nb_read_frames",
        ],
    );
    assert_eq!(video["codec_name"], "h264");
    assert_eq!(video["nb_read_frames"], "45");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn finish_reports_progress_and_leaves_no_child() {
    if !common::have_ffmpeg() {
        return;
    }
    let dir = common::scratch_dir("finish");
    let enc = common::start(&small(dir.join("program.mp4"))).unwrap();
    let pid = enc.pid();
    assert!(common::process_exists(pid));
    // Real-time pacing until ffmpeg has published a progress report.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut i = 0;
    let stats = loop {
        enc.push_video(&common::frame(320, 180, i)).unwrap();
        i += 1;
        thread::sleep(Duration::from_millis(33));
        let stats = enc.stats();
        if stats.progress.frame > 0 || Instant::now() > deadline {
            break stats;
        }
    };
    assert!(stats.ffmpeg_alive, "{stats:?}");
    assert_eq!(stats.exit_code, None);
    assert!(stats.progress.frame > 0, "no progress report: {stats:?}");
    assert!(stats.progress.out_time.is_some());
    let done = enc.finish().unwrap();
    assert_eq!(done.frames_written, u64::from(i));
    assert!(!common::process_exists(pid), "ffmpeg {pid} still running");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dropping_a_running_encoder_kills_ffmpeg() {
    if !common::have_ffmpeg() {
        return;
    }
    let dir = common::scratch_dir("drop");
    let config = OutputConfig {
        audio: Some(AudioFormat {
            sample_rate: 48_000,
            channels: 1,
        }),
        ..small(dir.join("program.mp4"))
    };
    let enc = common::start(&config).unwrap();
    let pid = enc.pid();
    for i in 0..5u32 {
        enc.push_video(&common::frame(320, 180, i)).unwrap();
        enc.push_audio(&vec![0.0; 1600]).unwrap();
    }
    drop(enc);
    assert!(!common::process_exists(pid), "ffmpeg {pid} still running");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn existing_recordings_are_never_overwritten() {
    if !common::have_ffmpeg() {
        return;
    }
    let dir = common::scratch_dir("exists");
    let path = dir.join("program.mp4");
    std::fs::write(&path, b"yesterday's service").unwrap();
    let err = common::start(&small(path.clone())).unwrap_err();
    assert!(matches!(err, OutputError::InvalidConfig(_)), "{err:?}");
    assert_eq!(std::fs::read(&path).unwrap(), b"yesterday's service");
    std::fs::remove_dir_all(dir).unwrap();
}
