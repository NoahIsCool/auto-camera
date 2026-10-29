//! `FileSource` against real ffmpeg, on tiny clips generated per test.
//!
//! Every test skips (passes with a note) when `ffmpeg`/`ffprobe` are not
//! installed, unless `AC_REQUIRE_FFMPEG` is set (CI), which makes a missing
//! tool a failure. Clips come from ffmpeg's built-in `testsrc` and `sine`
//! generators, so no media files live in the repository.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{AudioBlock, MediaError, SourceInfo, SourceState, VideoFrame, VideoSource};
use ac_sources::{
    Backoff, FfmpegTools, FileOptions, FileSource, FrameRate, SourceConfig, SupervisedSource,
    VirtualClock, WallClock, open_source, probe,
};
use common::{TempDir, make_clip, poll, tools};

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

/// Writes `name.mkv` in `dir`: a 1 s 160x120 30 fps `testsrc` clip whose
/// audio is ffmpeg's 48 kHz 440 Hz `sine` tone (crest 1/8, 16-bit PCM)
/// routed by the `pan` filter `pan` (its layout, then its channels).
fn clip_with_audio(tools: &FfmpegTools, dir: &Path, name: &str, pan: &str) -> PathBuf {
    let path = dir.join(format!("{name}.mkv"));
    let status = Command::new(&tools.ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg("testsrc=size=160x120:rate=30:duration=1")
        .args(["-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=48000:duration=1")
        .args(["-af", &format!("pan={pan}"), "-c:a", "pcm_s16le"])
        .args(["-c:v", "mpeg4", "-q:v", "4"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make {name}");
    path
}

/// A file's audio arrives as stereo, mixed as the encoder mixed a
/// source's own channels before program audio was always stereo: a mono
/// track on both sides at −3 dB, a 5.1 track's centre (its dialogue) in
/// both sides at −3 dB and its LFE left out. Program audio from a video
/// source therefore keeps its level.
#[test]
fn embedded_audio_is_mixed_to_stereo_as_the_encoder_mixed_it() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("mix");
    let want = 0.125 * std::f64::consts::FRAC_1_SQRT_2;
    for (name, pan) in [("mono", "mono|c0=c0"), ("surround", "5.1|FC=c0|LFE=c0")] {
        let clip = clip_with_audio(&tools, &dir.0, name, pan);
        let src = open_unpaced(&clip, opts(false));
        assert_eq!(src.info().audio, Some((48_000, 2)), "{name}");
        let mut blocks: Vec<AudioBlock> = Vec::new();
        assert!(poll(Duration::from_secs(20), || {
            blocks.extend(src.drain_audio());
            src.state() == SourceState::Stopped
        }));
        std::thread::sleep(Duration::from_millis(50));
        blocks.extend(src.drain_audio());
        assert!(!blocks.is_empty(), "{name}: no audio");
        let mut peak = 0f64;
        for b in &blocks {
            assert_eq!(b.channels, 2, "{name}");
            for frame in b.samples.chunks_exact(2) {
                let (l, r) = (frame[0], frame[1]);
                assert_eq!(l.to_bits(), r.to_bits(), "{name}: both sides alike");
                peak = peak.max(f64::from(l.abs()));
            }
        }
        assert!(
            (peak - want).abs() < 1e-4,
            "{name}: peak {peak}, want {want}"
        );
    }
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

/// Kills process `pid` with `SIGKILL`.
#[cfg(unix)]
fn kill(pid: u32) {
    let killed = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success(), "kill {pid}");
}

/// Spec C.3: when ffmpeg dies under a running file source (killed here,
/// first the video decoder, then on the next connection the audio one), the
/// source fails, its supervisor opens the file again under a new epoch, and
/// every decoder is reaped: the killed ones at once, the rest on stop.
#[cfg(unix)]
#[test]
fn a_file_source_whose_ffmpeg_is_killed_restarts_and_leaves_no_child_process() {
    let Some(tools) = tools() else { return };
    let dir = TempDir::new("killed");
    let clip = make_clip(&tools, &dir.0, 4.0, Some(4.0));
    let id = SourceId("file".into());
    // The decoders of each connection, video first.
    let decoders: Arc<Mutex<Vec<Vec<u32>>>> = Arc::default();
    let (opened, path, file) = (decoders.clone(), clip.clone(), id.clone());
    let opener = move || -> Result<Box<dyn VideoSource>, MediaError> {
        let src = FileSource::open(file.clone(), &path, opts(false))?;
        opened.lock().unwrap().push(src.child_pids());
        Ok(Box::new(src))
    };
    let offline = SourceInfo {
        id,
        width: 160,
        height: 120,
        fps: 0.0,
        audio: None,
    };
    let clock = Arc::new(WallClock::new());
    let mut src = SupervisedSource::start(opener, offline, clock, Backoff::default()).unwrap();
    assert_eq!(src.start_error(), None);
    let connection = |n: usize| decoders.lock().unwrap().get(n).cloned();
    let mut frames = Vec::new();
    for (epoch, victim, what) in [(1, 0, "video"), (2, 1, "audio")] {
        assert!(poll(Duration::from_secs(20), || {
            record(&src, &mut frames);
            frames.last().is_some_and(|f| f.epoch == epoch - 1)
                && src.state() == SourceState::Running
        }));
        let pids = connection(epoch as usize - 1).unwrap();
        assert_eq!(pids.len(), 2, "video and audio decoders: {pids:?}");
        assert!(pids.iter().all(|&p| alive(p)));
        kill(pids[victim]);
        // The failure shows while the supervisor backs off (0.5 s, then 1 s).
        let message = format!("the {what} decoder exited during playback");
        assert!(poll(Duration::from_secs(20), || {
            src.state() == SourceState::Offline && src.error().is_some_and(|e| e.contains(&message))
        }));
        for pid in &pids {
            assert!(!alive(*pid), "{pid} of a failed connection was not reaped");
        }
        let started = Instant::now();
        assert!(poll(Duration::from_secs(20), || {
            record(&src, &mut frames);
            frames.last().is_some_and(|f| f.epoch == epoch)
        }));
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "within the backoff"
        );
        assert_eq!(src.error(), None);
    }
    assert!(
        frames
            .windows(2)
            .all(|w| w[0].seq < w[1].seq && w[0].pts < w[1].pts)
    );
    src.stop();
    let all: Vec<u32> = decoders.lock().unwrap().concat();
    assert_eq!(all.len(), 6, "three connections");
    for pid in all {
        assert!(!alive(pid), "ffmpeg {pid} outlived its source");
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
