//! Unit tests for config validation, ffmpeg argument building and encoder
//! choice (including the fallback when a VideoToolbox test encode fails).

use super::*;

/// A valid config recording to a file.
fn config() -> OutputConfig {
    OutputConfig {
        record_path: Some("/tmp/program.mp4".into()),
        ..OutputConfig::default()
    }
}

/// Asserts that `c` fails validation with a message containing `needle`.
fn rejects(c: OutputConfig, needle: &str) {
    match c.validate() {
        Err(OutputError::InvalidConfig(msg)) => {
            assert!(msg.contains(needle), "{msg:?} lacks {needle:?}")
        }
        other => panic!("expected InvalidConfig({needle}), got {other:?}"),
    }
}

const LISTING: &str = "Encoders:\n V..... = Video\n ------\n \
    V....D libx264              libx264 H.264 / AVC\n \
    V....D h264_videotoolbox    VideoToolbox H.264 Encoder\n \
    A....D aac                  AAC (Advanced Audio Coding)\n";

#[test]
fn default_with_destination_is_valid() {
    config().validate().unwrap();
    let c = OutputConfig {
        rtmp_url: Some("rtmps://live.example.com:443/app/key".into()),
        record_path: None,
        audio: Some(AudioFormat {
            sample_rate: 44_100,
            channels: 1,
        }),
        ..OutputConfig::default()
    };
    c.validate().unwrap();
}

#[test]
fn rejects_bad_fields() {
    rejects(
        OutputConfig {
            width: 1921,
            ..config()
        },
        "odd",
    );
    rejects(
        OutputConfig {
            height: 0,
            ..config()
        },
        "height",
    );
    rejects(OutputConfig { fps: 0, ..config() }, "fps");
    rejects(
        OutputConfig {
            video_bitrate_kbps: 10,
            ..config()
        },
        "video_bitrate_kbps",
    );
    rejects(
        OutputConfig {
            keyframe_seconds: 0,
            ..config()
        },
        "keyframe_seconds",
    );
    rejects(
        OutputConfig {
            record_path: None,
            ..config()
        },
        "no destination",
    );
    rejects(
        OutputConfig {
            record_path: Some(PathBuf::new()),
            ..config()
        },
        "empty",
    );
    rejects(
        OutputConfig {
            ffmpeg: PathBuf::new(),
            ..config()
        },
        "ffmpeg",
    );
}

#[test]
fn rejects_bad_audio() {
    let with = |sample_rate, channels, kbps| OutputConfig {
        audio: Some(AudioFormat {
            sample_rate,
            channels,
        }),
        audio_bitrate_kbps: kbps,
        ..config()
    };
    with(48_000, 2, 160).validate().unwrap();
    rejects(with(48_000, 0, 160), "channels");
    rejects(with(1_000, 2, 160), "sample_rate");
    rejects(with(48_000, 2, 1), "audio_bitrate_kbps");
    // The audio bitrate is irrelevant without audio.
    OutputConfig {
        audio_bitrate_kbps: 0,
        ..config()
    }
    .validate()
    .unwrap();
}

#[test]
fn rejects_bad_rtmp_urls() {
    for (url, needle) in [
        ("http://example.com/live/key", "rtmp://"),
        ("rtmp://", "no host"),
        ("rtmp:///live/key", "no host"),
        ("rtmp://host/live/key with space", "whitespace"),
    ] {
        rejects(
            OutputConfig {
                rtmp_url: Some(url.into()),
                ..config()
            },
            needle,
        );
    }
    OutputConfig {
        rtmp_url: Some("RTMP://Host/live/key".into()),
        ..config()
    }
    .validate()
    .unwrap();
    assert_eq!(
        validate_rtmp_url("rtmps://live.example.com/app/key"),
        Ok(())
    );
    let msg = validate_rtmp_url("http://live.example.com/app/SECRET-KEY").unwrap_err();
    assert!(
        msg.contains("rtmp_url") && !msg.contains("SECRET"),
        "names the key, not the URL: {msg}"
    );
}

#[test]
fn every_bad_field_is_listed_and_validate_reports_the_first() {
    assert!(config().problems().is_empty());
    let c = OutputConfig {
        ffmpeg: PathBuf::new(),
        width: 1921,
        height: 2,
        fps: 0,
        video_bitrate_kbps: 50,
        keyframe_seconds: 30,
        rtmp_url: Some("http://example.com/live/key".into()),
        record_path: Some(PathBuf::new()),
        audio: Some(AudioFormat {
            sample_rate: 1_000,
            channels: 0,
        }),
        audio_bitrate_kbps: 1,
        ..config()
    };
    assert_eq!(
        c.problems(),
        [
            "ffmpeg path is empty",
            "height = 2 is outside 16..=4320",
            "1921x2 is odd; yuv420p needs even dimensions",
            "fps = 0 is outside 1..=120",
            "video_bitrate_kbps = 50 is outside 100..=100000",
            "keyframe_seconds = 30 is outside 1..=10",
            "rtmp_url must start with rtmp:// or rtmps://",
            "record_path is empty",
            "audio.sample_rate = 1000 is outside 8000..=192000",
            "audio.channels = 0 is outside 1..=8",
            "audio_bitrate_kbps = 1 is outside 32..=512",
        ]
    );
    rejects(c, "ffmpeg path is empty");
    // A side outside its range is not called odd as well; one within it is.
    let wide = OutputConfig {
        width: 7681,
        height: 1081,
        ..config()
    };
    assert_eq!(
        wide.problems(),
        [
            "width = 7681 is outside 16..=7680",
            "7681x1081 is odd; yuv420p needs even dimensions",
        ]
    );
    let only_range = OutputConfig {
        width: 15,
        ..config()
    };
    assert_eq!(only_range.problems(), ["width = 15 is outside 16..=7680"]);
    let none = OutputConfig {
        record_path: None,
        ..config()
    };
    assert_eq!(
        none.problems(),
        ["no destination: set rtmp_url, record_path or both"]
    );
}

