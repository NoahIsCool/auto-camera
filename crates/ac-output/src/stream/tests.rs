//! The stream monitor against canned stderr and against hand-made lines.
//!
//! The fixtures in `tests/fixtures/stderr` are ffmpeg 6.1's stderr for the
//! encoder's tee and fifo options (`-loglevel repeat+level+info`), recorded
//! with a loopback receiver (`ffmpeg -listen 1`) and kept verbatim but for
//! the recording path and trailing blanks:
//!
//! * `connected`: the receiver took the whole stream;
//! * `receiver-killed`: the receiver was killed after 4 s and started again
//!   5 s later;
//! * `refused-at-start`: nothing listened for the first 3 s;
//! * `ingest-hangs`: the port accepted connections but never answered;
//! * `ingest-stalls`: the receiver was stopped (`SIGSTOP`) for 5 s and
//!   continued, at a bitrate that outgrows the socket buffers;
//! * `slave-abandoned`: slave options the fifo did not get, so the tee
//!   dropped the RTMP slave (#1) at its first failure.

use super::*;

/// `connected.log`.
const CONNECTED: &str = include_str!("../../tests/fixtures/stderr/connected.log");
/// `receiver-killed.log`.
const RECEIVER_KILLED: &str = include_str!("../../tests/fixtures/stderr/receiver-killed.log");
/// `refused-at-start.log`.
const REFUSED_AT_START: &str = include_str!("../../tests/fixtures/stderr/refused-at-start.log");
/// `ingest-hangs.log`.
const INGEST_HANGS: &str = include_str!("../../tests/fixtures/stderr/ingest-hangs.log");
/// `ingest-stalls.log`.
const INGEST_STALLS: &str = include_str!("../../tests/fixtures/stderr/ingest-stalls.log");
/// `slave-abandoned.log`.
const SLAVE_ABANDONED: &str = include_str!("../../tests/fixtures/stderr/slave-abandoned.log");

/// The settle time of a 30 fps stream without audio: 2 s of queue + 1 s.
const SETTLE: Duration = Duration::from_secs(3);

/// Seconds as a duration.
fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

/// A monitor for a recorded and streamed 30 fps program (RTMP is slave 1).
fn monitor() -> StreamMonitor {
    StreamMonitor::new(SETTLE, 1)
}

/// The state while ffmpeg runs, at `at`.
fn state(m: &StreamMonitor, at: Duration) -> StreamState {
    m.health(Some(at), true, None).state
}

/// Feeds every line of `log` at media time `at`; returns each event with
/// the state right after it.
fn replay(m: &mut StreamMonitor, log: &str, at: Duration) -> Vec<(StreamEvent, StreamState)> {
    log.lines()
        .filter_map(|line| {
            let event = m.feed(&LogLine::parse(line), at)?;
            Some((event, state(m, at)))
        })
        .collect()
}

/// `Reconnecting { attempt }`.
fn reconnecting(attempt: u32) -> StreamState {
    StreamState::Reconnecting { attempt }
}

#[test]
fn fixture_connected_says_nothing_so_the_stream_is_proven() {
    let mut m = monitor();
    assert_eq!(replay(&mut m, CONNECTED, secs(3.9)), []);
    assert_eq!(state(&m, Duration::ZERO), StreamState::Connecting);
    assert_eq!(state(&m, secs(2.9)), StreamState::Connecting);
    let h = m.health(Some(SETTLE), true, Some(812.0));
    assert_eq!(h.state, StreamState::Connected);
    assert!(!h.stalled);
    assert_eq!((h.outages, h.reconnects), (0, 0));
    assert_eq!(h.bitrate_kbps, Some(812.0));
    // The end-of-stream warnings and info lines are not errors of note.
    let errors: Vec<_> = CONNECTED
        .lines()
        .map(LogLine::parse)
        .filter(LogLine::is_error)
        .collect();
    assert_eq!(errors, []);
}

