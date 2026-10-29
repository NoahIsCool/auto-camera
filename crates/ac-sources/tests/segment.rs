//! Segments of a file against real ffmpeg: a file opened at `start_s`
//! starts at PTS 0 with the frame from that time, its audio with it, in a
//! live file source and in a clip reader, and stops after `duration_s`.
//!
//! The fixtures are made per test from ffmpeg's `lavfi` generators, so no
//! media lives in the repository. The clip is 10 fps, and every frame
//! encodes its own number in its brightness (frame `n`, at `n / 10` s, has
//! luma `16 + 5 (n mod 40)`); its audio is a ramp on which every sample is
//! a tenth of its own time in seconds, starting again every 4 s. It comes
//! in three files:
//!
//! * [`MKV`]: 4 s of MPEG-4 Part 2 with a single keyframe, so that seeking
//!   to 2 s must decode from the start and drop what comes before, and
//!   lossless PCM audio, so a sample tells the time it was taken from.
//! * [`TS`]: the same 4 s as an MPEG-TS recording (MPEG-2 video with a
//!   keyframe every second, MP2 audio). The container has no index, so
//!   ffmpeg's own seek lands near the target rather than on a keyframe
//!   before it, and its timestamps start at about 1.4 s.
//! * [`LONG_TS`]: 20 s of the TS, long enough that a segment near its end
//!   is reached by seeking rather than by decoding from the start.
//!
//! Every test skips (passes with a note) when ffmpeg is not installed,
//! unless `AC_REQUIRE_FFMPEG` is set (CI), which makes a missing tool a
//! failure.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{SourceState, VideoFrame};
use ac_sources::segment::Segment;
use ac_sources::{
    ClipOptions, ClipReader, FfmpegTools, SourceConfig, VirtualClock, open_source_with_clock,
};

/// Frames per second of the fixture.
const FPS: u64 = 10;

/// The tools, or `None` (and a note) when ffmpeg is not installed; panics
/// instead when `AC_REQUIRE_FFMPEG` is set.
fn tools() -> Option<FfmpegTools> {
    let tools = FfmpegTools::from_env();
    if tools.is_available() {
        Some(tools)
    } else {
        let required =
            std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
        assert!(!required, "ffmpeg/ffprobe are required (AC_REQUIRE_FFMPEG)");
        eprintln!("skipping: ffmpeg/ffprobe not found");
        None
    }
}

/// A temporary directory removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    /// A fresh directory unique to this process and `name`.
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ac-segment-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fixture file: see the module docs.
struct Fixture {
    /// File name; its extension picks the container.
    name: &'static str,
    /// Length in seconds.
    seconds: u32,
    /// Encoder options, video then audio.
    codecs: &'static [&'static str],
}

/// The clip in Matroska with a single keyframe and PCM audio.
const MKV: Fixture = Fixture {
    name: "numbered.mkv",
    seconds: 4,
    codecs: &[
        "-c:v",
        "mpeg4",
        "-q:v",
        "1",
        "-g",
        "1000",
        "-c:a",
        "pcm_s16le",
    ],
};

/// The clip as an MPEG-TS recording.
const TS: Fixture = Fixture {
    name: "numbered.ts",
    seconds: 4,
    codecs: &["-c:v", "mpeg2video", "-q:v", "1", "-g", "10", "-c:a", "mp2"],
};

/// A longer MPEG-TS recording.
const LONG_TS: Fixture = Fixture {
    name: "long.ts",
    seconds: 20,
    ..TS
};

/// Writes `fixture` (see the module docs) into `dir`.
fn make(tools: &FfmpegTools, dir: &Path, fixture: &Fixture) -> PathBuf {
    let path = dir.join(fixture.name);
    let d = fixture.seconds;
    let status = Command::new(&tools.ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!(
            "nullsrc=s=64x36:r={FPS}:d={d},geq=lum='16+5*mod(N,40)':cb=128:cr=128"
        ))
        .args(["-f", "lavfi", "-i"])
        .arg(format!(
            "aevalsrc=exprs='mod(t,4)/10|mod(t,4)/10':s=48000:d={d}"
        ))
        .args(fixture.codecs)
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make {}", fixture.name);
    path
}

/// [`MKV`] in `dir`.
fn make_clip(tools: &FfmpegTools, dir: &Path) -> PathBuf {
    make(tools, dir, &MKV)
}

/// The number of the fixture frame that `frame` shows, modulo 40, read
/// from its brightness.
fn number(frame: &VideoFrame) -> u64 {
    // Grey decodes to R = G = B = (Y - 16) * 255 / 219.
    let r = f64::from(frame.data[(frame.data.len() / 8) & !3]);
    (r * 219.0 / 255.0 / 5.0).round() as u64
}

/// Reads every frame of `reader` until the clip ends.
fn read_all(reader: &mut ClipReader) -> Vec<VideoFrame> {
    let mut frames = Vec::new();
    while let Some(f) = reader.next_frame().unwrap() {
        frames.push(f);
        assert!(frames.len() < 1000, "the clip does not end");
    }
    frames
}

