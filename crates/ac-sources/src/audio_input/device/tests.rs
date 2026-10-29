//! Tests of the device input's pure parts: the `kind = "device"` config,
//! selection by UID on a fake device table, the callback's hand-off, the
//! stamper's PTS and discontinuities, and the log's quiet time. The input
//! itself, driven through a fake backend, is tested in [`input`].

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::input::{LOG_QUIET, QuietLog};
use super::*;
use crate::audio_input::fake::fake_sample;
use crate::audio_input::{AudioInputConfig, open_audio_input};
use crate::clock::VirtualClock;
use ac_core::SourceId;

mod input;

/// Shorthand for milliseconds.
fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

/// A device with inputs.
fn device(uid: &str, name: &str, channels: u16, default_input: bool) -> AudioDevice {
    AudioDevice {
        uid: uid.into(),
        name: name.into(),
        input_channels: channels,
        sample_rate: 48_000,
        default_input,
    }
}

/// A Mac with the built-in microphone (the default input), the board's
/// USB interface, a Dante Virtual Soundcard and the built-in speakers.
fn table() -> Vec<AudioDevice> {
    vec![
        device("BuiltInMicrophoneDevice", "MacBook Pro Microphone", 1, true),
        device("AppleUSBAudioEngine:Allen&Heath:SQ:1A2B:1", "SQ", 32, false),
        device(
            "com.audinate.dante.DVS:0",
            "Dante Virtual Soundcard",
            64,
            false,
        ),
        AudioDevice {
            input_channels: 0,
            ..device("BuiltInSpeakerDevice", "MacBook Pro Speakers", 0, false)
        },
    ]
}

/// The open error's message of `r`.
fn open_error<T>(r: Result<T, MediaError>) -> String {
    match r {
        Err(MediaError::Open(m)) => m,
        Err(other) => panic!("expected an open error, got {other}"),
        Ok(_) => panic!("expected an open error, got a value"),
    }
}

/// `n` frames of the fake signal on `channels` channels, from frame
/// `first`.
fn frames(first: u64, n: u64, channels: u16) -> Vec<f32> {
    (first..first + n)
        .flat_map(|f| (0..channels).map(move |c| fake_sample(c, f)))
        .collect()
}

/// A captured block of `frames` stereo frames at `host`, nothing lost
/// before it.
fn captured(host: Duration, frames: usize) -> Captured {
    Captured {
        host_time: host,
        samples: vec![0.0; frames * 2],
        lost_blocks: 0,
        lost_frames: 0,
    }
}

