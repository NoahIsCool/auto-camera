//! Sources that drop out (spec M1 "hot-reconnect with backoff", C.3
//! "source reconnect within 8 s", §d "source epochs"): the core fed by a
//! supervised fault-injecting source on a virtual clock, and the engine
//! started with a source that is not there.

mod common;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ac_core::{ShotId, SourceId};
use ac_engine::discovery::{DiscoveryRequest, DiscoveryResult, synthetic_detector};
use ac_engine::{
    CommandError, CommandReply, DetectorSettings, EngineCommand, EngineCore, OutputState,
    OutputView, PassOutcome, SourceEntry, SourceFeed, SourceView, TimingView,
};
use ac_media::{SourceInfo, SourceState, VideoFrame, VideoSource};
use ac_sources::{
    Backoff, Clock, FlakySynthetic, OpenAttempt, Outage, SourceConfig, SourceOpener,
    SupervisedSource, SyntheticPattern, VirtualClock,
};
use ac_switcher::SwitchError;
use ac_vision::{Detector, ScriptedDetector};

use common::{WAIT, have, start, studio, temp_path};

/// `s` seconds.
fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

/// The shot with `id`.
fn shot(id: &str) -> ShotId {
    ShotId(id.into())
}

/// The core with one supervised source, ticked by hand, its discovery
/// answered on the next tick except for one request held back.
struct Rig {
    /// The core under test.
    core: EngineCore,
    /// Ground truth of the pattern.
    detector: ScriptedDetector,
    /// Answers for the next tick.
    pending: Vec<DiscoveryResult>,
    /// The request whose answer is held back, once made.
    held: Option<DiscoveryRequest>,
    /// Master-clock time of the newest tick.
    now: Duration,
}

impl Rig {
    /// The answer to `request`.
    fn answer(&mut self, request: &DiscoveryRequest) -> DiscoveryResult {
        DiscoveryResult {
            source: 0,
            frame: request.frame.clone(),
            queries: request.queries.iter().map(|q| q.id.clone()).collect(),
            outcome: self
                .detector
                .detect(&request.frame, &request.queries)
                .map_err(|e| e.to_string()),
            latency: Duration::ZERO,
        }
    }

    /// One tick on `src`'s state and `frame`, at the frame's PTS or one
    /// frame period after the previous tick, whichever is later. The first
    /// request on an epoch-0 frame of 5 s or later is held back; any other
    /// is answered on the next tick.
    fn tick(&mut self, src: &SupervisedSource, frame: &VideoFrame) {
        self.now = frame.pts.max(self.now + secs(1.0 / 30.0));
        let feed = SourceFeed {
            state: src.state(),
            frame: Some(frame.clone()),
            detector_loading: false,
            error: src.error(),
            info: Some(src.current_info()),
        };
        let results = std::mem::take(&mut self.pending);
        let out = self.core.tick(self.now, &[feed], results);
        for (_, request) in out.requests {
            let f = &request.frame;
            if self.held.is_none() && f.epoch == 0 && f.pts >= secs(5.0) {
                self.held = Some(request);
            } else {
                let answer = self.answer(&request);
                self.pending.push(answer);
            }
        }
    }

    /// The core's view of the source.
    fn source(&self) -> SourceView {
        let snap = self
            .core
            .snapshot(OutputView::default(), TimingView::default(), true);
        snap.sources[0].clone()
    }

    /// Sequence number of the frame the newest applied answer looked at.
    fn feedback_seq(&self) -> Option<u64> {
        let snap = self
            .core
            .snapshot(OutputView::default(), TimingView::default(), true);
        snap.queries[0].matches.first().map(|m| m.seq)
    }
}