/// Options for reading the segment `start_s`, `duration_s`.
fn segment(start_s: Option<f64>, duration_s: Option<f64>) -> ClipOptions {
    ClipOptions {
        segment: Segment::from_seconds(start_s, duration_s).unwrap(),
        ..ClipOptions::default()
    }
}

/// The fixture as a file source's config, cut to `start_s`, `duration_s`.
fn file_config(path: &Path, start_s: f64, duration_s: f64) -> SourceConfig {
    SourceConfig::File {
        path: path.to_owned(),
        loop_playback: false,
        max_width: None,
        audio: true,
        start_s: Some(start_s),
        duration_s: Some(duration_s),
    }
}

/// What a live source delivered of a segment.
struct Played {
    /// The last frame.
    last: VideoFrame,
    /// Audio samples per channel.
    samples: usize,
    /// The first audio sample of the first channel.
    first_sample: f32,
}

impl Played {
    /// The time in the fixture at which its first audio sample was taken
    /// (the ramp is a tenth of the time).
    fn audio_start(&self) -> f64 {
        f64::from(self.first_sample) * 10.0
    }
}

/// Plays `config` unpaced to its end.
fn play_to_end(config: &SourceConfig) -> Played {
    let clock = Arc::new(VirtualClock::new());
    let mut src = open_source_with_clock(config, SourceId("file".into()), clock).unwrap();
    let mut blocks = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while src.state() != SourceState::Stopped {
        blocks.extend(src.drain_audio());
        assert!(Instant::now() < deadline, "the segment did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(50));
    blocks.extend(src.drain_audio());
    let last = src.latest().expect("a frame was delivered");
    src.stop();
    let first = blocks.first().expect("audio was delivered");
    assert_eq!(first.pts, Duration::ZERO, "the audio starts at PTS 0");
    Played {
        last,
        samples: blocks.iter().map(|b| b.frames()).sum(),
        first_sample: first.samples[0],
    }
}

#[test]
fn a_file_opened_at_start_s_2_starts_at_pts_0_with_the_frame_from_2_s() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("start");
    let clip = make_clip(&tools, &dir.0);
    let id = SourceId("clip".into());

    // Read frame by frame: the frame from 2 s comes first, at PTS 0.
    let mut reader = ClipReader::open(id.clone(), &clip, &segment(Some(2.0), None)).unwrap();
    assert_eq!(reader.info().fps, 10.0);
    let frames = read_all(&mut reader);
    assert_eq!(frames.len(), 20, "the last 2 s at 10 fps");
    for (k, f) in frames.iter().enumerate() {
        let k = k as u64;
        assert_eq!((f.seq, f.pts), (k, Duration::from_millis(100 * k)));
        assert_eq!(
            number(f),
            20 + k,
            "frame {k} shows the file at {} s",
            (20 + k) / FPS
        );
    }
    assert!(reader.next_frame().unwrap().is_none(), "and stays ended");

    // Played as a live source, cut to one frame: that frame is the one
    // from 2 s, with sequence number 0 and PTS 0.
    let played = play_to_end(&file_config(&clip, 2.0, 0.1));
    let first = &played.last;
    assert_eq!((first.seq, first.pts), (0, Duration::ZERO));
    assert_eq!(number(first), 20);
    assert!(
        (played.audio_start() - 2.0).abs() < 1e-3,
        "and so is the audio"
    );
}

#[test]
fn a_segment_ends_after_its_duration_with_its_audio() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("duration");
    let clip = make_clip(&tools, &dir.0);
    let mut reader = ClipReader::open(
        SourceId("clip".into()),
        &clip,
        &segment(Some(2.0), Some(1.0)),
    )
    .unwrap();
    let frames = read_all(&mut reader);
    let numbers: Vec<u64> = frames.iter().map(number).collect();
    assert_eq!(numbers, (20..30).collect::<Vec<u64>>());
    // A live source plays the same second, its audio cut with it.
    let played = play_to_end(&file_config(&clip, 2.0, 1.0));
    let last = &played.last;
    assert_eq!((last.seq, last.pts), (9, Duration::from_millis(900)));
    assert_eq!(number(last), 29);
    let samples = played.samples;
    assert!(
        samples.abs_diff(48_000) <= 480,
        "one second of audio, give or take a block: {samples}"
    );
}

#[test]
fn a_start_between_frames_begins_at_the_next_frame() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("between");
    let clip = make_clip(&tools, &dir.0);
    let mut reader = ClipReader::open(
        SourceId("clip".into()),
        &clip,
        &segment(Some(2.05), Some(0.3)),
    )
    .unwrap();
    let frames = read_all(&mut reader);
    assert_eq!(frames[0].pts, Duration::ZERO);
    assert_eq!(number(&frames[0]), 21, "the first frame at or after 2.05 s");
    let numbers: Vec<u64> = frames.iter().map(number).collect();
    assert_eq!(numbers, [21, 22, 23], "the frames from 2.1 s to 2.3 s");
}