#[test]
fn fixture_receiver_killed_reconnects_attempt_by_attempt() {
    let mut m = monitor();
    // Connected for four seconds before the kill.
    let events = replay(&mut m, RECEIVER_KILLED, secs(4.0));
    use StreamEvent::*;
    assert_eq!(
        events,
        [
            (OpenFailed, reconnecting(1)),
            (AttemptFailed, reconnecting(2)),
            (OpenFailed, reconnecting(2)),
            (AttemptFailed, reconnecting(3)),
            (OpenFailed, reconnecting(3)),
            (AttemptFailed, reconnecting(4)),
            (Reconnected, StreamState::Connected),
        ]
    );
    let h = m.health(Some(secs(11.9)), true, Some(790.0));
    assert_eq!((h.outages, h.reconnects), (1, 1));
    assert_eq!(h.bitrate_kbps, Some(790.0));
    // The kill's error lines are the stream's, and only they.
    let stream_errors: Vec<_> = RECEIVER_KILLED
        .lines()
        .map(LogLine::parse)
        .filter(|l| l.is_error())
        .map(|l| (l.context, is_stream_line(&l)))
        .collect();
    assert_eq!(stream_errors.len(), 9);
    assert!(stream_errors.iter().all(|(_, stream)| *stream));
}

#[test]
fn fixture_refused_at_start_is_an_outage_from_the_first_line() {
    let mut m = monitor();
    let events = replay(&mut m, REFUSED_AT_START, secs(0.05));
    let states: Vec<_> = events.iter().map(|(_, s)| *s).collect();
    assert_eq!(
        states,
        [
            reconnecting(1),
            reconnecting(1),
            reconnecting(2),
            reconnecting(2),
            reconnecting(3),
            StreamState::Connected,
        ]
    );
    let h = m.health(Some(secs(5.9)), true, None);
    assert_eq!((h.outages, h.reconnects), (1, 1));
    assert!(!h.stalled);
    assert_eq!(h.bitrate_kbps, None, "nothing measured");
}

#[test]
fn fixture_ingest_hangs_stays_connecting_and_stalled() {
    let mut m = monitor();
    let events = replay(&mut m, INGEST_HANGS, secs(0.8));
    assert_eq!(events, [(StreamEvent::QueueFull, StreamState::Connecting)]);
    for at in [1.0, 10.0, 600.0] {
        let h = m.health(Some(secs(at)), true, Some(800.0));
        assert_eq!(h.state, StreamState::Connecting, "{at}");
        assert!(h.stalled, "{at}");
        assert_eq!(h.bitrate_kbps, Some(0.0), "{at}");
    }
    // ffmpeg was killed at the finish timeout.
    assert_eq!(
        m.health(Some(secs(3.0)), false, None).state,
        StreamState::Down
    );
}

#[test]
fn fixture_ingest_stalls_while_connected_until_proven_again() {
    let mut m = monitor();
    let full = INGEST_STALLS
        .lines()
        .position(|l| l.ends_with("FIFO queue full"))
        .unwrap();
    let (before, after) = INGEST_STALLS.split_at(
        INGEST_STALLS
            .match_indices('\n')
            .nth(full)
            .map_or(0, |(i, _)| i + 1),
    );
    // The receiver stopped at 4 s; the queue overflowed a little later.
    let events = replay(&mut m, before, secs(4.5));
    assert_eq!(events, [(StreamEvent::QueueFull, StreamState::Connected)]);
    let h = m.health(Some(secs(6.0)), true, Some(19_000.0));
    assert!(h.stalled);
    assert_eq!(h.bitrate_kbps, Some(0.0));
    // It continued at 9 s; the flush starts a new proof.
    let events = replay(&mut m, after, secs(9.0));
    assert_eq!(
        events,
        [(StreamEvent::QueueFlushed, StreamState::Connected)]
    );
    assert!(m.health(Some(secs(11.9)), true, None).stalled);
    let h = m.health(Some(secs(12.0)), true, Some(19_000.0));
    assert!(!h.stalled);
    assert_eq!(h.bitrate_kbps, Some(19_000.0));
    assert_eq!((h.outages, h.reconnects), (0, 0), "a stall is no outage");
}

