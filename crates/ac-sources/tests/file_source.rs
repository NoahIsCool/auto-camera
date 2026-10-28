//! `FileSource` against real ffmpeg, on tiny clips generated per test.
//!
//! Every test skips (passes with a note) when `ffmpeg`/`ffprobe` are not
//! installed, unless `AC_REQUIRE_FFMPEG` is set (CI), which makes a missing
//! tool a failure. Clips come from ffmpeg's built-in `testsrc` and `sine`
//! generators, so no media files live in the repository.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{AudioBlock, MediaError, SourceState, VideoFrame, VideoSource};
use ac_sources::{
    FfmpegTools, FileOptions, FileSource, FrameRate, SourceConfig, VirtualClock, open_source, probe,
};

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
        let dir = std::env::temp_dir().join(format!("ac-sources-{}-{name}", std::process::id()));
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

/// Writes a 160x120 30 fps `testsrc` clip of `video_secs`, with a stereo
/// 48 kHz 440 Hz tone of `audio_secs` when given.
fn make_clip(tools: &FfmpegTools, dir: &Path, video_secs: f64, audio_secs: Option<f64>) -> PathBuf {
    let path = dir.join("clip.mkv");
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!(
            "testsrc=size=160x120:rate=30:duration={video_secs}"
        ));
    if let Some(a) = audio_secs {
        cmd.args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:sample_rate=48000:duration={a}"))
            .args(["-ac", "2", "-c:a", "pcm_s16le"]);
    }
    cmd.args(["-c:v", "mpeg4", "-q:v", "4"]).arg(&path);
    assert!(
        cmd.status().unwrap().success(),
        "ffmpeg could not make a clip"
    );
    path
}

/// Unpaced playback options.
fn opts(loop_playback: bool) -> FileOptions {
    FileOptions {
        loop_playback,
        ..FileOptions::default()
    }
}

/// Opens `path` unpaced.
fn open_unpaced(path: &Path, options: FileOptions) -> FileSource {
    FileSource::open_with_clock(
        SourceId("file".into()),
        path,
        options,
        Arc::new(VirtualClock::new()),
    )
    .unwrap()
}

/// Polls `step` every 2 ms until it returns true or `timeout` passes.
fn poll(timeout: Duration, mut step: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if step() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Records each newly seen frame of `src` into `frames`.
fn record(src: &dyn VideoSource, frames: &mut Vec<VideoFrame>) {
    if let Some(f) = src.latest()
        && frames.last().is_none_or(|l| l.seq != f.seq)
    {
        frames.push(f);
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

/// Sum of squares of a block's samples.
fn energy(b: &AudioBlock) -> f64 {
    b.samples.iter().map(|&s| (s as f64).powi(2)).sum()
}

/// Asserts that blocks follow each other without gaps or overlaps.
fn assert_contiguous(blocks: &[AudioBlock]) {
    for w in blocks.windows(2) {
        let gap = w[1].pts.as_secs_f64() - (w[0].pts.as_secs_f64() + w[0].duration().as_secs_f64());
        assert!(
            gap.abs() < 1e-6,
            "gap of {gap}s after block at {:?}",
            w[0].pts
        );
    }
}

#[test]
fn probe_reports_size_rate_and_audio() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("probe");
    let clip = make_clip(&tools, &dir.0, 1.0, Some(1.0));
    let p = probe(&tools, &clip).unwrap();
    assert_eq!((p.video.width, p.video.height), (160, 120));
    assert_eq!(p.video.frame_rate, FrameRate::new(30, 1).unwrap());
    let a = p.audio.unwrap();
    assert_eq!((a.sample_rate, a.channels), (48_000, 2));
    assert_eq!(p.duration, Some(Duration::from_secs(1)));

    let src = open_unpaced(&clip, opts(false));
    let info = src.info();
    assert_eq!((info.width, info.height, info.fps), (160, 120, 30.0));
    assert_eq!(info.audio, Some((48_000, 2)));
    assert_eq!(src.probe_info(), &p);
}

#[test]
fn frames_arrive_in_order_until_end_of_file() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("frames");
    let clip = make_clip(&tools, &dir.0, 1.0, None);
    let src = open_unpaced(&clip, opts(false));
    let mut frames = Vec::new();
    assert!(poll(Duration::from_secs(20), || {
        record(&src, &mut frames);
        src.state() == SourceState::Stopped
    }));
    record(&src, &mut frames);
    assert_eq!(src.error(), None);
    assert!(frames.windows(2).all(|w| w[0].seq < w[1].seq));
    let rate = FrameRate::new(30, 1).unwrap();
    for f in &frames {
        assert_eq!(f.source, SourceId("file".into()));
        assert_eq!((f.width, f.height, f.data.len()), (160, 120, 160 * 120 * 4));
        assert_eq!(f.pts, rate.pts(f.seq));
    }
    let last = frames.last().unwrap();
    assert_eq!(last.seq, 29, "a 1 s clip at 30 fps has 30 frames");
    assert!(last.data.chunks_exact(4).any(|p| p != &last.data[..4]));
    assert!(last.data.chunks_exact(4).all(|p| p[3] == 255));
}

#[test]
fn downscales_to_max_width() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("scale");
    let clip = make_clip(&tools, &dir.0, 0.2, None);
    let options = FileOptions {
        max_width: Some(80),
        ..opts(false)
    };
    let src = open_unpaced(&clip, options);
    assert_eq!((src.info().width, src.info().height), (80, 60));
    assert!(poll(Duration::from_secs(20), || src.latest().is_some()));
    let f = src.latest().unwrap();
    assert_eq!((f.width, f.height, f.data.len()), (80, 60, 80 * 60 * 4));
}