#[test]
fn a_device_config_names_a_uid_and_round_trips() {
    let c: AudioInputConfig =
        serde_json::from_str(r#"{"kind": "device", "uid": "AppleUSBAudioEngine:SQ:1"}"#).unwrap();
    assert_eq!(
        c,
        AudioInputConfig::Device {
            uid: "AppleUSBAudioEngine:SQ:1".into()
        }
    );
    assert_eq!(c.device_uid(), Some("AppleUSBAudioEngine:SQ:1"));
    assert_eq!((c.path(), c.problem()), (None, None));
    let json = serde_json::to_string(&c).unwrap();
    assert_eq!(
        json,
        r#"{"kind":"device","uid":"AppleUSBAudioEngine:SQ:1"}"#
    );
    let mut c = c;
    assert!(c.path_mut().is_none(), "no path to resolve");
    for bad in [
        r#"{"kind": "device"}"#,
        r#"{"kind": "device", "uid": "x", "path": "board.wav"}"#,
        r#"{"kind": "device", "uid": 7}"#,
        r#"{"kind": "coreaudio", "uid": "x"}"#,
    ] {
        assert!(
            serde_json::from_str::<AudioInputConfig>(bad).is_err(),
            "{bad}"
        );
    }
    let blank = AudioInputConfig::Device { uid: "  ".into() };
    assert!(blank.problem().unwrap().contains("--list-audio-devices"));
    let file = AudioInputConfig::File {
        path: "board.wav".into(),
        loop_playback: false,
    };
    assert_eq!((file.device_uid(), file.problem()), (None, None));
}

#[test]
fn the_board_is_selected_by_its_exact_uid() {
    let devices = table();
    let board = select(&devices, "AppleUSBAudioEngine:Allen&Heath:SQ:1A2B:1").unwrap();
    assert_eq!((board.name.as_str(), board.input_channels), ("SQ", 32));
    let dante = select(&devices, "com.audinate.dante.DVS:0").unwrap();
    assert_eq!(dante.input_channels, 64);
    // Close is not the same: no prefix, case or whitespace matching.
    for near in [
        "AppleUSBAudioEngine:Allen&Heath:SQ:1A2B",
        "appleusbaudioengine:allen&heath:sq:1a2b:1",
        " AppleUSBAudioEngine:Allen&Heath:SQ:1A2B:1",
        "SQ",
    ] {
        assert!(select(&devices, near).is_err(), "{near}");
    }
}

#[test]
fn a_missing_uid_is_an_error_naming_the_devices_with_no_fallback() {
    let devices = table();
    let message = open_error(select(
        &devices,
        "AppleUSBAudioEngine:Allen&Heath:SQ:9999:1",
    ));
    assert!(
        message.starts_with(
            "no audio device with UID \"AppleUSBAudioEngine:Allen&Heath:SQ:9999:1\" is connected \
             (no other device is opened in its place)"
        ),
        "{message}"
    );
    // Every input device is named with its UID, the default one too, and
    // the output-only one is not.
    for listed in [
        "\"BuiltInMicrophoneDevice\" (MacBook Pro Microphone, 1 input channel at 48000 Hz)",
        "\"AppleUSBAudioEngine:Allen&Heath:SQ:1A2B:1\" (SQ, 32 input channels at 48000 Hz)",
        "\"com.audinate.dante.DVS:0\" (Dante Virtual Soundcard, 64 input channels",
    ] {
        assert!(message.contains(listed), "{message}");
    }
    assert!(!message.contains("BuiltInSpeakerDevice"), "{message}");
    // With only the default microphone left, it is still not chosen.
    let mic_only = vec![device("BuiltInMicrophoneDevice", "Mic", 1, true)];
    assert!(select(&mic_only, "AppleUSBAudioEngine:SQ:1").is_err());
    let none = open_error(select(&[], "AppleUSBAudioEngine:SQ:1"));
    assert!(none.ends_with("; no input device is connected"), "{none}");
}

#[test]
fn unusable_devices_and_uids_are_refused() {
    let devices = table();
    let speakers = open_error(select(&devices, "BuiltInSpeakerDevice"));
    assert_eq!(
        speakers,
        "audio device \"BuiltInSpeakerDevice\" (MacBook Pro Speakers) has no input channels"
    );
    for blank in ["", "   "] {
        assert_eq!(
            open_error(select(&devices, blank)),
            "an audio device's UID cannot be empty"
        );
    }
    let twins = vec![device("dup", "A", 2, false), device("dup", "B", 2, false)];
    assert_eq!(
        open_error(select(&twins, "dup")),
        "2 audio devices report the UID \"dup\"; unplug all but one"
    );
    let rateless = vec![AudioDevice {
        sample_rate: 0,
        ..device("odd", "Odd", 2, false)
    }];
    assert!(open_error(select(&rateless, "odd")).contains("reports no sample rate"));
    assert_eq!(inputs(table()).len(), 3, "the speakers are left out");
}

#[test]
fn opening_a_device_in_this_build_says_how_to_build_with_coreaudio() {
    // Linux, or macOS without the feature: the config parses, and opening
    // it is an error saying what to do.
    if SUPPORTED {
        return;
    }
    let config = AudioInputConfig::Device {
        uid: "AppleUSBAudioEngine:SQ:1".into(),
    };
    let message = open_error(open_audio_input(
        &config,
        SourceId("board".into()),
        std::sync::Arc::new(VirtualClock::new()),
    ));
    assert_eq!(message, UNSUPPORTED);
    assert!(message.contains("--features coreaudio"), "{message}");
    assert!(SystemDevices.devices().is_err());
}

#[test]
fn the_handoff_carries_blocks_and_recycles_its_buffers() {
    let (mut sink, reader) = handoff(2, 48_000, 480, 4);
    assert!(reader.take().is_none());
    sink.deliver(ms(1000), &frames(0, 480, 2));
    let block = reader.take().unwrap();
    assert_eq!(block.host_time, ms(1000));
    assert_eq!(block.samples, frames(0, 480, 2));
    assert_eq!((block.lost_blocks, block.lost_frames), (0, 0));
    reader.recycle(block);
    // Many more blocks than buffers, each taken in turn: only the pool's
    // own buffers ever carry samples, at their first capacity.
    let mut seen = HashSet::new();
    for k in 1..100u64 {
        sink.deliver(ms(1000 + 10 * k), &frames(480 * k, 480, 2));
        let block = reader.take().unwrap();
        assert_eq!(block.samples[0], fake_sample(0, 480 * k));
        assert_eq!(block.samples.capacity(), 960);
        seen.insert(block.samples.as_ptr() as usize);
        reader.recycle(block);
    }
    assert!(seen.len() <= 4, "{} buffers", seen.len());
    assert_eq!(reader.lost(), 0);
}

#[test]
fn a_long_callback_buffer_is_cut_into_blocks_with_their_own_host_times() {
    let (mut sink, reader) = handoff(2, 48_000, 480, 8);
    // 1200 frames and half a frame: 480 + 480 + 240, the half frame left.
    let mut samples = frames(0, 1200, 2);
    samples.push(0.5);
    sink.deliver(ms(500), &samples);
    let blocks: Vec<Captured> = std::iter::from_fn(|| reader.take()).collect();
    let sizes: Vec<usize> = blocks.iter().map(|b| b.samples.len() / 2).collect();
    assert_eq!(sizes, [480, 480, 240]);
    let times: Vec<Duration> = blocks.iter().map(|b| b.host_time).collect();
    assert_eq!(times, [ms(500), ms(510), ms(520)]);
    assert_eq!(blocks[2].samples[0], fake_sample(0, 960));
    // Nothing at all: no block.
    sink.deliver(ms(600), &[]);
    sink.deliver(ms(600), &[0.25]);
    assert!(reader.take().is_none());
}

#[test]
fn a_full_pool_drops_blocks_and_the_next_one_says_so() {
    let (mut sink, reader) = handoff(2, 48_000, 480, 2);
    for k in 0..5u64 {
        sink.deliver(ms(10 * k), &frames(480 * k, 480, 2));
    }
    assert_eq!(reader.lost(), 3, "two buffers, five blocks");
    let (a, b) = (reader.take().unwrap(), reader.take().unwrap());
    assert!(reader.take().is_none());
    assert_eq!((a.host_time, b.host_time), (ms(0), ms(10)));
    reader.recycle(a);
    reader.recycle(b);
    sink.deliver(ms(50), &frames(2400, 240, 2));
    let next = reader.take().unwrap();
    assert_eq!((next.lost_blocks, next.lost_frames), (3, 1440));
    reader.recycle(next);
    sink.deliver(ms(55), &frames(2640, 240, 2));
    assert_eq!(reader.take().unwrap().lost_blocks, 0, "counted once");
}

#[test]
fn a_sink_whose_reader_is_gone_keeps_its_buffer_and_counts() {
    let (mut sink, reader) = handoff(2, 48_000, 480, 1);
    let lost = reader.lost_counter();
    drop(reader);
    for k in 0..3u64 {
        sink.deliver(ms(10 * k), &frames(0, 480, 2));
    }
    assert_eq!(lost.load(std::sync::atomic::Ordering::Relaxed), 3);
}

#[test]
fn stream_events_count_overloads_and_keep_the_first_loss() {
    let events = StreamEvents::new();
    let theirs = events.clone();
    assert_eq!((events.overloads(), events.lost_reason()), (0, None));
    theirs.overload();
    theirs.overload();
    theirs.lost("Device disconnected");
    theirs.lost("Device sample rate changed");
    assert_eq!(events.overloads(), 2);
    assert_eq!(events.lost_reason(), Some("Device disconnected"));
}

#[test]
fn contiguous_blocks_get_numbers_and_their_capture_time_as_pts() {
    let mut s = Stamper::new(SourceId("board".into()), 48_000, 2, ms(1000));
    for k in 0..50u64 {
        let (block, found) = s.stamp(&captured(ms(1000 + 10 * k), 480));
        assert!(found.is_empty(), "{found:?}");
        assert_eq!((block.seq, block.pts), (k, ms(10 * k)));
        assert_eq!((block.sample_rate, block.channels), (48_000, 2));
        assert_eq!(block.frames(), 480);
    }
    assert_eq!(s.next_seq(), 50);
}

#[test]
fn a_drifting_device_keeps_its_host_times_and_reports_nothing() {
    // 1000 ppm fast: 480 frames take 479.5 frames' time on the host clock.
    let step = Duration::from_secs_f64(480.0 / 48_048.0);
    let mut s = Stamper::new(SourceId("board".into()), 48_000, 2, Duration::ZERO);
    let mut last = Duration::ZERO;
    for k in 0..1000u32 {
        let (block, found) = s.stamp(&captured(step * k, 480));
        assert!(found.is_empty(), "block {k}: {found:?}");
        assert_eq!(block.pts, step * k, "the host time, not the sample count");
        last = block.pts;
    }
    assert!(
        last < ms(9990),
        "{last:?}: PTS advance slower than the samples"
    );
}

#[test]
fn a_block_captured_before_the_open_starts_pts_at_zero() {
    let mut s = Stamper::new(SourceId("board".into()), 48_000, 2, ms(1000));
    let (first, found) = s.stamp(&captured(ms(993), 480));
    assert!(found.is_empty());
    assert_eq!(first.pts, Duration::ZERO);
    let (second, _) = s.stamp(&captured(ms(1003), 480));
    assert_eq!(second.pts, ms(10), "contiguous from the moved origin");
}

#[test]
fn jitter_within_tolerance_is_not_a_jump_and_pts_never_go_back() {
    let mut s = Stamper::new(SourceId("board".into()), 48_000, 2, Duration::ZERO);
    let (_, found) = s.stamp(&captured(ms(0), 480));
    assert!(found.is_empty());
    // Due at 10 ms; half a block (5 ms) either way is jitter.
    let (late, found) = s.stamp(&captured(Duration::from_micros(14_900), 480));
    assert!(found.is_empty(), "{found:?}");
    let (early, found) = s.stamp(&captured(Duration::from_micros(20_000), 480));
    assert!(found.is_empty(), "{found:?}");
    assert!(early.pts >= late.pts);
    // A tiny block's tolerance is still a millisecond.
    let mut t = Stamper::new(SourceId("board".into()), 48_000, 2, Duration::ZERO);
    t.stamp(&captured(ms(0), 4));
    let (_, found) = t.stamp(&captured(Duration::from_micros(900), 4));
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_host_time_jump_ahead_is_a_gap_kept_in_pts() {
    let mut s = Stamper::new(SourceId("board".into()), 48_000, 2, Duration::ZERO);
    s.stamp(&captured(ms(0), 480));
    s.stamp(&captured(ms(10), 480));
    // Due at 20 ms, captured at 41.3 ms: 21.3 ms missing.
    let (block, found) = s.stamp(&captured(Duration::from_micros(41_300), 480));
    assert_eq!(
        found,
        [Discontinuity::Gap {
            seq: 2,
            by: Duration::from_micros(21_300)
        }]
    );
    assert_eq!(block.seq, 2, "nothing was dropped on the way");
    assert_eq!(block.pts, Duration::from_micros(41_300));
    assert_eq!(
        found[0].to_string(),
        "21.3 ms of audio missing before block 2 (its host time jumped ahead)"
    );
}

#[test]
fn a_host_time_rewind_moves_the_map_so_pts_carry_on() {
    let mut s = Stamper::new(SourceId("board".into()), 48_000, 2, Duration::ZERO);
    s.stamp(&captured(ms(1000), 480));
    s.stamp(&captured(ms(1010), 480));
    // Due at 1020 ms, stamped 400 ms: the timestamps went back 620 ms.
    let (block, found) = s.stamp(&captured(ms(400), 480));
    assert_eq!(
        found,
        [Discontinuity::Rewind {
            seq: 2,
            by: ms(620)
        }]
    );
    assert_eq!(block.pts, ms(1020), "where it was due");
    let (next, found) = s.stamp(&captured(ms(410), 480));
    assert!(found.is_empty(), "the map stays moved: {found:?}");
    assert_eq!(next.pts, ms(1030));
    assert_eq!(found.len(), 0);
    assert_eq!(
        Discontinuity::Rewind {
            seq: 2,
            by: ms(620)
        }
        .to_string(),
        "host time went back 620.0 ms at block 2"
    );
}

#[test]
fn dropped_blocks_skip_their_numbers_and_their_time() {
    let mut s = Stamper::new(SourceId("board".into()), 48_000, 2, Duration::ZERO);
    s.stamp(&captured(ms(0), 480));
    // Three 10 ms blocks dropped; the next is captured where they ended.
    let after = Captured {
        lost_blocks: 3,
        lost_frames: 1440,
        ..captured(ms(40), 480)
    };
    let (block, found) = s.stamp(&after);
    let overflow = Discontinuity::Overflow {
        seq: 4,
        blocks: 3,
        frames: 1440,
        lasting: ms(30),
    };
    assert_eq!(found, std::slice::from_ref(&overflow), "no gap besides");
    assert_eq!((block.seq, block.pts), (4, ms(40)));
    assert_eq!(
        overflow.to_string(),
        "3 block(s) (1440 sample frames, 30.0 ms) dropped before block 4: \
         the input fell behind its device"
    );
    // Dropped blocks and a jump on top: both.
    let worse = Captured {
        lost_blocks: 1,
        lost_frames: 480,
        ..captured(ms(100), 480)
    };
    let (block, found) = s.stamp(&worse);
    assert_eq!(block.seq, 6);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(matches!(found[1], Discontinuity::Gap { by, .. } if by == ms(40)));
    assert!(
        Discontinuity::Overload
            .to_string()
            .contains("reported an overload")
    );
}

#[test]
fn the_log_takes_a_line_per_quiet_time_and_counts_the_rest() {
    let mut log = QuietLog::default();
    let t0 = Instant::now();
    assert_eq!(log.admit(t0), Some(0));
    assert_eq!(log.admit(t0 + ms(1)), None);
    assert_eq!(log.admit(t0 + LOG_QUIET - ms(1)), None);
    assert_eq!(log.admit(t0 + LOG_QUIET), Some(2), "two held back");
    assert_eq!(log.admit(t0 + LOG_QUIET * 3), Some(0));
}

#[test]
fn a_device_describes_itself_with_its_uid() {
    let d = device("BuiltInMicrophoneDevice", "MacBook Pro Microphone", 1, true);
    assert_eq!(
        d.to_string(),
        "\"BuiltInMicrophoneDevice\" (MacBook Pro Microphone, 1 input channel at 48000 Hz)"
    );
    assert!(d.is_input());
    assert!(!table()[3].is_input());
}