/// A fake source fails at 10 s and returns at 13 s: its frames resume
/// within the backoff under a new epoch, with sequence numbers and PTS
/// continuing, and a detection about the older epoch that comes back after
/// the new one began is dropped as stale.
#[test]
fn fails_at_10_s_returns_at_13_s_in_a_new_epoch_and_older_detections_are_dropped() {
    let mut config = studio(1);
    // Neither the age limit nor the timeout can drop the held answer: only
    // its epoch can.
    config.discovery.max_result_age_s = 60.0;
    config.discovery.timeout_s = 60.0;
    let stage = SourceId("stage".into());
    let pattern = SyntheticPattern::new(320, 180, 30.0, 1).unwrap();
    let rate = pattern.frame_rate();
    let clock = Arc::new(VirtualClock::new());
    let outage = Outage {
        from: secs(10.0),
        until: secs(13.0),
    };
    let mut flaky =
        FlakySynthetic::new(stage.clone(), pattern.clone(), clock.clone(), vec![outage]);
    let (log, tried) = (flaky.log(), flaky.log());
    // The first attempt after the failure waits until the test has looked:
    // on a virtual clock the whole outage passes in microseconds.
    let (arrived, at_retry) = mpsc::channel();
    let (resume, go) = mpsc::channel::<()>();
    let opener = move || {
        if tried.attempts().len() == 1 {
            let _ = arrived.send(());
            let _ = go.recv_timeout(Duration::from_secs(30));
        }
        flaky.open()
    };
    let offline = SourceInfo {
        id: stage.clone(),
        width: 320,
        height: 180,
        fps: 30.0,
        audio: None,
    };
    let mut src =
        SupervisedSource::start(opener, offline, clock.clone(), Backoff::default()).unwrap();
    let core = EngineCore::new(&config, vec![src.info().clone()], &[true]).unwrap();
    let mut rig = Rig {
        core,
        detector: synthetic_detector(pattern.clone(), 0.9),
        pending: Vec::new(),
        held: None,
        now: Duration::ZERO,
    };

    // Epoch 0, up to the failure at 10 s: every frame the supervisor
    // passes on, ending with the last one before 10 s.
    let deadline = Instant::now() + WAIT;
    let mut last: Option<VideoFrame> = None;
    loop {
        let failed = at_retry.try_recv().is_ok();
        let wait = if failed {
            Duration::ZERO
        } else {
            Duration::from_millis(5)
        };
        if let Some(f) = src.wait_newer(last.as_ref().map(|f| f.seq), wait) {
            rig.tick(&src, &f);
            last = Some(f);
        }
        if failed {
            break;
        }
        assert!(Instant::now() < deadline, "the fault never came");
    }
    let last = last.expect("epoch 0 delivered");
    assert_eq!((last.epoch, last.seq), (0, 299));
    assert_eq!(last.pts, rate.pts(299));
    assert_eq!(clock.now(), secs(10.5), "retrying 0.5 s after the failure");
    // While the supervisor retries, the core holds a detector pass on the
    // last picture of epoch 0 (made now if not made before).
    rig.tick(&src, &last);
    for _ in 0..60 {
        if rig.held.is_some() {
            break;
        }
        rig.tick(&src, &last);
    }
    let held = rig.held.clone().expect("a pass on epoch 0 is in flight");
    assert_eq!(held.frame.epoch, 0);
    let offline = rig.source();
    assert_eq!(offline.state, SourceState::Reconnecting);
    assert!(!offline.available, "not on air while it reconnects");
    let why = offline.error.unwrap_or_default();
    assert!(why.contains("injected fault"), "{why}");
    let (applied, stale) = (offline.discovery.applied, offline.discovery.stale);
    let feedback = rig.feedback_seq();

    // Epoch 1: the input is back at 13 s, and the attempt at 13.5 s gets
    // through, 0.5 s later (within the 8 s cap).
    resume.send(()).unwrap();
    let first = loop {
        if let Some(f) = src.wait_newer(Some(last.seq), Duration::from_millis(5)) {
            break f;
        }
        assert!(Instant::now() < deadline, "frames never resumed");
    };
    let attempts: Vec<OpenAttempt> = [(0.0, true), (10.5, false), (11.5, false), (13.5, true)]
        .map(|(at, opened)| OpenAttempt {
            at: secs(at),
            opened,
        })
        .into();
    assert_eq!(log.attempts(), attempts);
    assert_eq!(first.epoch, 1, "a new epoch");
    assert!(first.seq >= 300, "numbers continue: {}", first.seq);
    assert_eq!(first.pts, secs(13.5) + rate.pts(first.seq - 300));
    rig.tick(&src, &first);
    let back = rig.source();
    assert_eq!(back.state, SourceState::Running);
    assert!(back.available && back.error.is_none());

    // The held answer about epoch 0 now arrives: it frees the request slot
    // but its boxes are dropped.
    let late = rig.answer(&held);
    rig.pending.push(late);
    rig.tick(&src, &first);
    let after = rig.source();
    assert_eq!(after.discovery.stale, stale + 1, "dropped as stale");
    assert_eq!(after.discovery.applied, applied, "not applied");
    assert_eq!(after.discovery.last_outcome, Some(PassOutcome::Stale));
    assert_eq!(rig.feedback_seq(), feedback, "the feedback is not its");
    // The next pass looks at the new picture and is applied.
    assert!(
        rig.held
            .as_ref()
            .is_some_and(|h| h.frame.seq == held.frame.seq)
    );
    assert_eq!(rig.pending.len(), 1, "a pass on epoch 1 went out at once");
    assert_eq!(rig.pending[0].frame.epoch, 1);
    rig.tick(&src, &first);
    let applied_now = rig.source().discovery;
    assert_eq!(applied_now.applied, applied + 1);
    assert_eq!(applied_now.last_outcome, Some(PassOutcome::Applied));
    assert_eq!(rig.feedback_seq(), Some(first.seq));
    src.stop();
    assert_eq!(src.state(), SourceState::Stopped);
}