#[test]
fn audio_blocks_cover_the_file() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("audio");
    let clip = make_clip(&tools, &dir.0, 1.0, Some(1.0));
    let src = open_unpaced(&clip, opts(false));
    let mut blocks: Vec<AudioBlock> = Vec::new();
    let total = |b: &[AudioBlock]| b.iter().map(AudioBlock::frames).sum::<usize>();
    assert!(poll(Duration::from_secs(20), || {
        blocks.extend(src.drain_audio());
        total(&blocks) >= 48_000 && src.state() == SourceState::Stopped
    }));
    std::thread::sleep(Duration::from_millis(50));
    blocks.extend(src.drain_audio());
    assert_eq!(total(&blocks), 48_000);
    assert_eq!(src.audio_drops(), 0);
    assert_eq!(blocks[0].pts, Duration::ZERO);
    for b in &blocks {
        assert_eq!((b.sample_rate, b.channels, b.frames()), (48_000, 2, 480));
        assert_eq!(b.source, SourceId("file".into()));
        assert!(energy(b) > 0.0);
    }
    assert_contiguous(&blocks);
}

#[test]
fn audio_can_be_disabled_or_absent() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("noaudio");
    let clip = make_clip(&tools, &dir.0, 0.3, Some(0.3));
    let options = FileOptions {
        audio: false,
        ..opts(false)
    };
    let src = open_unpaced(&clip, options);
    assert_eq!(src.info().audio, None);
    assert!(poll(Duration::from_secs(20), || src.state()
        == SourceState::Stopped));
    assert!(src.drain_audio().is_empty());
    drop(src);

    let silent = TempDir::new("videoonly");
    let clip = make_clip(&tools, &silent.0, 0.3, None);
    let src = open_unpaced(&clip, opts(false));
    assert_eq!(src.info().audio, None);
}

#[cfg(unix)]
#[test]
fn stop_and_drop_leave_no_child_processes() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("stop");
    let clip = make_clip(&tools, &dir.0, 1.0, Some(1.0));
    for use_drop in [false, true] {
        let mut src = FileSource::open(SourceId("file".into()), &clip, opts(true)).unwrap();
        assert!(poll(Duration::from_secs(20), || src.state()
            == SourceState::Running));
        let pids = src.child_pids();
        assert!(pids.len() >= 2, "video and audio decoders: {pids:?}");
        assert!(pids.iter().all(|&p| alive(p)));
        if use_drop {
            drop(src);
        } else {
            let started = Instant::now();
            src.stop();
            assert!(started.elapsed() < Duration::from_secs(3));
            assert_eq!(src.state(), SourceState::Stopped);
            assert!(src.child_pids().is_empty());
            src.stop();
        }
        for pid in pids {
            assert!(!alive(pid), "ffmpeg {pid} outlived its source");
        }
    }
}

#[test]
fn looping_keeps_pts_monotonic() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("loop");
    let clip = make_clip(&tools, &dir.0, 0.5, Some(0.5));
    let mut src = open_unpaced(&clip, opts(true));
    let mut frames = Vec::new();
    let mut blocks = Vec::new();
    assert!(poll(Duration::from_secs(30), || {
        record(&src, &mut frames);
        blocks.extend(src.drain_audio());
        frames.last().is_some_and(|f| f.seq >= 75)
    }));
    assert_eq!(src.state(), SourceState::Running);
    let rate = FrameRate::new(30, 1).unwrap();
    assert!(
        frames
            .windows(2)
            .all(|w| w[0].seq < w[1].seq && w[0].pts < w[1].pts)
    );
    assert!(frames.iter().all(|f| f.pts == rate.pts(f.seq)));
    assert!(
        frames.last().unwrap().pts > Duration::from_millis(2400),
        "five passes"
    );
    assert!(!blocks.is_empty());
    assert!(blocks.windows(2).all(|w| w[0].pts < w[1].pts));
    src.stop();
    assert_eq!(src.error(), None);
}

