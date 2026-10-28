//! The stream against a real RTMP receiver on loopback, killed and started
//! again mid-stream (spec M11, and M14: encoder status, visible errors).
//!
//! The receiver is a second ffmpeg (`-listen 1`) that accepts one
//! publisher on `127.0.0.1` and throws away what it gets. Each test binds
//! a port that was just free on the loopback interface, never any other
//! address, and holds its receiver in a [`Receiver`] guard that kills and
//! reaps it when dropped, so a failed assertion leaves no process behind
//! (the encoder's own `Drop` does the same for the sender). Frames go in at
//! the real 30 fps, because ffmpeg times its reconnection attempts by the
//! wall clock. Every test returns early (passing) with a note when ffmpeg
//! is missing, unless `AC_REQUIRE_FFMPEG` is set.

mod common;

use std::fs::File;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ac_output::{
    EncoderStats, OutputConfig, ProgramEncoder, StreamHealth, StreamState, mux_stats_supported,
};

/// Stream key in the URLs; it must never show in an error.
const KEY: &str = "test-stream-key";

/// Frames per second of the program.
const FPS: u32 = 30;

/// Longest wait for a state the stream reaches by itself.
const LIMIT: Duration = Duration::from_secs(15);

/// A receiving ffmpeg; killed and reaped when dropped.
struct Receiver {
    /// The ffmpeg process.
    child: Child,
}

impl Receiver {
    /// Starts a receiver for `url`, logging to `log`.
    fn start(url: &str, log: &Path) -> Self {
        let child = Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-loglevel", "error"])
            .args(["-listen", "1", "-i", url, "-c", "copy", "-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(log).unwrap())
            .spawn()
            .unwrap();
        Self { child }
    }

    /// Kills the receiver (as a crash would) and reaps it.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Receiver {
    /// Kills and reaps the receiver, also when a test fails.
    fn drop(&mut self) {
        self.kill();
    }
}

/// A loopback port that was free a moment ago.
fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A 30 fps program recorded to `path` and streamed to `url`.
fn config(path: PathBuf, url: String) -> OutputConfig {
    OutputConfig {
        width: 320,
        height: 180,
        fps: FPS,
        video_bitrate_kbps: 800,
        keyframe_seconds: 1,
        encoder: ac_output::Encoder::Libx264,
        record_path: Some(path),
        rtmp_url: Some(url),
        ..OutputConfig::default()
    }
}

/// The stream's health in `stats`.
fn stream(stats: &EncoderStats) -> StreamHealth {
    stats.stream.expect("the encode streams")
}

/// Pushes program frames to an encoder on time.
struct Feed<'a> {
    /// The encoder.
    enc: &'a ProgramEncoder,
    /// Frames pushed so far.
    frames: u32,
    /// When the first frame was due.
    started: Instant,
}

impl<'a> Feed<'a> {
    /// A feed whose first frame is due now.
    fn new(enc: &'a ProgramEncoder) -> Self {
        Self {
            enc,
            frames: 0,
            started: Instant::now(),
        }
    }