#[test]
fn gop_and_frame_size() {
    let c = OutputConfig {
        fps: 60,
        keyframe_seconds: 2,
        width: 4,
        height: 2,
        ..config()
    };
    assert_eq!(c.gop_frames(), 120);
    assert_eq!(c.frame_bytes(), 32);
}

#[test]
fn redacts_stream_keys() {
    let url = "rtmp://a.rtmp.youtube.com/live2/abcd-efgh-ijkl";
    assert_eq!(
        redact_url(url),
        "rtmp://a.rtmp.youtube.com/live2/<redacted>"
    );
    let line = format!("[fifo @ 0x1] Error opening {url}: refused; key abcd-efgh-ijkl");
    let safe = redact(&line, Some(url));
    assert!(!safe.contains("abcd-efgh-ijkl"), "{safe}");
    assert!(safe.contains("a.rtmp.youtube.com/live2/<redacted>"));
    // Query strings are part of the key; user info is a secret too.
    let fb = "rtmps://user:pw@live.example.com:443/rtmp/FB-123?s_bl=1&a=b";
    assert_eq!(
        redact_url(fb),
        "rtmps://<redacted>@live.example.com:443/rtmp/<redacted>"
    );
}

#[test]
fn redaction_edge_cases() {
    // No key segment: nothing to hide.
    assert_eq!(redact_url("rtmp://host/app"), "rtmp://host/app");
    assert_eq!(redact("text", None), "text");
    // A one-letter key is hidden inside the URL but not elsewhere.
    let url = "rtmp://host/app/k";
    assert_eq!(
        redact("open rtmp://host/app/k: kaput", Some(url)),
        "open rtmp://host/app/<redacted>: kaput"
    );
    let c = OutputConfig {
        rtmp_url: Some("rtmp://host/app/secret-key".into()),
        ..config()
    };
    let debug = format!("{c:?}");
    assert!(!debug.contains("secret-key"), "{debug}");
    assert!(debug.contains("<redacted>"));
}

#[test]
fn parses_encoder_listing() {
    assert!(encoder_listed(LISTING, "libx264"));
    assert!(encoder_listed(LISTING, "h264_videotoolbox"));
    // Audio encoders and the legend do not count as video encoders.
    assert!(!encoder_listed(LISTING, "aac"));
    assert!(!encoder_listed(LISTING, "="));
    assert!(!encoder_listed("", "libx264"));
    assert!(!encoder_listed(LISTING, "libx26"));
}

#[test]
fn auto_prefers_videotoolbox_on_macos_only() {
    let c = |req, listing, mac| choose_encoder(req, listing, mac).unwrap();
    assert_eq!(c(Encoder::Auto, LISTING, true), EncoderChoice::VideoToolbox);
    assert_eq!(c(Encoder::Auto, LISTING, false), EncoderChoice::Libx264);
    let x264_only = " V....D libx264 libx264 H.264\n";
    assert_eq!(c(Encoder::Auto, x264_only, true), EncoderChoice::Libx264);
    assert_eq!(c(Encoder::Libx264, LISTING, true), EncoderChoice::Libx264);
    assert_eq!(
        c(Encoder::VideoToolbox, LISTING, false),
        EncoderChoice::VideoToolbox
    );
}

#[test]
fn auto_falls_back_to_x264_when_videotoolbox_cannot_encode() {
    let pick = |req, works: bool| {
        let mut asked = false;
        let choice = choose_working_encoder(req, LISTING, true, || {
            asked = true;
            works
        });
        (choice.unwrap(), asked)
    };
    assert_eq!(
        pick(Encoder::Auto, true),
        (EncoderChoice::VideoToolbox, true)
    );
    assert_eq!(pick(Encoder::Auto, false), (EncoderChoice::Libx264, true));
    // An explicit request is used as is; libx264 needs no test encode.
    assert_eq!(
        pick(Encoder::VideoToolbox, false),
        (EncoderChoice::VideoToolbox, false)
    );
    assert_eq!(
        pick(Encoder::Libx264, false),
        (EncoderChoice::Libx264, false)
    );
    let off_mac = choose_working_encoder(Encoder::Auto, LISTING, false, || unreachable!());
    assert_eq!(off_mac.unwrap(), EncoderChoice::Libx264);
    // Without libx264 to fall back on, the failure is reported.
    let vt_only = " V....D h264_videotoolbox VideoToolbox H.264 Encoder\n";
    assert!(matches!(
        choose_working_encoder(Encoder::Auto, vt_only, true, || false),
        Err(OutputError::EncoderUnavailable(name)) if name == "libx264"
    ));
}

#[test]
fn missing_encoders_are_errors() {
    let x264_only = " V....D libx264 libx264 H.264\n";
    assert!(matches!(
        choose_encoder(Encoder::VideoToolbox, x264_only, true),
        Err(OutputError::EncoderUnavailable(name)) if name == "h264_videotoolbox"
    ));
    assert!(matches!(
        choose_encoder(Encoder::Auto, "", true),
        Err(OutputError::EncoderUnavailable(name)) if name == "libx264"
    ));
}