/// Plays a looping clip in real time for a bit over one pass and returns all
/// audio blocks, checking none were dropped.
fn play_loop_in_real_time(clip: &Path) -> Vec<AudioBlock> {
    let src = FileSource::open(SourceId("file".into()), clip, opts(true)).unwrap();
    let mut blocks: Vec<AudioBlock> = Vec::new();
    assert!(poll(Duration::from_secs(20), || {
        blocks.extend(src.drain_audio());
        blocks
            .last()
            .is_some_and(|b| b.pts >= Duration::from_millis(1100))
    }));
    assert_eq!(src.audio_drops(), 0);
    blocks
}

#[test]
fn looping_audio_restarts_with_the_video() {
    let Some(tools) = tools() else { return };
    // Audio shorter than video: padded with silence up to the loop point.
    let short = TempDir::new("loopshort");
    let short_clip = make_clip(&tools, &short.0, 1.0, Some(0.5));
    // Audio longer than video: trimmed at the loop point.
    let long = TempDir::new("looplong");
    let long_clip = make_clip(&tools, &long.0, 1.0, Some(1.5));
    let (short_blocks, long_blocks) = std::thread::scope(|s| {
        let a = s.spawn(|| play_loop_in_real_time(&short_clip));
        let b = s.spawn(|| play_loop_in_real_time(&long_clip));
        (a.join().unwrap(), b.join().unwrap())
    });
    let one_second = Duration::from_secs(1);
    for (blocks, audio_secs) in [(&short_blocks, 0.5), (&long_blocks, 1.5)] {
        assert_contiguous(blocks);
        let second_pass = blocks.iter().position(|b| b.pts == one_second);
        let second_pass = second_pass.expect("a block starts exactly at the loop point");
        assert_eq!(
            blocks[second_pass].samples, blocks[0].samples,
            "pass 2 restarts the tone"
        );
        for b in &blocks[..second_pass] {
            let silent = energy(b) == 0.0;
            let expect_silent = b.pts.as_secs_f64() >= audio_secs;
            assert_eq!(
                silent, expect_silent,
                "block at {:?} ({audio_secs}s of audio)",
                b.pts
            );
        }
    }
}

#[test]
fn real_time_playback_takes_the_clip_duration() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("paced");
    let clip = make_clip(&tools, &dir.0, 1.0, None);
    let started = Instant::now();
    let src = FileSource::open(SourceId("file".into()), &clip, opts(false)).unwrap();
    assert!(poll(Duration::from_secs(20), || src.state()
        == SourceState::Stopped));
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(950),
        "played 30 frames in {took:?}"
    );
    assert_eq!(src.latest().unwrap().seq, 29);
}

#[test]
fn unusable_files_fail_to_open() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("bad");
    let missing = dir.0.join("missing.mp4");
    let text = dir.0.join("notes.mp4");
    std::fs::write(&text, "not a video").unwrap();
    let wav = dir.0.join("tone.wav");
    let made = Command::new(&tools.ffmpeg)
        .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("sine=duration=0.2")
        .arg(&wav)
        .status()
        .unwrap();
    assert!(made.success());
    for (path, needle) in [
        (&missing, "missing.mp4"),
        (&text, "ffprobe failed"),
        (&wav, "no video stream"),
    ] {
        let config = SourceConfig::File {
            path: path.clone(),
            loop_playback: false,
            max_width: None,
            audio: true,
            start_s: None,
            duration_s: None,
        };
        match open_source(&config, SourceId("bad".into())) {
            Err(MediaError::Open(msg)) => assert!(msg.contains(needle), "{msg}"),
            Err(other) => panic!("{path:?}: unexpected error {other}"),
            Ok(_) => panic!("{path:?} opened"),
        }
    }
    let clip = make_clip(&tools, &dir.0, 0.2, None);
    let options = FileOptions {
        max_width: Some(1),
        ..opts(false)
    };
    match FileSource::open(SourceId("bad".into()), &clip, options) {
        Err(MediaError::Open(msg)) => assert!(msg.contains("max_width"), "{msg}"),
        other => panic!("max_width 1 accepted: {other:?}"),
    }
}
