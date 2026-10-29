//! Tests for the source supervisor: the backoff and restamping rules step by
//! step, and whole outages on a virtual clock with a fault-injecting source.

use super::*;
use crate::clock::{VirtualClock, WallClock};
use crate::fault::{FlakySynthetic, OpenAttempt, Outage};
use crate::synthetic::SyntheticPattern;
use ac_core::SourceId;
use std::sync::atomic::AtomicU32;
use std::time::Instant;

/// `s` seconds.
fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

/// `v` milliseconds.
fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

/// The test source id.
fn cam() -> SourceId {
    SourceId("cam".into())
}

/// What a source that never opened says about itself.
fn offline_info() -> SourceInfo {
    SourceInfo {
        id: cam(),
        width: 64,
        height: 36,
        fps: 0.0,
        audio: None,
    }
}

/// Polls `cond` every millisecond until it holds or ten seconds pass.
fn eventually(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(ms(1));
    }
    cond()
}

/// A 2x1 frame of `cam` with sequence number `seq` at `pts`.
fn frame(seq: u64, pts: Duration) -> VideoFrame {
    VideoFrame::new(cam(), seq, pts, 2, 1, Arc::from(vec![0u8; 8])).unwrap()
}

/// A 10 ms mono block at 1 kHz starting at `pts`.
fn block(pts: Duration) -> AudioBlock {
    AudioBlock {
        source: cam(),
        seq: 0,
        pts,
        sample_rate: 1000,
        channels: 1,
        samples: Arc::from(vec![0.0f32; 10]),
    }
}

/// A connection that published everything up front and then stopped, with
/// `error` or cleanly.
struct Scripted {
    /// Description.
    info: SourceInfo,
    /// Holds the newest of its frames.
    latest: LatestFrame,
    /// Its audio, until drained.
    audio: Mutex<Vec<AudioBlock>>,
    /// Why it stopped, if it failed.
    error: Option<String>,
}

impl Scripted {
    /// A stopped connection that published `frames` and `audio`.
    fn new(frames: Vec<VideoFrame>, audio: Vec<AudioBlock>, error: Option<&str>) -> Box<Self> {
        let latest = LatestFrame::new();
        for f in frames {
            latest.publish(f);
        }
        Box::new(Self {
            info: offline_info(),
            latest,
            audio: Mutex::new(audio),
            error: error.map(String::from),
        })
    }
}

impl VideoSource for Scripted {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        SourceState::Stopped
    }

    fn latest(&self) -> Option<VideoFrame> {
        self.latest.get()
    }

    fn wait_newer(&self, after: Option<u64>, timeout: Duration) -> Option<VideoFrame> {
        self.latest.wait_newer(after, timeout)
    }

    fn drain_audio(&self) -> Vec<AudioBlock> {
        std::mem::take(&mut *lock(&self.audio))
    }

    fn error(&self) -> Option<String> {
        self.error.clone()
    }

    fn stop(&mut self) {}
}