/// The view of the only source of a core built with a placeholder for it
/// (the program's 320x180 at an unknown rate, as for a file missing at
/// start), after its first frame (160x120) came with `connection` and its
/// second with a description of 60 fps.
fn described(connection: Option<SourceInfo>) -> SourceView {
    let mut config = studio(1);
    config.detector = DetectorSettings::None;
    let stage = SourceId("stage".into());
    let placeholder = SourceInfo {
        id: stage.clone(),
        width: 320,
        height: 180,
        fps: 0.0,
        audio: None,
    };
    let mut core = EngineCore::new(&config, vec![placeholder.clone()], &[false]).unwrap();
    let pattern = SyntheticPattern::new(160, 120, 25.0, 1).unwrap();
    let later = SourceInfo {
        fps: 60.0,
        ..placeholder
    };
    for (seq, info) in [(0, connection), (1, Some(later))] {
        let feed = SourceFeed {
            state: SourceState::Running,
            frame: Some(pattern.frame(&stage, seq)),
            detector_loading: false,
            error: None,
            info,
        };
        let now = secs(seq as f64 / 25.0);
        core.tick(now, &[feed], Vec::new());
    }
    let snap = core.snapshot(OutputView::default(), TimingView::default(), true);
    snap.sources[0].clone()
}

/// A source described by a placeholder takes its size from its first
/// frame and its rate from the description of that frame's connection.
/// Later descriptions, one of another source and a rate that is not
/// positive and finite change nothing.
#[test]
fn a_placeholder_takes_its_first_frames_size_and_its_connections_rate() {
    let own = |id: &str, fps: f64| SourceInfo {
        id: SourceId(id.into()),
        width: 160,
        height: 120,
        fps,
        audio: Some((48_000, 2)),
    };
    let view = described(Some(own("stage", 25.0)));
    assert_eq!(
        (view.width, view.height, view.nominal_fps),
        (160, 120, 25.0)
    );
    for connection in [
        None,
        Some(own("other", 25.0)),
        Some(own("stage", 0.0)),
        Some(own("stage", f64::NAN)),
        Some(own("stage", f64::INFINITY)),
    ] {
        let why = format!("{connection:?}");
        let view = described(connection);
        assert_eq!((view.width, view.height), (160, 120), "{why}");
        assert_eq!(view.nominal_fps, 0.0, "{why}: still unknown");
    }
}

/// A studio of the synthetic `stage` and a file source `later` reading
/// `path`, which need not exist, without a detector (the synthetic one
/// needs every source synthetic).
fn studio_with_file(path: &std::path::Path) -> ac_engine::StudioConfig {
    let mut config = studio(1);
    config.detector = DetectorSettings::None;
    config.sources.push(SourceEntry {
        id: "later".into(),
        name: None,
        input: SourceConfig::File {
            path: path.into(),
            loop_playback: true,
            max_width: None,
            audio: false,
        },
    });
    config
}

/// The view of source `later`.
fn later(snap: &ac_engine::EngineSnapshot) -> &SourceView {
    snap.source(&SourceId("later".into())).unwrap()
}

