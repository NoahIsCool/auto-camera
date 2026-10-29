//! The two reader threads: stdout carries `-progress` reports and mux
//! statistics, stderr carries ffmpeg's log.
//!
//! The progress reader owns the progress parser and the bitrate window and
//! publishes a report (with the muxed bitrate beside it) at every block
//! terminator, twice a second. The stderr reader owns the stream monitor:
//! it parses every line ([`LogLine`]), records the errors (redacted, and
//! marked as the stream's when an object of the stream logged them), feeds
//! the monitor with the media time of the newest progress report and
//! publishes a copy after each event. A reconnection clears the stream's
//! error. Neither holds a lock across a read.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::{Shared, lock};
use crate::config::redact;
use crate::logline::LogLine;
use crate::progress::{
    BitrateWindow, ProgressParser, STREAM_BITRATE_WINDOW, average_kbps, parse_mux_line,
};
use crate::stream::{StreamEvent, StreamMonitor, is_stream_line};

/// Calls `f` with each line of `input` (lossily decoded) until end of file.
fn for_each_line(input: impl Read, mut f: impl FnMut(&str)) {
    let mut reader = BufReader::new(input);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => f(&String::from_utf8_lossy(&line)),
        }
    }
}

/// Parses ffmpeg's `-progress` stream from stdout into [`Shared::progress`],
/// and its mux statistics into [`Shared::muxed_kbps`].
///
/// When ffmpeg reports no bitrate (the tee case) and a recording exists,
/// the report's bitrate is the recording's average so far.
pub fn progress_reader(stdout: impl Read, shared: Arc<Shared>, record_path: Option<PathBuf>) {
    let mut parser = ProgressParser::new();
    let mut window = BitrateWindow::new(STREAM_BITRATE_WINDOW);
    for_each_line(stdout, |line| {
        if let Some((time, bytes)) = parse_mux_line(line) {
            window.push(time, bytes);
        } else if let Some(mut report) = parser.feed(line) {
            if report.bitrate_kbps.is_none()
                && let (Some(path), Some(t)) = (&record_path, report.out_time)
                && let Ok(meta) = std::fs::metadata(path)
            {
                report.bitrate_kbps = average_kbps(meta.len(), t);
            }
            *lock(&shared.muxed_kbps) = window.kbps();
            *lock(&shared.progress) = report;
        }
    });
    shared.stdout_closed.store(true, Ordering::SeqCst);
}

/// An error line as the operator reads it: `context: message`, without
/// the object's address and the level tag, stream key redacted.
fn error_text(line: &LogLine, rtmp_url: Option<&str>) -> String {
    let text = match line.context {
        Some(context) => format!("{context}: {}", line.message),
        None => line.message.to_owned(),
    };
    redact(&text, rtmp_url)
}

/// Keeps the newest error line of stderr, and runs `monitor` (the encode
/// streams) over every line; see the module docs.
pub fn stderr_reader(
    stderr: impl Read,
    shared: Arc<Shared>,
    rtmp_url: Option<String>,
    mut monitor: Option<StreamMonitor>,
) {
    for_each_line(stderr, |line| {
        for piece in line.split('\r').map(str::trim).filter(|p| !p.is_empty()) {
            let parsed = LogLine::parse(piece);
            if parsed.is_error() {
                let text = error_text(&parsed, rtmp_url.as_deref());
                if is_stream_line(&parsed) {
                    shared.record_stream_error(&text);
                } else {
                    shared.record_error(&text);
                }
            }
            let Some(monitor) = &mut monitor else {
                continue;
            };
            let now = lock(&shared.progress).out_time.unwrap_or_default();
            if let Some(event) = monitor.feed(&parsed, now) {
                if event == StreamEvent::Reconnected {
                    shared.clear_stream_error();
                }
                shared.publish_stream(*monitor);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::StreamState;
    use std::time::Duration;

    #[test]
    fn progress_reports_and_mux_statistics_are_published() {
        let shared = Arc::new(Shared::default());
        let stdout = b"frame=5\nfps=30\nout_time_us=166666\nprogress=continue\n\
                       mux 0 0 1000\nmux 0 0.25 1000\nmux 1 0.5 500\n\
                       frame=15\nprogress=end\n";
        progress_reader(&stdout[..], shared.clone(), None);
        let p = shared.progress();
        assert_eq!(p.frame, 15);
        assert!(p.ended);
        assert_eq!(p.bitrate_kbps, None);
        // 2500 bytes over half a second of media.
        assert_eq!(shared.muxed_kbps(), Some(40.0));
        assert!(shared.stdout_closed.load(Ordering::SeqCst));

        // Without mux statistics there is no muxed bitrate.
        let shared = Arc::new(Shared::default());
        progress_reader(&b"frame=1\nprogress=end\n"[..], shared.clone(), None);
        assert_eq!(shared.muxed_kbps(), None);
    }

    #[test]
    fn errors_are_kept_redacted_and_without_tags() {
        let shared = Arc::new(Shared::default());
        let url = "rtmp://host/live/super-secret";
        let stderr = b"first\r\n\
            [fifo @ 0x55629163f840] [error] Error opening rtmp://host/live/super-secret: refused\n\
            [fifo @ 0x55629163f840] [info] Recovery failed: refused\n\
            [flv @ 0x556291640380] [warning] Failed to update header with correct duration.\n\n";
        stderr_reader(&stderr[..], shared.clone(), Some(url.into()), None);
        // Info and warning lines are not errors.
        assert_eq!(
            shared.last_error().as_deref(),
            Some("fifo: Error opening rtmp://host/live/<redacted>: refused")
        );
        assert_eq!(shared.stream(), None, "no monitor without a stream");

        let shared = Arc::new(Shared::default());
        stderr_reader(
            &b"fake ffmpeg failed on purpose\n"[..],
            shared.clone(),
            None,
            None,
        );
        assert_eq!(
            shared.last_error().as_deref(),
            Some("fake ffmpeg failed on purpose")
        );
    }

    #[test]
    fn the_monitor_follows_the_log_and_a_reconnection_clears_the_error() {
        let shared = Arc::new(Shared::default());
        shared.record_error("aac: an older error");
        let monitor = StreamMonitor::new(Duration::from_secs(3), 1);
        let stderr = b"[tcp @ 0x1] [error] Connection to tcp://h:1935 failed: refused\n\
            [fifo @ 0x2] [error] Error opening rtmp://h/app/key-1234: refused\n\
            [fifo @ 0x2] [info] Recovery failed: refused\n";
        stderr_reader(&stderr[..], shared.clone(), None, Some(monitor));
        let health = shared.stream().unwrap().health(None, true, None);
        assert_eq!(health.state, StreamState::Reconnecting { attempt: 2 });
        assert!(
            shared
                .last_error()
                .unwrap()
                .starts_with("fifo: Error opening")
        );

        let monitor = shared.stream().unwrap();
        let stderr = b"[fifo @ 0x2] [info] Recovery successful\n";
        stderr_reader(&stderr[..], shared.clone(), None, Some(monitor));
        let health = shared.stream().unwrap().health(None, true, None);
        assert_eq!(health.state, StreamState::Connected);
        assert_eq!((health.outages, health.reconnects), (1, 1));
        assert_eq!(shared.last_error().as_deref(), Some("aac: an older error"));
    }
}
