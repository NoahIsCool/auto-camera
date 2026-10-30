//! The device input driven through a fake backend: the test plays the
//! device's callbacks (its buffers, an overload, the device going away),
//! so the input's worker, its health, its log lines and its supervision
//! run here as they would on CoreAudio. The log lines are read through a
//! subscriber that this test binary installs once as its global one, as
//! the worker logs from a thread of its own.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::*;
use crate::audio_input::supervised::SupervisedAudio;
use crate::clock::{Clock, WallClock};
use crate::status::lock;
use crate::supervisor::Backoff;
use ac_media::{AudioBlock, AudioInput, AudioInputInfo, SourceState};

/// The board's UID in [`table`].
const BOARD: &str = "AppleUSBAudioEngine:Allen&Heath:SQ:1A2B:1";

/// What a test does with a stream's sink inside `start`, before the
/// input's worker runs.
type OnStart = Box<dyn FnOnce(&mut CaptureSink) + Send>;

/// The callbacks of a stream the fake started, for the test to play.
struct Callbacks {
    /// The data callback's side.
    sink: CaptureSink,
    /// The error callback's side.
    events: StreamEvents,
    /// Raised when the input drops the stream.
    dropped: Arc<AtomicBool>,
}

/// A backend whose devices and streams the test controls.
#[derive(Default)]
struct FakeDevices {
    /// The devices listed.
    table: Mutex<Vec<AudioDevice>>,
    /// The host clock every stream reads, in nanoseconds.
    host: Arc<AtomicU64>,
    /// Streams started and not yet taken by the test, oldest first.
    started: Mutex<VecDeque<Callbacks>>,
    /// Calls of `start`.
    starts: AtomicUsize,
    /// Refuse to start, with this reason.
    refuse: Mutex<Option<String>>,
    /// Done inside the next `start`.
    on_start: Mutex<Option<OnStart>>,
    /// Advances this clock by this much inside `start`: a slow open.
    slow_open: Mutex<Option<(Arc<VirtualClock>, Duration)>>,
}

impl FakeDevices {
    /// A backend listing `table`, its host clock at `host`.
    fn new(table: Vec<AudioDevice>, host: Duration) -> Arc<Self> {
        let fake = Self::default();
        *lock(&fake.table) = table;
        fake.host.store(host.as_nanos() as u64, Ordering::SeqCst);
        Arc::new(fake)
    }

    /// The oldest started stream's callbacks, waiting up to 10 s for one.
    fn callbacks(&self) -> Callbacks {
        let mut taken = None;
        assert!(eventually(|| {
            taken = lock(&self.started).pop_front();
            taken.is_some()
        }));
        taken.unwrap()
    }
}

impl DeviceBackend for FakeDevices {
    fn devices(&self) -> Result<Vec<AudioDevice>, MediaError> {
        Ok(lock(&self.table).clone())
    }

    fn start(
        &self,
        device: &AudioDevice,
        mut sink: CaptureSink,
        events: StreamEvents,
    ) -> Result<Box<dyn DeviceStream>, MediaError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        assert!(lock(&self.table).contains(device), "{device}");
        if let Some(why) = lock(&self.refuse).clone() {
            return Err(MediaError::Open(why));
        }
        if let Some((clock, by)) = lock(&self.slow_open).take() {
            clock.advance(by);
        }
        if let Some(each) = lock(&self.on_start).take() {
            each(&mut sink);
        }
        let dropped = Arc::new(AtomicBool::new(false));
        lock(&self.started).push_back(Callbacks {
            sink,
            events,
            dropped: dropped.clone(),
        });
        Ok(Box::new(FakeStream {
            host: self.host.clone(),
            dropped,
        }))
    }
}

/// A stream of [`FakeDevices`].
struct FakeStream {
    /// The backend's host clock.
    host: Arc<AtomicU64>,
    /// Raised on drop.
    dropped: Arc<AtomicBool>,
}

