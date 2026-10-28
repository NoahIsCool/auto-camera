//! Tests of the watcher's notes and of its thread, on snapshots built by a
//! real engine core.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use ac_output::{EncoderChoice, EncoderStats, Progress, StreamHealth};

use super::*;
use crate::logs::fixtures;

/// The texts of `notes` at `level`.
fn at(notes: &[Note], level: Level) -> Vec<&str> {
    notes
        .iter()
        .filter(|(l, _)| *l == level)
        .map(|(_, t)| t.as_str())
        .collect()
}

/// The texts of `notes`.
fn texts(notes: &[Note]) -> Vec<&str> {
    notes.iter().map(|(_, t)| t.as_str()).collect()
}

/// Encoder statistics with a stream in `state`.
fn streaming(state: StreamState) -> EncoderStats {
    EncoderStats {
        encoder: EncoderChoice::Libx264,
        frames_in: 90,
        frames_dropped: 0,
        frames_repeated: 0,
        frames_written: 90,
        audio_samples_in: 0,
        audio_samples_dropped: 0,
        audio_silence_samples: 0,
        audio_samples_written: 0,
        audio_connected: false,
        ffmpeg_alive: true,
        exit_code: None,
        last_error: None,
        progress: Progress::default(),
        stream: Some(StreamHealth {
            state,
            stalled: false,
            outages: 0,
            reconnects: 0,
            bitrate_kbps: None,
        }),
    }
}

#[test]
fn a_run_reads_as_one_line_per_change() {
    let mut w = Watcher::default();
    let mut snap = fixtures::snapshot();
    assert_eq!(
        texts(&w.observe(&snap)),
        [
            "engine running: 1 sources, 1 queries, director suggest",
            "source stage (stage): 320x180 at 30 fps, starting, detector loading",
        ]
    );
    assert!(w.observe(&snap).is_empty(), "nothing changed");

    snap.sources[0].state = SourceState::Running;
    snap.sources[0].discovery.preparing = false;
    snap.sources[0].discovery.ready_after = Some(Duration::from_millis(1500));
    let wide = snap.shots[0].clone();
    snap.switcher.program = Some(wide.id.clone());
    snap.director.mode = DirectorMode::Auto;
    let notes = w.observe(&snap);
    assert_eq!(
        texts(&notes),
        [
            "source stage: starting -> running",
            "source stage: detector ready at 1.5 s of media time",
            "director: auto",
            &format!("program: {} ({})", wide.name, wide.id.0),
        ]
    );
    assert!(notes.iter().all(|(l, _)| *l == Level::INFO));

    snap.output.state = OutputState::Running;
    snap.output.record_path = Some(PathBuf::from("/shows/a.mp4"));
    snap.output.rtmp_url = Some("rtmp://host/live/<redacted>".into());
    snap.output.stats = Some(streaming(StreamState::Connecting));
    assert_eq!(
        texts(&w.observe(&snap)),
        [
            "output running, recording /shows/a.mp4, streaming to rtmp://host/live/<redacted>",
            "stream connecting",
        ]
    );
    snap.output.stats = Some(streaming(StreamState::Connected));
    assert_eq!(texts(&w.observe(&snap)), ["stream connected"]);
    snap.output.stats = Some(streaming(StreamState::Reconnecting { attempt: 1 }));
    assert_eq!(
        at(&w.observe(&snap), Level::WARN),
        ["stream reconnecting (attempt 1)"]
    );
    // Further attempts of the same outage are ffmpeg's to log.
    snap.output.stats = Some(streaming(StreamState::Reconnecting { attempt: 2 }));
    assert!(w.observe(&snap).is_empty());

    snap.sources[0].state = SourceState::Stopped;
    snap.output.state = OutputState::Failed("ffmpeg exited with status 1".into());
    snap.output.stats = Some(streaming(StreamState::Down));
    let notes = w.observe(&snap);
    assert_eq!(
        at(&notes, Level::WARN),
        ["source stage: running -> stopped"]
    );
    assert_eq!(
        at(&notes, Level::ERROR),
        ["output failed: ffmpeg exited with status 1"]
    );
    assert_eq!(
        at(&notes, Level::INFO),
        ["stream down"],
        "not an outage now"
    );

    snap.running = false;
    snap.fatal = Some("the engine thread panicked: boom".into());
    snap.sidecar = Some(fixtures::sidecar());
    snap.switcher.program = None;
    let notes = w.observe(&snap);
    assert_eq!(
        texts(&notes),
        [
            "program: nothing",
            "sidecar: 2 starts (1 restarts), 1 failures, 0 timeouts over the run",
            "engine stopped: the engine thread panicked: boom",
        ]
    );
    assert_eq!(notes[2].0, Level::ERROR);
    assert!(w.observe(&snap).is_empty(), "nothing after the stop");
}

#[test]
fn a_clean_stop_and_a_finished_output_are_info() {
    let mut w = Watcher::default();
    let mut snap = fixtures::snapshot();
    w.observe(&snap);
    snap.output.state = OutputState::Finished;
    snap.output.stats = Some(streaming(StreamState::Down));
    snap.running = false;
    snap.tick = 300;
    snap.media_time = Duration::from_secs(10);
    let notes = w.observe(&snap);
    assert_eq!(
        texts(&notes),
        [
            "output finished: 90 frames written, 0 dropped",
            "stream down",
            "engine stopped after 300 ticks, 10.0 s of media time",
        ]
    );
    assert!(notes.iter().all(|(l, _)| *l == Level::INFO));
}

