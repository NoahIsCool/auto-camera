//! Tests of the audio inputs that need no ffmpeg: the config, the fake's
//! schedule (drift, stalls, unplugging), the fake as a running input, the
//! supervisor, and reading an audio-only file's probe. The file input
//! itself is tested against ffmpeg in `tests/audio_input.rs`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::clock::{VirtualClock, WallClock};
use crate::probe::parse_audio_probe;
use ac_media::{AudioBlock, SourceState};

/// Shorthand for milliseconds.
fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

/// The id every test input has.
fn board() -> SourceId {
    SourceId("board".into())
}

/// A feed of `plan` as [`board`].
fn feed(plan: FakeAudio) -> FakeFeed {
    FakeFeed::new(board(), plan).unwrap()
}

/// The sequence numbers of `blocks`.
fn seqs(blocks: &[AudioBlock]) -> Vec<u64> {
    blocks.iter().map(|b| b.seq).collect()
}

/// Polls `cond` every 2 ms for up to 10 s.
fn eventually(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(ms(2));
    }
    false
}

#[test]
fn a_file_config_fills_defaults_and_round_trips() {
    let c: AudioInputConfig =
        serde_json::from_str(r#"{"kind": "file", "path": "board.wav"}"#).unwrap();
    assert_eq!(
        c,
        AudioInputConfig::File {
            path: "board.wav".into(),
            loop_playback: false,
        }
    );
    let looping = AudioInputConfig::File {
        path: "/media/board.rf64".into(),
        loop_playback: true,
    };
    let json = serde_json::to_string(&looping).unwrap();
    assert!(json.contains(r#""kind":"file""#), "{json}");
    assert_eq!(
        serde_json::from_str::<AudioInputConfig>(&json).unwrap(),
        looping
    );
    let mut c = looping;
    assert_eq!(c.path(), Some(Path::new("/media/board.rf64")));
    *c.path_mut().unwrap() = "other.wav".into();
    assert_eq!(c.path(), Some(Path::new("other.wav")));
}

#[test]
fn malformed_audio_input_configs_are_rejected() {
    for json in [
        r#"{"kind": "device", "path": "x"}"#,
        r#"{"kind": "file"}"#,
        r#"{"kind": "file", "path": "a.wav", "loop": true}"#,
        r#"{"path": "a.wav"}"#,
    ] {
        assert!(
            serde_json::from_str::<AudioInputConfig>(json).is_err(),
            "{json}"
        );
    }
}

#[test]
fn opening_a_missing_file_says_which() {
    let config = AudioInputConfig::File {
        path: "/nonexistent/auto-camera/board.wav".into(),
        loop_playback: false,
    };
    match open_audio_input(&config, board(), Arc::new(VirtualClock::new())) {
        Err(MediaError::Open(msg)) => assert!(msg.contains("board.wav"), "{msg}"),
        other => panic!("expected an open error, got {:?}", other.is_ok()),
    }
}

#[test]
fn the_fake_signal_differs_on_every_channel_and_fits_sixteen_bits() {
    for n in [0, 1, 4_800, 65_535, 1 << 40] {
        let frame: Vec<f32> = (0..16).map(|c| fake_sample(c, n)).collect();
        for (i, a) in frame.iter().enumerate() {
            assert!((-1.0..1.0).contains(a), "{a}");
            assert_eq!(a * 32_768.0, (a * 32_768.0).round(), "exact in 16 bits");
            assert!(
                frame[i + 1..].iter().all(|b| b != a),
                "frame {n}: {frame:?}"
            );
        }
    }
    assert_ne!(fake_sample(3, 10), fake_sample(3, 11));
    assert_eq!(fake_sample(3, 10), fake_sample(3, 10 + 65_536));
}

#[test]
fn a_steady_feed_delivers_contiguous_blocks_of_the_fake_signal() {
    let mut f = feed(FakeAudio::steady(48_000, 3));
    assert_eq!(f.next_due(), Some(ms(10)));
    assert!(f.until(ms(9)).is_empty(), "the first block is not full yet");
    let blocks = f.until(Duration::from_secs(1));
    assert_eq!(seqs(&blocks), (0..100).collect::<Vec<_>>());
    for (k, b) in blocks.iter().enumerate() {
        assert_eq!(
            (b.source.clone(), b.sample_rate, b.channels),
            (board(), 48_000, 3)
        );
        assert_eq!(b.frames(), 480);
        assert_eq!(b.pts, f.time_of(k as u64 * 480));
        let first = k as u64 * 480;
        assert_eq!(b.samples[0], fake_sample(0, first));
        assert_eq!(b.samples[3 * 479 + 2], fake_sample(2, first + 479));
    }
    assert!(blocks.windows(2).all(|w| w[0].pts < w[1].pts));
    assert_eq!(f.next_due(), Some(Duration::from_millis(1010)));
    assert_eq!(f.state(), FeedState::Live);
}

#[test]
fn drift_delivers_more_or_fewer_frames_than_the_clock_asks() {
    for (ppm, blocks) in [(1000.0, 1001), (-1000.0, 999), (0.0, 1000)] {
        let mut f = feed(FakeAudio {
            drift_ppm: ppm,
            ..FakeAudio::steady(48_000, 2)
        });
        let got = f.until(Duration::from_secs(10));
        assert_eq!(got.len(), blocks, "{ppm} ppm");
        // PTS is when each block's first frame was captured, on the clock.
        let last = got.last().unwrap();
        let want = (blocks - 1) as f64 * 480.0 / (48_000.0 * (1.0 + ppm * 1e-6));
        assert!((last.pts.as_secs_f64() - want).abs() < 1e-9, "{ppm} ppm");
        assert!(got.iter().all(|b| b.sample_rate == 48_000), "nominal rate");
    }
}

#[test]
fn a_held_stall_delivers_its_blocks_in_a_burst_at_its_end() {
    let mut f = feed(FakeAudio {
        gaps: vec![Gap {
            from: ms(1000),
            until: ms(1500),
            held: true,
        }],
        ..FakeAudio::steady(48_000, 1)
    });
    // Blocks completing before the stall; the one completing at 1 s is in it.
    assert_eq!(f.until(ms(1000)).len(), 99);
    assert_eq!(f.next_due(), Some(ms(1500)), "nothing until the stall ends");
    assert!(f.until(ms(1499)).is_empty());
    let burst = f.until(ms(1500));
    assert_eq!(seqs(&burst), (99..150).collect::<Vec<_>>());
    // Each keeps the PTS of its capture.
    assert_eq!(burst[0].pts, f.time_of(99 * 480));
    assert_eq!(burst.last().unwrap().pts, f.time_of(149 * 480));
    assert_eq!(f.until(ms(1510))[0].seq, 150, "then steady again");
}

#[test]
fn a_lost_stall_leaves_a_gap_in_numbers_and_time() {
    let mut f = feed(FakeAudio {
        gaps: vec![Gap {
            from: ms(1000),
            until: ms(1500),
            held: false,
        }],
        ..FakeAudio::steady(48_000, 1)
    });
    let blocks = f.until(Duration::from_secs(2));
    let s = seqs(&blocks);
    assert_eq!(&s[..99], (0..99).collect::<Vec<_>>());
    // Blocks 99 to 148 completed inside the stall and are gone.
    assert_eq!(&s[99..], (149..200).collect::<Vec<_>>());
    assert_eq!(blocks[99].pts, ms(1490));
    assert!(blocks.windows(2).all(|w| w[0].pts < w[1].pts));
}

#[test]
fn an_unplugged_or_ended_feed_stops_at_its_time() {
    let mut f = feed(FakeAudio {
        fail_at: Some(ms(1000)),
        end_at: Some(ms(2000)),
        ..FakeAudio::steady(48_000, 1)
    });
    assert_eq!(f.until(ms(999)).len(), 99);
    assert_eq!(f.next_due(), Some(ms(1000)), "the failure comes first");
    // The block completing at the failure itself is not delivered.
    assert_eq!(f.until(Duration::from_secs(5)).len(), 0);
    assert_eq!(f.state(), FeedState::Unplugged);
    assert_eq!((f.next_due(), f.until(Duration::MAX).len()), (None, 0));
    let mut ends = feed(FakeAudio {
        end_at: Some(ms(505)),
        ..FakeAudio::steady(48_000, 1)
    });
    assert_eq!(ends.until(Duration::from_secs(5)).len(), 50);
    assert_eq!(ends.state(), FeedState::Ended);
}

#[test]
fn an_unplayable_plan_is_refused() {
    let steady = FakeAudio::steady(48_000, 2);
    let cases = [
        ("zero rate", FakeAudio::steady(0, 2)),
        ("zero channels", FakeAudio::steady(48_000, 0)),
        (
            "zero block",
            FakeAudio {
                block_frames: 0,
                ..steady.clone()
            },
        ),
        (
            "NaN drift",
            FakeAudio {
                drift_ppm: f64::NAN,
                ..steady.clone()
            },
        ),
        (
            "a fifth fast",
            FakeAudio {
                drift_ppm: 200_000.0,
                ..steady.clone()
            },
        ),
        (
            "inverted gap",
            FakeAudio {
                gaps: vec![Gap {
                    from: ms(5),
                    until: ms(5),
                    held: true,
                }],
                ..steady.clone()
            },
        ),
    ];
    for (what, plan) in cases {
        assert!(plan.check().is_err(), "{what}");
        assert!(FakeFeed::new(board(), plan).is_err(), "{what}");
    }
    assert!(steady.check().is_ok());
    assert_eq!(FakeAudio::steady(50, 1).block_frames, 1);
}

#[test]
fn a_fake_input_plays_its_feed_and_ends_or_fails_on_plan() {
    let clock = Arc::new(VirtualClock::new());
    let plan = FakeAudio {
        end_at: Some(ms(1000)),
        ..FakeAudio::steady(48_000, 2)
    };
    let mut input = FakeAudioInput::start(board(), plan, clock.clone()).unwrap();
    assert_eq!(input.info().format, Some((48_000, 2)));
    let mut got = Vec::new();
    assert!(eventually(|| {
        input.wait(ms(1));
        got.extend(input.drain());
        input.state() == SourceState::Stopped
    }));
    got.extend(input.drain());
    assert_eq!(seqs(&got), (0..99).collect::<Vec<_>>());
    assert_eq!((input.error(), input.dropped()), (None, 0));
    input.stop();

    let plan = FakeAudio {
        fail_at: Some(ms(200)),
        ..FakeAudio::steady(48_000, 2)
    };
    let input = FakeAudioInput::start(board(), plan, clock).unwrap();
    assert!(eventually(|| input.state() == SourceState::Stopped));
    assert!(input.error().unwrap().contains("unplugged"));
    assert_eq!(input.drain().len(), 19);
}

#[test]
fn stopping_a_live_fake_input_is_prompt() {
    let mut input = FakeAudioInput::start(
        board(),
        FakeAudio::steady(48_000, 2),
        Arc::new(WallClock::new()),
    )
    .unwrap();
    assert!(eventually(|| input.state() == SourceState::Running));
    let started = Instant::now();
    input.stop();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(input.state(), SourceState::Stopped);
}

/// The placeholder of an input that has not opened.
fn offline() -> AudioInputInfo {
    AudioInputInfo {
        id: board(),
        format: None,
    }
}

#[test]
fn a_supervised_input_missing_at_start_goes_on_air_when_it_opens() {
    let clock = Arc::new(VirtualClock::new());
    let attempts = Arc::new(AtomicU32::new(0));
    let (tries, pacing) = (attempts.clone(), clock.clone());
    let opener = move || -> Result<Box<dyn AudioInput>, MediaError> {
        if tries.fetch_add(1, Ordering::SeqCst) < 2 {
            return Err(MediaError::Open("board.wav does not exist".into()));
        }
        let input = FakeAudioInput::start(board(), FakeAudio::steady(44_100, 16), pacing.clone())?;
        Ok(Box::new(input))
    };
    let mut input = SupervisedAudio::start(opener, offline(), clock, Backoff::default()).unwrap();
    assert_eq!(
        input.start_error(),
        Some("cannot open source: board.wav does not exist")
    );
    assert_eq!(input.info(), &offline(), "the caller's description");
    let mut got = Vec::new();
    assert!(eventually(|| {
        got.extend(input.drain());
        got.len() >= 10
    }));
    assert_eq!(input.state(), SourceState::Running);
    assert_eq!(input.error(), None);
    assert_eq!(input.current_info().format, Some((44_100, 16)), "learned");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    // Tried 0.5 s and 1.5 s after the start: the first block is stamped
    // there, and the numbers start at 0.
    assert_eq!(got[0].seq, 0);
    assert!(got[0].pts >= ms(1500), "{:?}", got[0].pts);
    assert!(
        got.windows(2)
            .all(|w| w[1].seq == w[0].seq + 1 && w[0].pts < w[1].pts)
    );
    input.stop();
    assert_eq!(input.state(), SourceState::Stopped);
}

#[test]
fn an_unplugged_input_is_reopened_and_its_blocks_carry_on() {
    let clock = Arc::new(VirtualClock::new());
    let attempts = Arc::new(AtomicU32::new(0));
    let (tries, pacing) = (attempts.clone(), clock.clone());
    let opener = move || -> Result<Box<dyn AudioInput>, MediaError> {
        // Unplugged after 1 s the first time; the second connection plays
        // 1 s and ends.
        let plan = match tries.fetch_add(1, Ordering::SeqCst) {
            0 => FakeAudio {
                fail_at: Some(ms(1000)),
                ..FakeAudio::steady(48_000, 2)
            },
            _ => FakeAudio {
                end_at: Some(ms(1000)),
                ..FakeAudio::steady(48_000, 2)
            },
        };
        Ok(Box::new(FakeAudioInput::start(
            board(),
            plan,
            pacing.clone(),
        )?))
    };
    let mut input = SupervisedAudio::start(opener, offline(), clock, Backoff::default()).unwrap();
    assert_eq!(input.start_error(), None);
    assert_eq!(input.info().format, Some((48_000, 2)), "the input's own");
    let mut got = Vec::new();
    assert!(eventually(|| {
        got.extend(input.drain());
        input.state() == SourceState::Stopped
    }));
    got.extend(input.drain());
    // 99 blocks before the fault, 99 after: numbered on, never repeated.
    assert_eq!(seqs(&got), (0..198).collect::<Vec<_>>());
    let (before, after) = (&got[98], &got[99]);
    assert!(
        after.pts > before.pts + before.duration(),
        "time away counts"
    );
    assert!(after.pts >= ms(1500), "reopened after the backoff");
    assert!(got.windows(2).all(|w| w[0].pts < w[1].pts));
    // A connection that ends cleanly is not reopened.
    assert_eq!(input.error(), None);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(input.dropped(), 0);
    input.stop();
}

#[test]
fn a_refused_or_failing_input_waits_out_its_backoff_and_stops_at_once() {
    let opener = || -> Result<Box<dyn AudioInput>, MediaError> {
        Err(MediaError::Open("channels 3 and 4 need 4 channels".into()))
    };
    let slow = Backoff {
        initial: Duration::from_secs(30),
        max: Duration::from_secs(60),
    };
    let mut input =
        SupervisedAudio::start(opener, offline(), Arc::new(WallClock::new()), slow).unwrap();
    assert_eq!(input.state(), SourceState::Offline);
    assert!(input.error().unwrap().contains("channels 3 and 4"));
    assert!(input.drain().is_empty());
    let started = Instant::now();
    input.stop();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "not after the delay"
    );
    assert_eq!(input.state(), SourceState::Stopped);
    let bad = Backoff {
        initial: Duration::ZERO,
        max: Duration::ZERO,
    };
    assert!(SupervisedAudio::start(opener, offline(), Arc::new(WallClock::new()), bad).is_err());
}

/// Trimmed ffprobe output for a 16-channel 48 kHz WAV.
const BOARD_WAV: &str = r#"{
    "streams": [
        {"index": 0, "codec_name": "pcm_s16le", "codec_type": "audio",
         "sample_rate": "48000", "channels": 16, "r_frame_rate": "0/0"}
    ],
    "format": {"filename": "board.wav", "format_name": "wav", "duration": "0.100000"}
}"#;

#[test]
fn an_audio_only_probe_reads_the_first_audio_stream() {
    let s = parse_audio_probe(BOARD_WAV).unwrap();
    assert_eq!((s.index, s.sample_rate, s.channels), (0, 48_000, 16));
    // A file with a picture too: its audio stream, wherever it is.
    let clip = r#"{"streams": [
        {"index": 0, "codec_type": "video", "width": 160, "height": 120},
        {"index": 1, "codec_type": "audio", "sample_rate": "44100", "channels": 1}
    ]}"#;
    assert_eq!(parse_audio_probe(clip).unwrap().index, 1);
    for (json, why) in [
        (
            r#"{"streams": [{"index": 0, "codec_type": "video"}]}"#,
            "no audio stream",
        ),
        (r#"{"streams": []}"#, "no audio stream"),
        (
            r#"{"streams": [{"index": 0, "codec_type": "audio", "channels": 2}]}"#,
            "no sample rate",
        ),
        ("not json", "unreadable"),
    ] {
        let e = parse_audio_probe(json).unwrap_err().to_string();
        assert!(e.contains(why), "{json}: {e}");
    }
}