impl DeviceStream for FakeStream {
    fn host_now(&self) -> Duration {
        Duration::from_nanos(self.host.load(Ordering::SeqCst))
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
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

/// The input's id.
fn board() -> SourceId {
    SourceId("board".into())
}

/// Opens the board among `backend`'s devices with `options`, paced by
/// `clock`.
fn open(
    backend: &FakeDevices,
    options: DeviceOptions,
    clock: Arc<dyn Clock>,
) -> Result<DeviceInput, MediaError> {
    DeviceInput::open(board(), BOARD, backend, options, clock)
}

/// Delivers block `k` (480 frames of 32 channels) captured at `start +
/// 10 ms · k` into `sink`.
fn play(sink: &mut CaptureSink, start: Duration, k: u64) {
    sink.deliver(start + ms(10 * k), &frames(480 * k, 480, 32));
}

/// Drains `input` until it has delivered `n` blocks in all.
fn drain_until(input: &dyn AudioInput, got: &mut Vec<AudioBlock>, n: usize) {
    assert!(
        eventually(|| {
            got.extend(input.drain());
            got.len() >= n
        }),
        "{} of {n} blocks",
        got.len()
    );
}

#[test]
fn an_input_opens_its_device_and_stamps_capture_times_on_the_clock() {
    let start = Duration::from_secs(1000);
    let backend = FakeDevices::new(table(), start);
    let clock = Arc::new(VirtualClock::new());
    // Opening takes 100 ms of the pacing clock: PTS 0 is when it began.
    *lock(&backend.slow_open) = Some((clock.clone(), ms(100)));
    let mut input = open(&backend, DeviceOptions::default(), clock).unwrap();
    assert_eq!(input.info().format, Some((48_000, 32)));
    assert_eq!(input.device().name, "SQ");
    assert_eq!(input.state(), SourceState::Starting);
    let mut cb = backend.callbacks();
    for k in 0..10 {
        play(&mut cb.sink, start, k);
    }
    let mut got = Vec::new();
    drain_until(&input, &mut got, 10);
    for (k, b) in got.iter().enumerate() {
        let k = k as u64;
        assert_eq!((b.seq, b.pts), (k, ms(100 + 10 * k)), "block {k}");
        assert_eq!(
            (b.source.clone(), b.sample_rate, b.channels),
            (board(), 48_000, 32)
        );
        assert_eq!(b.samples[..], frames(480 * k, 480, 32)[..]);
    }
    assert_eq!(input.state(), SourceState::Running);
    assert_eq!((input.error(), input.discontinuities()), (None, 0));
    assert_eq!(input.overloads(), 0);
    assert_eq!(input.dropped(), 0);
    assert!(!cb.dropped.load(Ordering::SeqCst));
    input.stop();
    assert_eq!(input.state(), SourceState::Stopped);
    assert!(cb.dropped.load(Ordering::SeqCst), "the stream is stopped");
    assert_eq!(input.error(), None);
}

#[test]
fn a_missing_device_is_never_replaced_by_another() {
    let backend = FakeDevices::new(table(), Duration::ZERO);
    let message = open_error(DeviceInput::open(
        board(),
        "AppleUSBAudioEngine:Allen&Heath:SQ:FFFF:1",
        &*backend,
        DeviceOptions::default(),
        Arc::new(VirtualClock::new()),
    ));
    assert!(
        message.contains("no other device is opened in its place"),
        "{message}"
    );
    assert!(message.contains(BOARD), "the UIDs there are: {message}");
    assert_eq!(
        backend.starts.load(Ordering::SeqCst),
        0,
        "nothing was started"
    );
}

#[test]
fn a_stream_that_will_not_start_fails_the_open_naming_the_device() {
    let backend = FakeDevices::new(table(), Duration::ZERO);
    *lock(&backend.refuse) = Some("the device is busy".into());
    let message = open_error(open(
        &backend,
        DeviceOptions::default(),
        Arc::new(VirtualClock::new()),
    ));
    assert_eq!(
        message,
        format!(
            "cannot open audio device \"{BOARD}\" (SQ, 32 input channels at 48000 Hz): \
             the device is busy"
        )
    );
}

#[test]
fn discontinuities_are_counted_and_shown_while_the_input_runs() {
    let start = Duration::from_secs(5);
    let backend = FakeDevices::new(table(), start);
    let mut input = open(
        &backend,
        DeviceOptions::default(),
        Arc::new(VirtualClock::new()),
    )
    .unwrap();
    let mut cb = backend.callbacks();
    play(&mut cb.sink, start, 0);
    play(&mut cb.sink, start, 1);
    // Block 2 is due at 20 ms and captured at 50 ms, and the stream stays
    // there: a gap, reported once its window has passed.
    for k in 2..=53 {
        play(&mut cb.sink, start + ms(30), k);
    }
    let mut got = Vec::new();
    drain_until(&input, &mut got, 54);
    assert!(eventually(|| input.discontinuities() == 1));
    assert_eq!(
        input.error().as_deref(),
        Some(
            "audio discontinuity (1 so far): 30.0 ms of audio missing before block 2 \
             (its host time jumped ahead)"
        )
    );
    assert_eq!(input.state(), SourceState::Running, "it keeps delivering");
    assert_eq!(got[2].pts, ms(50), "the gap is kept");
    // An overload that lost nothing: shown, on its own count.
    cb.events.overload();
    assert!(eventually(|| input.error().as_deref()
        == Some(
            "the device reported an overload (1 so far; 1 audio discontinuity): its I/O \
             cycle ran late, so its audio may have a gap"
        )));
    assert_eq!((input.discontinuities(), input.overloads()), (1, 1));
    play(&mut cb.sink, start + ms(30), 54);
    drain_until(&input, &mut got, 55);
    assert_eq!(input.state(), SourceState::Running);
    input.stop();
}

#[test]
fn an_overload_that_loses_audio_is_one_overload_and_one_discontinuity() {
    let start = Duration::from_secs(5);
    let backend = FakeDevices::new(table(), start);
    let mut input = open(
        &backend,
        DeviceOptions::default(),
        Arc::new(VirtualClock::new()),
    )
    .unwrap();
    let mut cb = backend.callbacks();
    let mut got = Vec::new();
    let (mut k, mut late) = (0u64, Duration::ZERO);
    for round in 1..=5u64 {
        for _ in 0..3 {
            play(&mut cb.sink, start + late, k);
            k += 1;
        }
        // The device's cycle ran late and lost a block's audio: it reports
        // an overload, and the next block comes 10 ms later than due.
        cb.events.overload();
        assert!(eventually(|| input.overloads() == round));
        if round == 1 {
            assert!(eventually(|| input.error().as_deref()
                == Some(
                    "the device reported an overload (1 so far): its I/O cycle ran late, \
                     so its audio may have a gap"
                )));
        }
        late += ms(10);
        let jumped = k;
        // On past the gap's window.
        for _ in 0..55 {
            play(&mut cb.sink, start + late, k);
            k += 1;
        }
        drain_until(&input, &mut got, k as usize);
        let notice = format!(
            "audio discontinuity ({round} so far; the device reported {round} overload{}): \
             10.0 ms of audio missing before block {jumped} (its host time jumped ahead)",
            if round == 1 { "" } else { "s" }
        );
        assert!(
            eventually(|| input.error().as_deref() == Some(notice.as_str())),
            "{:?}, not {notice}",
            input.error()
        );
        assert_eq!(
            (input.discontinuities(), input.overloads()),
            (round, round),
            "counted once each"
        );
    }
    input.stop();
}

#[test]
fn an_overflow_drops_blocks_counts_them_and_says_so() {
    let start = Duration::from_secs(5);
    let backend = FakeDevices::new(table(), start);
    // Five blocks arrive before the worker first looks: two buffers.
    *lock(&backend.on_start) = Some(Box::new(move |sink: &mut CaptureSink| {
        for k in 0..5 {
            play(sink, start, k);
        }
    }));
    let options = DeviceOptions {
        pool_buffers: 2,
        ..DeviceOptions::default()
    };
    let mut input = open(&backend, options, Arc::new(VirtualClock::new())).unwrap();
    let mut cb = backend.callbacks();
    let mut got = Vec::new();
    drain_until(&input, &mut got, 2);
    assert_eq!(input.dropped(), 3, "counted at once");
    play(&mut cb.sink, start, 5);
    drain_until(&input, &mut got, 3);
    assert_eq!(
        seqs(&got),
        [0, 1, 5],
        "the dropped blocks keep their numbers"
    );
    assert_eq!(got[2].pts, ms(50));
    assert!(eventually(|| input.discontinuities() == 1));
    assert_eq!(
        input.error().as_deref(),
        Some(
            "audio discontinuity (1 so far): 3 block(s) (1440 sample frames, 30.0 ms) \
             dropped before block 5: the input fell behind its device"
        )
    );
    input.stop();
}

#[test]
fn a_device_that_goes_away_stops_the_input_with_why() {
    let start = Duration::from_secs(5);
    let backend = FakeDevices::new(table(), start);
    let mut input = open(
        &backend,
        DeviceOptions::default(),
        Arc::new(VirtualClock::new()),
    )
    .unwrap();
    let mut cb = backend.callbacks();
    for k in 0..3 {
        play(&mut cb.sink, start, k);
    }
    let mut got = Vec::new();
    drain_until(&input, &mut got, 3);
    cb.events.lost("Device disconnected");
    assert!(eventually(|| input.state() == SourceState::Stopped));
    assert_eq!(
        input.error().as_deref(),
        Some("audio device \"SQ\" went away: Device disconnected")
    );
    assert!(
        cb.dropped.load(Ordering::SeqCst),
        "the worker dropped the stream"
    );
    input.stop();
    assert_eq!(seqs(&got), [0, 1, 2]);
}

#[test]
fn a_silent_device_fails_after_its_timeouts() {
    let quick = DeviceOptions {
        open_timeout: ms(100),
        stall_timeout: ms(100),
        ..DeviceOptions::default()
    };
    let backend = FakeDevices::new(table(), Duration::ZERO);
    let mut mute = open(&backend, quick, Arc::new(WallClock::new())).unwrap();
    assert!(eventually(|| mute.state() == SourceState::Stopped));
    assert_eq!(
        mute.error().as_deref(),
        Some("audio device \"SQ\" delivered no audio within 0.1 s")
    );
    mute.stop();
    // Only the stall is quick here, so a slow test thread between the open
    // and the first block cannot meet the open timeout instead.
    let stall_only = DeviceOptions {
        stall_timeout: ms(100),
        ..DeviceOptions::default()
    };
    let mut stalled = open(&backend, stall_only, Arc::new(WallClock::new())).unwrap();
    let _first = backend.callbacks();
    let mut cb = backend.callbacks();
    play(&mut cb.sink, Duration::ZERO, 0);
    assert!(eventually(|| stalled.state() == SourceState::Stopped));
    assert_eq!(stalled.drain().len(), 1, "what came is kept");
    assert_eq!(
        stalled.error().as_deref(),
        Some("audio device \"SQ\" delivered no audio for 0.1 s")
    );
    stalled.stop();
}

#[test]
fn a_supervisor_shows_the_trouble_and_reopens_the_board_when_it_is_back() {
    let start = Duration::from_secs(5);
    let backend = FakeDevices::new(table(), start);
    let clock: Arc<dyn Clock> = Arc::new(WallClock::new());
    let (devices, pacing) = (backend.clone(), clock.clone());
    let opener = move || -> Result<Box<dyn AudioInput>, MediaError> {
        let input = DeviceInput::open(
            board(),
            BOARD,
            &*devices,
            DeviceOptions::default(),
            pacing.clone(),
        )?;
        Ok(Box::new(input))
    };
    let offline = AudioInputInfo {
        id: board(),
        format: None,
    };
    let quick = Backoff {
        initial: ms(20),
        max: ms(40),
    };
    let mut input = SupervisedAudio::start(opener, offline, clock, quick).unwrap();
    let mut cb = backend.callbacks();
    // The gap after the second block: one at the second is taken for the
    // first block's host time being off (see `timing`).
    play(&mut cb.sink, start, 0);
    play(&mut cb.sink, start, 1);
    for k in 2..=53 {
        play(&mut cb.sink, start + ms(30), k);
    }
    let mut got = Vec::new();
    drain_until(&input, &mut got, 54);
    // The gap is shown while the board stays on air.
    assert!(eventually(|| input.error().is_some_and(
        |e| e.starts_with("audio discontinuity (1 so far)")
    )));
    assert_eq!(input.state(), SourceState::Running);
    // Unplugged: the board is gone from the table, and its stream says so.
    lock(&backend.table).retain(|d| d.uid != BOARD);
    cb.events.lost("Device disconnected");
    assert!(eventually(|| input.error().is_some_and(
        |e| e.contains("no other device is opened in its place")
    )));
    assert_ne!(input.state(), SourceState::Running);
    assert_eq!(
        backend.starts.load(Ordering::SeqCst),
        1,
        "nothing else opened"
    );
    // Plugged back in: reopened, the error cleared, the numbers going on.
    lock(&backend.table).clone_from(&table());
    let mut again = backend.callbacks();
    for k in 0..3 {
        play(&mut again.sink, start + ms(500), k);
    }
    drain_until(&input, &mut got, 57);
    assert_eq!(input.state(), SourceState::Running);
    assert_eq!(input.error(), None);
    assert_eq!(seqs(&got), (0..57).collect::<Vec<u64>>());
    assert!(got.windows(2).all(|w| w[0].pts < w[1].pts), "PTS go on");
    input.stop();
    assert!(again.dropped.load(Ordering::SeqCst));
}

/// The sequence numbers of `blocks`.
fn seqs(blocks: &[AudioBlock]) -> Vec<u64> {
    blocks.iter().map(|b| b.seq).collect()
}

/// Log lines as (the event's `input` field, its message), in order.
type Lines = Arc<Mutex<Vec<(String, String)>>>;

/// The log lines of the device inputs' target, from the subscriber this
/// test binary installs as its global one on the first call.
fn logged() -> Lines {
    /// The lines, shared with the subscriber.
    static LINES: OnceLock<Lines> = OnceLock::new();
    LINES
        .get_or_init(|| {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let capture = Capture {
                lines: lines.clone(),
                spans: AtomicU64::new(1),
            };
            tracing::subscriber::set_global_default(capture)
                .expect("no other global subscriber in this test binary");
            lines
        })
        .clone()
}

/// A subscriber that keeps the device inputs' log lines.
struct Capture {
    /// The lines, in order.
    lines: Lines,
    /// Next span id (spans are not used, but must get ids).
    spans: AtomicU64,
}

/// An event's `input` field and message.
#[derive(Default)]
struct Fields {
    /// Its `input` field.
    input: String,
    /// Its message.
    message: String,
}

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        match field.name() {
            "input" => self.input = format!("{value:?}"),
            "message" => self.message = format!("{value:?}"),
            _ => {}
        }
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        // The target of the device inputs' own lines (see `input`).
        metadata.target() == "ac_sources::audio_device"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(self.spans.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        lock(&self.lines).push((fields.input, fields.message));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn an_overloads_log_line_leaves_the_gap_it_left_a_line_of_its_own() {
    let lines = logged();
    let start = Duration::from_secs(5);
    let backend = FakeDevices::new(table(), start);
    let id = SourceId("logged-board".into());
    let clock = Arc::new(VirtualClock::new());
    let mut input = DeviceInput::open(
        id.clone(),
        BOARD,
        &*backend,
        DeviceOptions::default(),
        clock,
    )
    .unwrap();
    let mut cb = backend.callbacks();
    for k in 0..3 {
        play(&mut cb.sink, start, k);
    }
    // The cycle ran late and lost a block's audio: an overload now, the
    // gap half a second of audio later.
    cb.events.overload();
    assert!(eventually(|| input.overloads() == 1));
    for k in 3..60 {
        play(&mut cb.sink, start + ms(10), k);
    }
    let mut got = Vec::new();
    drain_until(&input, &mut got, 60);
    let mine = || -> Vec<String> {
        lock(&lines)
            .iter()
            .filter(|(input, _)| *input == id.0)
            .map(|(_, message)| message.clone())
            .collect()
    };
    let gap = "audio discontinuity (1 so far; the device reported 1 overload): 10.0 ms of \
               audio missing before block 3 (its host time jumped ahead)";
    assert!(
        eventually(|| mine().iter().any(|m| m == gap)),
        "{:?}",
        mine()
    );
    let overload = "the device reported an overload (1 so far): its I/O cycle ran late, so its \
                    audio may have a gap";
    assert!(mine().iter().any(|m| m == overload), "{:?}", mine());
    input.stop();
}