#[test]
fn fixture_slave_abandoned_is_down_for_good() {
    let mut m = monitor();
    let events = replay(&mut m, SLAVE_ABANDONED, secs(0.1));
    assert_eq!(
        events,
        [
            (StreamEvent::OpenFailed, reconnecting(1)),
            (StreamEvent::Abandoned, StreamState::Down),
        ]
    );
    // Nothing brings it back.
    let line = LogLine::parse("[fifo @ 0x1] [info] Recovery successful");
    m.feed(&line, secs(1.0));
    assert_eq!(state(&m, secs(10.0)), StreamState::Down);
    assert_eq!(m.health(None, true, Some(500.0)).bitrate_kbps, Some(0.0));

    // Without a recording the stream is slave 0, so slave 1 is not it.
    let mut other = StreamMonitor::new(SETTLE, 0);
    let events = replay(&mut other, SLAVE_ABANDONED, secs(0.1));
    assert_eq!(events, [(StreamEvent::OpenFailed, reconnecting(1))]);
}

#[test]
fn only_the_fifo_and_tee_speak_for_the_connection() {
    let m = monitor();
    for line in [
        "[libx264 @ 0x1] [info] Recovery successful",
        "[info] Recovery successful",
        "Recovery successful",
        "[fifo @ 0x1] [info] Recovery attempt #1",
        "[fifo @ 0x1] [info] Dropping non-keyframe packet",
        "[tee @ 0x1] [error] Slave muxer #0 failed: No space left on device, continuing with 1/2 slaves.",
        "[tee @ 0x1] [error] Slave muxer #x failed",
        "[tee @ 0x1] [error] Slave muxer #1",
        "[out#0/tee @ 0x1] [info] video:146kB audio:0kB",
        "",
        "garbage \u{fffd}\u{fffd}",
    ] {
        assert_eq!(m.event(&LogLine::parse(line)), None, "{line:?}");
    }
    let given_up = "[fifo @ 0x1] [error] Maximal number of 3 recovery attempts reached.";
    assert_eq!(
        m.event(&LogLine::parse(given_up)),
        Some(StreamEvent::Abandoned)
    );
    let all = "[tee @ 0x1] [error] All tee outputs failed.";
    assert_eq!(m.event(&LogLine::parse(all)), Some(StreamEvent::Abandoned));
}

#[test]
fn stream_lines_are_those_of_the_streams_objects() {
    for (line, stream) in [
        (
            "[fifo @ 0x1] [error] Error opening rtmp://h/a/k: refused",
            true,
        ),
        (
            "[tcp @ 0x1] [error] Connection to tcp://h:1935 failed",
            true,
        ),
        ("[rtmp @ 0x1] [error] Server error: Bad key", true),
        ("[tls @ 0x1] [error] handshake failed", true),
        ("[flv @ 0x1] [warning] Failed to update header", true),
        ("[tee @ 0x1] [error] Slave muxer #0 failed", false),
        ("[libx264 @ 0x1] [error] broken", false),
        ("[error] Error while opening encoder", false),
        ("fake ffmpeg failed on purpose", false),
    ] {
        assert_eq!(is_stream_line(&LogLine::parse(line)), stream, "{line}");
    }
}

#[test]
fn an_outage_between_two_lines_counts_once_reconnected() {
    let mut m = monitor();
    let ok = LogLine::parse("[fifo @ 0x1] [info] Recovery successful");
    assert_eq!(m.feed(&ok, secs(5.0)), Some(StreamEvent::Reconnected));
    let h = m.health(Some(secs(5.0)), true, None);
    assert_eq!(h.state, StreamState::Connected);
    assert_eq!((h.outages, h.reconnects), (1, 1));

    // A failed attempt without a failed open still enters Reconnecting,
    // with that attempt counted.
    let failed = LogLine::parse("[fifo @ 0x1] [info] Recovery failed: Broken pipe");
    m.feed(&failed, secs(6.0));
    assert_eq!(state(&m, secs(6.0)), reconnecting(2));
    assert_eq!(m.health(None, true, None).outages, 2);
}