    /// Pushes frames on time until `done` holds for the encoder's stats,
    /// or for `limit`; returns the stats that satisfied it.
    fn until(
        &mut self,
        limit: Duration,
        done: impl Fn(&EncoderStats) -> bool,
    ) -> Option<EncoderStats> {
        let deadline = Instant::now() + limit;
        loop {
            let due =
                self.started + Duration::from_secs_f64(f64::from(self.frames) / f64::from(FPS));
            thread::sleep(due.saturating_duration_since(Instant::now()));
            self.enc
                .push_video(&common::frame(320, 180, self.frames))
                .unwrap_or_else(|e| panic!("ffmpeg stopped: {e}: {:?}", self.enc.stats()));
            self.frames += 1;
            let stats = self.enc.stats();
            if done(&stats) {
                return Some(stats);
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// Pushes frames on time for `span`.
    fn run(&mut self, span: Duration) {
        self.until(span, |_| false);
    }
}

/// Feeds until the stream is connected, which it must become.
fn connect(feed: &mut Feed, receiver_log: &Path) -> EncoderStats {
    feed.until(LIMIT, |s| stream(s).state == StreamState::Connected)
        .unwrap_or_else(|| {
            panic!(
                "never connected: {:?}; receiver: {:?}",
                feed.enc.stats(),
                std::fs::read_to_string(receiver_log)
            )
        })
}

/// Kills `receiver` and feeds until the stream says so; returns the stats.
fn kill(feed: &mut Feed, receiver: &mut Receiver) -> EncoderStats {
    receiver.kill();
    feed.until(LIMIT, |s| {
        matches!(stream(s).state, StreamState::Reconnecting { .. })
    })
    .unwrap_or_else(|| panic!("no reconnection after the kill: {:?}", feed.enc.stats()))
}

/// Frames in the recording, counted by ffprobe.
fn recorded_frames(path: &Path) -> u32 {
    let video = common::probe(
        path,
        &[
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
        ],
    );
    video["nb_read_frames"].parse().unwrap()
}

#[test]
fn a_killed_receiver_leaves_the_recording_whole_and_the_stream_reconnecting() {
    if !common::have_ffmpeg() {
        return;
    }
    let dir = common::scratch_dir("stream-kill");
    let path = dir.join("program.mp4");
    let url = format!("rtmp://127.0.0.1:{}/live/{KEY}", free_port());
    let log = dir.join("receiver.log");
    let mut receiver = Receiver::start(&url, &log);
    // Let the receiver listen before the first connection attempt.
    thread::sleep(Duration::from_millis(500));
    let enc = common::start(&config(path.clone(), url)).unwrap();
    let mut feed = Feed::new(&enc);

    let up = connect(&mut feed, &log);
    assert_eq!(up.last_error, None, "{up:?}");
    assert!(!stream(&up).stalled, "{up:?}");

    let down = kill(&mut feed, &mut receiver);
    let health = stream(&down);
    assert_eq!(health.outages, stream(&up).outages + 1, "{down:?}");
    assert!(!health.stalled);
    if health.bitrate_kbps.is_some() {
        assert_eq!(
            health.bitrate_kbps,
            Some(0.0),
            "nothing reaches a dead ingest"
        );
    }
    let err = down.last_error.clone().expect("the outage is reported");
    assert!(
        err.starts_with("fifo: Error opening") || err.starts_with("tcp:"),
        "{err}"
    );
    assert!(!err.contains(KEY), "key leaked: {err}");

    // The recording goes on through the outage, attempt after attempt.
    let retried = feed
        .until(
            LIMIT,
            |s| matches!(stream(s).state, StreamState::Reconnecting { attempt } if attempt >= 2),
        )
        .unwrap_or_else(|| panic!("no second attempt: {:?}", enc.stats()));
    assert_eq!(retried.frames_dropped, 0, "{retried:?}");
    feed.run(Duration::from_millis(500));
    let pushed = feed.frames;

    let done = enc.finish().unwrap();
    assert_eq!(done.frames_dropped, 0, "{done:?}");
    assert_eq!(done.frames_written, u64::from(pushed), "{done:?}");
    assert_eq!(done.progress.drop_frames, 0, "{done:?}");
    assert_eq!(stream(&done).state, StreamState::Down, "ffmpeg has exited");
    assert_eq!(recorded_frames(&path), pushed);
    drop(receiver);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_restarted_receiver_is_connected_again_within_five_seconds() {
    if !common::have_ffmpeg() {
        return;
    }
    let dir = common::scratch_dir("stream-restart");
    let path = dir.join("program.mp4");
    let url = format!("rtmp://127.0.0.1:{}/live/{KEY}", free_port());
    let log = dir.join("receiver-1.log");
    let mut receiver = Receiver::start(&url, &log);
    thread::sleep(Duration::from_millis(500));
    let enc = common::start(&config(path.clone(), url.clone())).unwrap();
    let mut feed = Feed::new(&enc);
    let up = connect(&mut feed, &log);
    let down = kill(&mut feed, &mut receiver);
    assert!(down.last_error.is_some(), "{down:?}");

    let log = dir.join("receiver-2.log");
    receiver = Receiver::start(&url, &log);
    let restarted = Instant::now();
    let again = connect(&mut feed, &log);
    let took = restarted.elapsed();
    assert!(
        took < Duration::from_secs(5),
        "connected again after {took:?}"
    );
    let health = stream(&again);
    assert_eq!(health.reconnects, stream(&up).reconnects + 1, "{again:?}");
    assert!(!health.stalled, "{again:?}");
    assert_eq!(
        again.last_error, None,
        "the recovery clears the stream's error"
    );
    if mux_stats_supported(Path::new("ffmpeg")) {
        let kbps = health.bitrate_kbps.expect("ffmpeg reports muxed packets");
        assert!(kbps > 0.0, "{again:?}");
    } else {
        eprintln!("note: this ffmpeg has no mux statistics; the bitrate is not checked");
    }

    feed.run(Duration::from_millis(500));
    let pushed = feed.frames;
    let done = enc.finish().unwrap();
    assert_eq!(done.frames_dropped, 0, "{done:?}");
    assert_eq!(recorded_frames(&path), pushed);
    drop(receiver);
    std::fs::remove_dir_all(dir).unwrap();
}