/// The engine starts with one of two sources missing: it shows that one
/// offline, with why, and runs the other.
#[test]
fn the_engine_starts_with_one_of_two_sources_missing_and_runs_the_other() {
    let path = temp_path("missing.mkv");
    let engine = start(studio_with_file(&path));
    let h = engine.handle();
    let snap = h
        .wait_until(WAIT, |s| {
            s.sources[0].frames > 30 && s.shot(&shot("stage/wide")).is_some_and(|v| v.available)
        })
        .expect("the stage runs");
    let missing = later(&snap);
    assert!(
        matches!(
            missing.state,
            SourceState::Offline | SourceState::Reconnecting
        ),
        "{:?}",
        missing.state
    );
    let why = missing.error.clone().unwrap_or_default();
    assert!(why.contains("missing.mkv"), "{why}");
    assert!(!missing.available && missing.frames == 0);
    assert_eq!(
        (missing.width, missing.height),
        (320, 180),
        "the program size"
    );
    assert!(!snap.shot(&shot("later/wide")).unwrap().available);
    assert_eq!(snap.sources[0].state, SourceState::Running);
    assert_eq!(snap.sources[0].error, None);
    // The running source goes on air; the missing one cannot.
    h.send(EngineCommand::SetPreview {
        shot: shot("stage/wide"),
    })
    .unwrap();
    h.send(EngineCommand::Cut).unwrap();
    h.wait_until(WAIT, |s| s.switcher.program == Some(shot("stage/wide")))
        .expect("the stage on air");
    h.send(EngineCommand::SetPreview {
        shot: shot("later/wide"),
    })
    .unwrap();
    assert!(matches!(
        h.send(EngineCommand::Cut),
        Err(CommandError::Switch(SwitchError::Unavailable(_)))
    ));
    let report = engine.shutdown().unwrap();
    let end = later(&report.final_snapshot);
    assert!(!report.final_snapshot.running && end.frames == 0);
}

/// A source missing at start is retried and goes on air once its file
/// appears, described as the file is: the size of its first frame, the
/// file's frame rate, and its audio, which an output started from then on
/// carries when the source is the program audio source.
#[test]
fn a_source_missing_at_start_goes_on_air_when_its_file_appears() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    }
    let path = temp_path("appears.mkv");
    let mut config = studio_with_file(&path);
    if let SourceConfig::File { audio, .. } = &mut config.sources[1].input {
        *audio = true;
    }
    config.output.audio_source = Some("later".into());
    let engine = start(config);
    let h = engine.handle();
    h.wait_until(WAIT, |s| {
        later(s).error.is_some() && s.sources[0].frames > 0
    })
    .expect("started with the file missing");
    // Written aside and moved into place, so no attempt opens half a file.
    let part = temp_path("appears.part.mkv");
    let made = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=30:duration=2",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
        ])
        .args(["-c:v", "mpeg4", "-q:v", "4", "-c:a", "pcm_s16le"])
        .arg(&part)
        .status()
        .unwrap();
    assert!(made.success(), "ffmpeg could not make a clip");
    std::fs::rename(&part, &path).unwrap();
    let snap = h
        .wait_until(WAIT, |s| {
            later(s).available && s.shot(&shot("later/wide")).is_some_and(|v| v.available)
        })
        .expect("the file goes on air");
    let on = later(&snap);
    assert_eq!(on.state, SourceState::Running);
    assert_eq!(on.error, None);
    assert_eq!((on.width, on.height), (160, 120), "its first frame's size");
    assert_eq!(on.nominal_fps, 30.0, "the file's rate, no longer unknown");
    assert!(snap.sources[0].available, "the stage kept running");
    // Its audio is the program's from the next output on.
    let record = temp_path("appears.mp4");
    assert_eq!(
        h.send(EngineCommand::StartOutput {
            rtmp_url: None,
            record_path: Some(record.clone()),
        }),
        Ok(CommandReply::OutputStarting)
    );
    h.wait_until(WAIT, |s| {
        s.output.state == OutputState::Running && s.output.audio.is_some_and(|a| a.pushed > 0)
    })
    .expect("the program carries the file's audio");
    h.send(EngineCommand::StopOutput).unwrap();
    let done = h
        .wait_until(WAIT, |s| {
            !matches!(s.output.state, OutputState::Stopping | OutputState::Running)
        })
        .expect("the encoder finishes");
    assert_eq!(
        done.output.state,
        OutputState::Finished,
        "{:?}",
        done.output
    );
    engine.shutdown().unwrap();
    let streams = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type"])
        .args(["-of", "csv=p=0"])
        .arg(&record)
        .output()
        .unwrap();
    let streams = String::from_utf8_lossy(&streams.stdout);
    assert!(streams.lines().any(|l| l.trim() == "audio"), "{streams}");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&record);
}
