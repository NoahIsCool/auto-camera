//! What the source decoders leave in the app's log, seen through a
//! subscriber of this binary's own: it holds one test, so the subscriber
//! is the process's global one before any decoder starts.
//!
//! A clean stop must leave nothing there. A decoder that finds its pipe
//! closed while it is still alive says "Broken pipe" on stderr, which reads
//! like an error of the file's, so every decoder must be dead before its
//! pipe closes. A stand-in for ffmpeg that writes one line to stderr and
//! then runs the real one shows that decoder lines do reach the log, as
//! warnings that name the input, so the empty result means something.

#![cfg(unix)]

mod common;

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ac_core::SourceId;
use ac_media::{SourceState, VideoSource};
use ac_sources::{FfmpegTools, FileOptions, FileSource};
use common::{TempDir, make_clip, poll, tools};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// What the stand-in for ffmpeg writes to stderr before it runs ffmpeg.
const HELLO: &str = "decoder starting (test stand-in)";

/// The target of decoder lines in the log.
const TARGET: &str = "ac_sources::ffmpeg";

/// One captured event.
#[derive(Debug, Clone)]
struct Captured {
    /// Its level.
    level: Level,
    /// Its target.
    target: String,
    /// Its message.
    message: String,
    /// Its `input` field, if any.
    input: String,
}

/// A subscriber that keeps every event.
#[derive(Default)]
struct Capture {
    /// The events, in order.
    events: Arc<Mutex<Vec<Captured>>>,
    /// Next span id (spans are not used, but must get ids).
    spans: AtomicU64,
}

/// Collects an event's message and `input` field.
struct Fields<'a>(&'a mut Captured);

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => drop(write!(self.0.message, "{value:?}")),
            "input" => drop(write!(self.0.input, "{value:?}")),
            _ => {}
        }
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(self.spans.fetch_add(1, Ordering::Relaxed) + 1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let mut captured = Captured {
            level: *meta.level(),
            target: meta.target().to_owned(),
            message: String::new(),
            input: String::new(),
        };
        event.record(&mut Fields(&mut captured));
        self.events.lock().unwrap().push(captured);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[test]
fn a_clean_stop_leaves_no_decoder_errors_in_the_log() {
    let Some(tools) = tools() else { return };
    let capture = Capture::default();
    let events = capture.events.clone();
    tracing::subscriber::set_global_default(capture).unwrap();
    let dir = TempDir::new("decoder-log");
    // The audio outlasts the video, so every loop also cuts an audio pass
    // short, killing a decoder that is still writing.
    let clip = make_clip(&tools, &dir.0, 0.5, Some(1.0));
    let stand_in = dir.0.join("ffmpeg-stand-in");
    let script = format!(
        "#!/bin/sh\necho '{HELLO}' >&2\nexec '{}' \"$@\"\n",
        tools.ffmpeg.display()
    );
    std::fs::write(&stand_in, script).unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    let options = FileOptions {
        loop_playback: true,
        tools: FfmpegTools {
            ffmpeg: stand_in,
            ..tools
        },
        ..FileOptions::default()
    };
    // Real-time pacing, so every decoder waits on a full pipe; stopped
    // before the first loop, just after it and after a few.
    for millis in [200, 600, 1300, 100] {
        let mut src = FileSource::open(SourceId("file".into()), &clip, options.clone()).unwrap();
        assert!(poll(Duration::from_secs(20), || src.state()
            == SourceState::Running));
        std::thread::sleep(Duration::from_millis(millis));
        src.stop();
        assert_eq!(src.error(), None);
    }
    // Each drain ends when its decoder is reaped, just after the stop.
    std::thread::sleep(Duration::from_millis(300));
    let events = events.lock().unwrap().clone();
    let (hello, other): (Vec<_>, Vec<_>) = events
        .into_iter()
        .filter(|e| e.target == TARGET)
        .partition(|e| e.message == HELLO);
    assert!(
        other.is_empty(),
        "decoder lines at a clean stop: {other:#?}"
    );
    // At least the first video and audio decoder of each run spoke.
    assert!(hello.len() >= 8, "{hello:#?}");
    // Decoders read their input through ffmpeg's `file:` protocol.
    let input = format!("file:{}", clip.canonicalize().unwrap().display());
    assert!(
        hello
            .iter()
            .all(|e| e.level == Level::WARN && e.input == input),
        "{hello:#?}"
    );
}