#[test]
fn a_start_between_frames_starts_the_audio_with_the_next_frame() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("between-audio");
    let clip = make_clip(&tools, &dir.0);
    let played = play_to_end(&file_config(&clip, 2.05, 0.3));
    assert_eq!(
        number(&played.last),
        23,
        "the last frame is the one from 2.3 s"
    );
    // The first sample is the one from 2.1 s, where the first frame is,
    // not from 2.05 s, and the audio lasts as long as the three frames.
    let start = played.audio_start();
    assert!((start - 2.1).abs() < 1e-3, "the audio starts at {start} s");
    let samples = played.samples;
    assert!(
        samples.abs_diff(14_400) <= 480,
        "0.3 s of audio, give or take a block: {samples}"
    );
}

#[test]
fn an_mpegts_recording_opened_at_start_s_starts_with_the_frame_from_then() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("ts");
    let ts = make(&tools, &dir.0, &TS);
    let id = SourceId("ts".into());
    for (start_s, first) in [(2.0, 20), (2.5, 25), (2.05, 21)] {
        let mut reader =
            ClipReader::open(id.clone(), &ts, &segment(Some(start_s), Some(1.0))).unwrap();
        let frames = read_all(&mut reader);
        let numbers: Vec<u64> = frames.iter().map(number).collect();
        assert_eq!(
            numbers,
            (first..first + 10).collect::<Vec<u64>>(),
            "one second from {start_s} s"
        );
        assert_eq!((frames[0].seq, frames[0].pts), (0, Duration::ZERO));
    }
    // A live source of the TS plays the same frames with a second of audio.
    let played = play_to_end(&file_config(&ts, 2.5, 1.0));
    assert_eq!((played.last.seq, number(&played.last)), (9, 34));
    let samples = played.samples;
    assert!(
        samples.abs_diff(48_000) <= 1_200,
        "one second of audio, give or take a frame of MP2: {samples}"
    );
}

#[test]
fn a_segment_far_into_an_mpegts_recording_seeks_to_the_right_frame() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("long-ts");
    let ts = make(&tools, &dir.0, &LONG_TS);
    let id = SourceId("ts".into());
    // Far enough in that the decoder seeks rather than starting from the
    // top; frames 175, 163 and 190 show as their numbers modulo 40.
    for (start_s, first) in [(17.5, 175), (16.3, 163), (19.0, 190)] {
        let mut reader =
            ClipReader::open(id.clone(), &ts, &segment(Some(start_s), Some(0.5))).unwrap();
        let numbers: Vec<u64> = read_all(&mut reader).iter().map(number).collect();
        let expected: Vec<u64> = (first..first + 5).map(|n| n % 40).collect();
        assert_eq!(numbers, expected, "half a second from {start_s} s");
    }
}

#[test]
fn a_whole_clip_reads_in_order_and_a_looping_one_keeps_counting() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("whole");
    let clip = make_clip(&tools, &dir.0);
    let id = SourceId("clip".into());
    let mut whole = ClipReader::open(id.clone(), &clip, &ClipOptions::default()).unwrap();
    let numbers: Vec<u64> = read_all(&mut whole).iter().map(number).collect();
    assert_eq!(numbers, (0..40).collect::<Vec<u64>>());
    let options = ClipOptions {
        loop_playback: true,
        ..segment(Some(3.0), Some(0.5))
    };
    let mut looping = ClipReader::open(id, &clip, &options).unwrap();
    let frames: Vec<VideoFrame> = (0..12)
        .map(|_| looping.next_frame().unwrap().unwrap())
        .collect();
    let numbers: Vec<u64> = frames.iter().map(number).collect();
    assert_eq!(numbers, [30, 31, 32, 33, 34, 30, 31, 32, 33, 34, 30, 31]);
    assert!(
        frames
            .iter()
            .zip(0..)
            .all(|(f, k)| f.seq == k && f.pts == Duration::from_millis(100 * k))
    );
}

#[test]
fn a_segment_past_the_end_fails_and_no_decoder_outlives_its_reader() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("past");
    let clip = make_clip(&tools, &dir.0);
    let mut past =
        ClipReader::open(SourceId("clip".into()), &clip, &segment(Some(10.0), None)).unwrap();
    let err = past.next_frame().unwrap_err();
    assert!(err.to_string().contains("decoded no frames"), "{err}");
    assert!(past.child_pids().is_empty());
    let mut reader =
        ClipReader::open(SourceId("clip".into()), &clip, &ClipOptions::default()).unwrap();
    reader.next_frame().unwrap().unwrap();
    let pids = reader.child_pids();
    assert_eq!(pids.len(), 1, "one decoder");
    drop(reader);
    #[cfg(unix)]
    for pid in pids {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(!alive, "ffmpeg {pid} outlived its reader");
    }
}