#[test]
fn overflow_and_flush_while_reconnecting_change_nothing() {
    let mut m = monitor();
    let open_failed = LogLine::parse("[fifo @ 0x1] [error] Error opening rtmp://h/a/k: timeout");
    m.feed(&open_failed, secs(4.0));
    for line in [
        "[fifo @ 0x1] [warning] FIFO queue full",
        "[fifo @ 0x1] [info] FIFO queue flushed",
    ] {
        m.feed(&LogLine::parse(line), secs(5.0));
    }
    let h = m.health(Some(secs(20.0)), true, Some(800.0));
    assert_eq!(h.state, reconnecting(1));
    assert!(!h.stalled);
    assert_eq!(h.bitrate_kbps, Some(0.0));
    // Reconnected, it is not stalled and needs no proof.
    m.feed(
        &LogLine::parse("[fifo @ 0x1] [info] Recovery successful"),
        secs(21.0),
    );
    let h = m.health(Some(secs(21.0)), true, Some(800.0));
    assert_eq!(h.state, StreamState::Connected);
    assert!(!h.stalled);
    assert_eq!(h.bitrate_kbps, Some(800.0));
}

#[test]
fn a_proof_once_made_survives_a_later_overflow() {
    let mut m = monitor();
    // Proven at 3 s; the overflow at 10 s stalls it but does not undo it.
    m.feed(
        &LogLine::parse("[fifo @ 0x1] [warning] FIFO queue full"),
        secs(10.0),
    );
    let h = m.health(Some(secs(10.0)), true, None);
    assert_eq!(h.state, StreamState::Connected);
    assert!(h.stalled);
    // An overflow before the proof keeps it connecting instead.
    let mut early = monitor();
    early.feed(
        &LogLine::parse("[fifo @ 0x1] [warning] FIFO queue full"),
        secs(1.0),
    );
    early.feed(
        &LogLine::parse("[fifo @ 0x1] [info] FIFO queue flushed"),
        secs(2.0),
    );
    assert_eq!(state(&early, secs(4.9)), StreamState::Connecting);
    assert!(!early.health(Some(secs(4.9)), true, None).stalled);
    assert_eq!(state(&early, secs(5.0)), StreamState::Connected);
}

#[test]
fn ffmpeg_gone_means_down_whatever_the_log_said() {
    let mut m = monitor();
    assert_eq!(m.health(None, false, Some(1.0)).state, StreamState::Down);
    m.feed(
        &LogLine::parse("[fifo @ 0x1] [info] Recovery successful"),
        secs(1.0),
    );
    let h = m.health(Some(secs(9.0)), false, Some(800.0));
    assert_eq!(h.state, StreamState::Down);
    assert_eq!(h.bitrate_kbps, Some(0.0));
    assert_eq!(h.reconnects, 1, "the counters stay");
}

#[test]
fn settle_time_is_the_queue_at_the_packet_rate_plus_the_margin() {
    assert_eq!(settle_time(30, false), SETTLE);
    assert_eq!(settle_time(60, false), secs(2.0));
    let with_audio = settle_time(30, true).as_secs_f64();
    assert!(
        (with_audio - (60.0 / (30.0 + 46.875) + 1.0)).abs() < 1e-9,
        "{with_audio}"
    );
    // A zero rate cannot divide by zero.
    assert_eq!(settle_time(0, false), secs(61.0));
}

#[test]
fn states_read_as_words() {
    assert_eq!(StreamState::Connecting.to_string(), "connecting");
    assert_eq!(StreamState::Connected.to_string(), "connected");
    assert_eq!(reconnecting(3).to_string(), "reconnecting (attempt 3)");
    assert_eq!(StreamState::Down.to_string(), "down");
}