#[test]
fn delays_double_up_to_the_cap() {
    let b = Backoff::default();
    let delays: Vec<Duration> = (1..=7).map(|n| b.delay(n)).collect();
    assert_eq!(
        delays,
        [
            ms(500),
            ms(1000),
            ms(2000),
            ms(4000),
            ms(8000),
            ms(8000),
            ms(8000)
        ]
    );
    assert_eq!(
        b.delay(0),
        ms(500),
        "no failure yet still waits the initial delay"
    );
    assert_eq!(b.delay(u32::MAX), ms(8000));
    assert!(b.validate().is_ok());
    for bad in [
        Backoff {
            initial: Duration::ZERO,
            max: ms(10),
        },
        Backoff {
            initial: ms(20),
            max: ms(10),
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}

#[test]
fn a_connection_that_stayed_up_resets_the_delay_and_a_flapping_one_does_not() {
    let mut l = Ledger::new(Backoff::default(), Duration::ZERO);
    assert_eq!(l.failed(secs(1.0), None), ms(500));
    assert_eq!(l.failed(secs(2.0), None), ms(1000));
    // Up for 3 s only: it keeps backing off.
    assert_eq!(l.failed(secs(6.0), Some(secs(3.0))), ms(2000));
    // Up for the cap: the next delay is the initial one again.
    assert_eq!(l.failed(secs(20.0), Some(secs(12.0))), ms(500));
    assert_eq!(l.failed(secs(21.0), None), ms(1000));
}

#[test]
fn the_first_connection_is_stamped_as_is_and_later_ones_continue() {
    let mut l = Ledger::new(Backoff::default(), secs(100.0));
    let first = l.connect(secs(100.0));
    assert_eq!(
        first,
        Stamp {
            epoch: 0,
            seq: 0,
            audio_seq: 0,
            pts: Duration::ZERO
        }
    );
    let f = first.frame(frame(299, ms(9_900)));
    assert_eq!((f.epoch, f.seq, f.pts), (0, 299, ms(9_900)));
    l.frame_delivered(&f);
    // Opened 13.5 s after the supervisor started: PTS from the clock.
    let second = l.connect(secs(113.5));
    assert_eq!(
        second,
        Stamp {
            epoch: 1,
            seq: 300,
            audio_seq: 0,
            pts: ms(13_500)
        }
    );
    let g = second.frame(frame(2, ms(100)));
    assert_eq!((g.epoch, g.seq, g.pts), (1, 302, ms(13_600)));
    l.frame_delivered(&g);
    let a = second.audio(AudioBlock {
        seq: 4,
        ..block(ms(200))
    });
    assert_eq!((a.seq, a.pts), (4, ms(13_700)));
    l.audio_delivered(&a);
    // A clock that did not move past what was delivered (the end of that
    // audio): just after it, never at or before. Blocks are numbered on
    // after the newest delivered, as frames are.
    let third = l.connect(secs(101.0));
    assert_eq!((third.epoch, third.seq, third.audio_seq), (2, 303, 5));
    assert_eq!(third.audio(block(ms(0))).seq, 5);
    assert_eq!(third.pts, a.pts + a.duration() + Duration::from_nanos(1));
    // A clock reading before the origin counts as the origin.
    let mut early = Ledger::new(Backoff::default(), secs(5.0));
    assert_eq!(early.connect(secs(1.0)).pts, Duration::ZERO);
}

#[test]
fn a_source_that_fails_at_10_s_and_returns_at_13_s_resumes_under_a_new_epoch() {
    let clock = Arc::new(VirtualClock::new());
    let pattern = SyntheticPattern::new(64, 36, 30.0, 2).unwrap();
    let outage = Outage {
        from: secs(10.0),
        until: secs(13.0),
    };
    let mut flaky = FlakySynthetic::new(cam(), pattern.clone(), clock.clone(), vec![outage]);
    let (log, tried) = (flaky.log(), flaky.log());
    // The first attempt after the failure waits until the test has looked:
    // on a virtual clock the whole outage passes in microseconds.
    let (arrived, at_retry) = std::sync::mpsc::channel();
    let (resume, go) = std::sync::mpsc::channel::<()>();
    let opener = move || {
        if tried.attempts().len() == 1 {
            // Bounded, so a failed assertion cannot leave the supervisor
            // (and the test's stop) waiting forever.
            let _ = arrived.send(());
            let _ = go.recv_timeout(Duration::from_secs(30));
        }
        flaky.open()
    };
    let mut src =
        SupervisedSource::start(opener, offline_info(), clock.clone(), Backoff::default()).unwrap();
    assert_eq!(src.start_error(), None);
    assert_eq!(
        (src.info().width, src.info().height),
        (64, 36),
        "the input's own"
    );
    assert_eq!(&src.current_info(), src.info());
    at_retry.recv_timeout(Duration::from_secs(10)).unwrap();
    // The connection failed on the frame due at 10 s; 0.5 s later the
    // supervisor is trying again, saying why.
    assert_eq!(clock.now(), secs(10.5));
    assert_eq!(src.state(), SourceState::Reconnecting);
    assert!(src.error().unwrap().contains("injected fault"));
    let last_before = src.latest().unwrap();
    let rate = pattern.frame_rate();
    assert_eq!((last_before.epoch, last_before.seq), (0, 299));
    assert_eq!(last_before.pts, rate.pts(299));
    resume.send(()).unwrap();
    let mut seen = vec![last_before];
    assert!(eventually(|| {
        if let Some(f) = src.wait_newer(seen.last().map(|f| f.seq), ms(5)) {
            seen.push(f);
        }
        seen.last().is_some_and(|f| f.pts >= secs(14.0))
    }));
    // Tried 0.5, 1 and 2 s after the failure, and got through on the third
    // attempt, 0.5 s after the input came back: within the 2 s delay.
    assert_eq!(
        log.attempts(),
        [(0.0, true), (10.5, false), (11.5, false), (13.5, true)].map(|(at, opened)| {
            OpenAttempt {
                at: secs(at),
                opened,
            }
        })
    );
    // The new connection's frame 0 is frame 300, stamped at 13.5 s, and the
    // pattern plays from its start again.
    for f in &seen[1..] {
        assert_eq!((f.epoch, f.source.clone()), (1, cam()));
        assert!(f.seq >= 300, "{}", f.seq);
        let inner = f.seq - 300;
        assert_eq!(f.pts, secs(13.5) + rate.pts(inner));
        assert_eq!(f.data, pattern.frame(&cam(), inner).data);
    }
    assert!(
        seen.windows(2)
            .all(|w| w[0].seq < w[1].seq && w[0].pts < w[1].pts)
    );
    assert_eq!(src.state(), SourceState::Running);
    assert_eq!(src.error(), None, "running again");
    assert_eq!(&src.current_info(), src.info(), "the same input again");
    src.stop();
    assert_eq!(src.state(), SourceState::Stopped);
}

#[test]
fn a_source_missing_at_start_starts_offline_and_connects_when_it_appears() {
    let clock = Arc::new(VirtualClock::new());
    let pattern = SyntheticPattern::new(32, 18, 30.0, 1).unwrap();
    let outage = Outage {
        from: Duration::ZERO,
        until: secs(5.0),
    };
    let opener = FlakySynthetic::new(cam(), pattern, clock.clone(), vec![outage]);
    let log = opener.log();
    let src = SupervisedSource::start(opener, offline_info(), clock, Backoff::default()).unwrap();
    assert!(src.start_error().unwrap().contains("gone until"));
    assert_eq!(src.info(), &offline_info(), "described by the caller");
    assert_eq!(src.current_info(), offline_info(), "until it delivers");
    let mut first = None;
    assert!(eventually(|| {
        first = first.take().or_else(|| src.wait_newer(None, ms(5)));
        first.is_some()
    }));
    // Whoever sees its first frame sees the input's own description.
    let own = SourceInfo {
        id: cam(),
        width: 32,
        height: 18,
        fps: 30.0,
        audio: None,
    };
    assert_eq!(src.current_info(), own);
    assert_eq!(src.info(), &offline_info(), "fixed at start");
    let at: Vec<f64> = log.attempts().iter().map(|a| a.at.as_secs_f64()).collect();
    assert_eq!(at, [0.0, 0.5, 1.5, 3.5, 7.5]);
    let first = first.unwrap();
    assert_eq!(first.epoch, 0, "its first connection");
    assert!(first.pts >= secs(7.5), "media time counts from the start");
    assert!(eventually(|| src.state() == SourceState::Running));
    assert_eq!(src.error(), None);
}

#[test]
fn audio_and_gaps_carry_over_and_a_clean_end_is_not_reopened() {
    let clock = Arc::new(VirtualClock::new());
    let opened = Arc::new(AtomicU32::new(0));
    let count = opened.clone();
    let opener = move || -> Result<Box<dyn VideoSource>, MediaError> {
        Ok(match count.fetch_add(1, Ordering::SeqCst) {
            0 => Scripted::new(
                (0..3).map(|i| frame(i, ms(33 * i))).collect(),
                (0..3).map(|i| block(ms(10 * i))).collect(),
                Some("lost"),
            ),
            _ => Scripted::new(
                (0..2).map(|i| frame(i, ms(33 * i))).collect(),
                vec![block(Duration::ZERO)],
                None,
            ),
        })
    };
    let src = SupervisedSource::start(opener, offline_info(), clock, Backoff::default()).unwrap();
    assert!(eventually(|| src.state() == SourceState::Stopped));
    let last = src.latest().unwrap();
    // Frames 0 and 1 of the first connection were replaced in its mailbox
    // (a gap of two, kept); the second connection starts at 3, 0.5 s in.
    assert_eq!((last.epoch, last.seq, last.pts), (1, 4, ms(533)));
    let pts: Vec<Duration> = src.drain_audio().iter().map(|b| b.pts).collect();
    assert_eq!(pts, [ms(0), ms(10), ms(20), ms(500)]);
    assert_eq!(src.error(), None, "it ended");
    std::thread::sleep(ms(20));
    assert_eq!(opened.load(Ordering::SeqCst), 2, "a clean end is final");
    assert_eq!(src.state(), SourceState::Stopped);
}

#[test]
fn stop_ends_an_offline_source_at_once_even_during_a_long_backoff() {
    let attempts = Arc::new(AtomicU32::new(0));
    let count = attempts.clone();
    let opener = move || -> Result<Box<dyn VideoSource>, MediaError> {
        count.fetch_add(1, Ordering::SeqCst);
        Err(MediaError::Open("no such device".into()))
    };
    let backoff = Backoff {
        initial: secs(30.0),
        max: secs(30.0),
    };
    let mut src =
        SupervisedSource::start(opener, offline_info(), Arc::new(WallClock::new()), backoff)
            .unwrap();
    assert_eq!(src.state(), SourceState::Offline);
    assert_eq!(
        src.error().as_deref(),
        Some("cannot open source: no such device")
    );
    assert!(src.latest().is_none() && src.drain_audio().is_empty());
    assert!(src.wait_newer(None, ms(5)).is_none());
    std::thread::sleep(ms(20));
    let asked = Instant::now();
    src.stop();
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(src.state(), SourceState::Stopped);
    src.stop();
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "only the first attempt");
}

#[test]
fn unusable_backoffs_are_rejected() {
    let opener = || -> Result<Box<dyn VideoSource>, MediaError> {
        panic!("never opened");
    };
    let backoff = Backoff {
        initial: Duration::ZERO,
        max: secs(1.0),
    };
    let clock = Arc::new(VirtualClock::new());
    assert!(matches!(
        SupervisedSource::start(opener, offline_info(), clock, backoff),
        Err(MediaError::Open(_))
    ));
}

#[test]
fn a_supervised_config_opens_like_an_unsupervised_one() {
    let config = SourceConfig::Synthetic {
        width: 32,
        height: 18,
        fps: 30.0,
        subjects: 1,
    };
    let clock = Arc::new(VirtualClock::new());
    let mut src = open_supervised(&config, offline_info(), clock, Backoff::default()).unwrap();
    assert_eq!(src.start_error(), None);
    assert_eq!(src.info().fps, 30.0);
    let pattern = SyntheticPattern::new(32, 18, 30.0, 1).unwrap();
    let f = src.wait_newer(Some(5), Duration::from_secs(10)).unwrap();
    assert_eq!((f.epoch, f.pts), (0, pattern.frame_rate().pts(f.seq)));
    assert_eq!(f.data, pattern.frame(&cam(), f.seq).data);
    src.stop();
    assert_eq!(src.state(), SourceState::Stopped);
}