#[test]
fn detector_errors_and_timeouts_are_throttled() {
    let mut w = Watcher::default();
    let mut snap = fixtures::snapshot();
    w.observe(&snap);
    let mut step = |errors: u64, error: &str, timeouts: u64, secs: u64| {
        snap.media_time = Duration::from_secs(secs);
        let d = &mut snap.sources[0].discovery;
        d.errors = errors;
        d.last_error = Some(error.into());
        d.timeouts = timeouts;
        texts(&w.observe(&snap))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        step(1, "timed out after 1.2 s", 1, 0),
        [
            "source stage: detector error: timed out after 1.2 s",
            "source stage: a detector pass timed out"
        ]
    );
    // The same error with other numbers, soon after: no note.
    assert!(step(2, "timed out after 1.3 s", 3, 1).is_empty());
    // Another error: noted at once, with what came since.
    assert_eq!(
        step(3, "refused", 3, 2),
        ["source stage: 2 detector errors, the newest: refused"]
    );
    assert!(
        step(4, "refused", 3, 61).is_empty(),
        "59 s after the last note"
    );
    assert_eq!(
        step(5, "refused", 4, 62),
        [
            "source stage: 2 detector errors, the newest: refused",
            "source stage: 3 detector passes timed out"
        ]
    );
    assert!(step(6, "refused", 4, 63).is_empty());
    // At the stop, what was not noted yet.
    snap.running = false;
    let notes = w.observe(&snap);
    assert_eq!(
        texts(&notes)[..1],
        ["source stage: detector error: refused"]
    );
    assert_eq!(notes[0].0, Level::WARN);
}

#[test]
fn numbers_do_not_change_an_errors_shape() {
    assert_eq!(shape("after 1.25 s, pass 7"), "after #.# s, pass #");
    assert_eq!(shape(""), "");
    assert_eq!(shape("x86"), "x#");
    let mut t = Throttle::default();
    assert_eq!(t.update(0, None, Duration::ZERO), None, "no events");
    assert_eq!(t.update(2, None, Duration::ZERO), Some(2));
    assert_eq!(t.flush(), None);
    // A count that went down starts over.
    assert_eq!(t.update(1, None, Duration::from_secs(1)), Some(1));
}

/// Waits up to ten seconds for `done`.
fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A watcher thread on a snapshot the test swaps, with what it emitted
/// and published.
struct Rig {
    /// The snapshot the thread reads.
    snap: Arc<Mutex<Arc<EngineSnapshot>>>,
    /// The notes it emitted.
    notes: Arc<Mutex<Vec<String>>>,
    /// The summaries it published.
    summaries: Arc<Mutex<Vec<String>>>,
    /// The thread.
    watch: Watch,
}

impl Rig {
    /// Starts the thread on `snap`.
    fn new(snap: EngineSnapshot) -> Self {
        let snap = Arc::new(Mutex::new(Arc::new(snap)));
        let notes = Arc::new(Mutex::new(Vec::new()));
        let summaries = Arc::new(Mutex::new(Vec::new()));
        let watch = spawn(
            {
                let snap = snap.clone();
                move || snap.lock().unwrap().clone()
            },
            {
                let notes = notes.clone();
                move |_, text: &str| notes.lock().unwrap().push(text.to_owned())
            },
            {
                let summaries = summaries.clone();
                move |text: &str| summaries.lock().unwrap().push(text.to_owned())
            },
        );
        Self {
            snap,
            notes,
            summaries,
            watch,
        }
    }

    /// True once the thread emitted a note starting with `prefix`.
    fn noted(&self, prefix: &str) -> bool {
        self.notes
            .lock()
            .unwrap()
            .iter()
            .any(|n| n.starts_with(prefix))
    }
}

#[test]
fn the_thread_logs_publishes_and_ends_with_the_engine() {
    let rig = Rig::new(fixtures::snapshot());
    eventually("the first summary", || {
        !rig.summaries.lock().unwrap().is_empty()
    });
    assert!(rig.noted("engine running"));
    assert!(rig.summaries.lock().unwrap()[0].starts_with("engine: tick 0"));
    let mut stopped = fixtures::snapshot();
    stopped.running = false;
    *rig.snap.lock().unwrap() = Arc::new(stopped);
    eventually("the thread to end", || {
        rig.watch.thread.as_ref().unwrap().is_finished()
    });
    assert!(rig.noted("engine stopped after 0 ticks"));
    let last = rig.summaries.lock().unwrap().last().unwrap().clone();
    assert!(
        last.starts_with("engine: tick 0, media 0.000 s, stopped"),
        "{last}"
    );
    let summaries = rig.summaries.lock().unwrap().len();
    assert_eq!(summaries, 2, "one per new snapshot");
}

#[test]
fn dropping_the_watch_looks_once_more_and_joins() {
    let rig = Rig::new(fixtures::snapshot());
    eventually("the first note", || rig.noted("engine running"));
    let mut next = fixtures::snapshot();
    next.switcher.program = Some(next.shots[0].id.clone());
    *rig.snap.lock().unwrap() = Arc::new(next);
    let Rig { notes, watch, .. } = rig;
    drop(watch);
    assert!(
        notes
            .lock()
            .unwrap()
            .iter()
            .any(|n| n.starts_with("program: ")),
        "{:?}",
        notes.lock().unwrap()
    );
}
