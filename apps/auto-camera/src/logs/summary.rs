//! A few lines of text that say what the studio was doing, for crash
//! reports.
//!
//! # Theory
//!
//! A crash report answers "what was going on when it broke": which shot
//! was on air, what each source and its detector were doing, whether the
//! output ran and streamed, and how the detector sidecar fared. The
//! engine's snapshot has all of it; this module turns one into plain text:
//! an engine line, a switcher line, one line per source, an output line
//! and, with a sidecar, a sidecar line. The snapshot watcher renders the
//! newest snapshot this way and stores the text with the crash reporter
//! (see the `crash` module), so the panic hook only copies a string.
//!
//! # Invariants
//!
//! * Pure: the text is a function of the snapshot alone.
//! * Bounded: every line is cut at [`MAX_LINE`] bytes and at most
//!   [`MAX_SOURCES`] sources get a line, so the summary stays under the
//!   crash reporter's cap (`crash::MAX_SUMMARY`) however large the studio
//!   or its error messages are.

use std::fmt::Write as _;

use ac_engine::{EngineSnapshot, OutputState, OutputView, SourceView};
use ac_sidecar::{HostState, HostStatus};

use super::writer::one_line;

/// Longest line of a summary, in bytes.
pub const MAX_LINE: usize = 300;

/// Sources that get a line of their own.
pub const MAX_SOURCES: usize = 16;

/// The summary of `snap`; see the module docs.
pub fn render(snap: &EngineSnapshot) -> String {
    let mut lines = vec![engine_line(snap)];
    if let Some(fatal) = &snap.fatal {
        lines.push(format!("fatal: {fatal}"));
    }
    lines.push(switcher_line(snap));
    lines.extend(snap.sources.iter().take(MAX_SOURCES).map(source_line));
    if snap.sources.len() > MAX_SOURCES {
        lines.push(format!(
            "and {} more sources",
            snap.sources.len() - MAX_SOURCES
        ));
    }
    lines.push(output_line(&snap.output));
    if let Some(sidecar) = &snap.sidecar {
        lines.push(sidecar_line(sidecar));
    }
    if !snap.render_issues.is_empty() {
        lines.push(format!("render issues: {}", snap.render_issues.join("; ")));
    }
    lines
        .iter()
        .map(|l| one_line(l, MAX_LINE))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `engine: tick 42, media 1.400 s, running`.
fn engine_line(snap: &EngineSnapshot) -> String {
    format!(
        "engine: tick {}, media {:.3} s, {}",
        snap.tick,
        snap.media_time.as_secs_f64(),
        if snap.running { "running" } else { "stopped" }
    )
}

/// What is on program and preview, and what the director does.
fn switcher_line(snap: &EngineSnapshot) -> String {
    let shot = |id: &Option<ac_core::ShotId>| match id {
        Some(id) => match snap.shot(id) {
            Some(s) => format!("{} ({})", s.name, id.0),
            None => id.0.clone(),
        },
        None => "none".to_owned(),
    };
    let mut line = format!(
        "program: {}, preview: {}, program changes: {}",
        shot(&snap.switcher.program),
        shot(&snap.switcher.preview),
        snap.program_changes
    );
    if snap.switcher.in_transition {
        line.push_str(", in a transition");
    }
    let _ = write!(
        line,
        "; director: {}",
        format!("{:?}", snap.director.mode).to_lowercase()
    );
    if !snap.director.reason_text.is_empty() {
        let _ = write!(line, " ({})", snap.director.reason_text);
    }
    line
}

/// One source's health and its detector's.
fn source_line(s: &SourceView) -> String {
    let mut line = format!(
        "source {} ({}): {}, {:.1} of {} fps, {}x{}, {} subjects, {} frames missed",
        s.id.0,
        s.name,
        format!("{:?}", s.state).to_lowercase(),
        s.fps,
        s.nominal_fps,
        s.width,
        s.height,
        s.subjects,
        s.missed
    );
    if !s.discovery_enabled {
        line.push_str("; no detector");
        return line;
    }
    let d = &s.discovery;
    let _ = write!(
        line,
        "; detector {}, {} applied, {} stale, {} errors, {} timeouts",
        if d.preparing { "preparing" } else { "ready" },
        d.applied,
        d.stale,
        d.errors,
        d.timeouts
    );
    if let Some(error) = &d.last_error {
        let _ = write!(line, ", last error: {error}");
    }
    line
}

/// The output's state, counters and stream.
fn output_line(o: &OutputView) -> String {
    let mut line = match &o.state {
        OutputState::Failed(why) => format!("output: failed ({why})"),
        state => format!("output: {}", format!("{state:?}").to_lowercase()),
    };
    if let Some(path) = &o.record_path {
        let _ = write!(line, ", recording {}", path.display());
    }
    if let Some(url) = &o.rtmp_url {
        let _ = write!(line, ", streaming to {url}");
    }
    if let Some(stats) = &o.stats {
        let _ = write!(
            line,
            ", {} frames written, {} dropped",
            stats.frames_written, stats.frames_dropped
        );
        if let Some(stream) = &stats.stream {
            let _ = write!(line, ", stream {}", stream.state);
            if let Some(kbps) = stream.bitrate_kbps {
                let _ = write!(line, " at {kbps:.0} kbit/s");
            }
        }
        if let Some(error) = &stats.last_error {
            let _ = write!(line, ", last error: {error}");
        }
    }
    line
}

/// The sidecar host's state, process and counters.
fn sidecar_line(s: &HostStatus) -> String {
    let state = match &s.state {
        HostState::Idle => "idle".to_owned(),
        HostState::Starting => "starting".to_owned(),
        HostState::Ready => "ready".to_owned(),
        HostState::Unavailable { reason, .. } => format!("unavailable ({reason})"),
        HostState::Stopped => "stopped".to_owned(),
    };
    let mut line = format!("sidecar: {state}");
    if let Some(pid) = s.pid {
        let _ = write!(line, ", pid {pid}");
    }
    if let Some(hello) = &s.hello {
        let _ = write!(line, ", model {}", hello.model);
    }
    let st = &s.stats;
    let _ = write!(
        line,
        ", {} starts ({} restarts), {} failures, {} timeouts, {} of {} requests answered",
        st.spawns,
        st.restarts(),
        st.failures,
        st.timeouts,
        st.results,
        st.requests
    );
    if let Some(latency) = s.last_latency {
        let _ = write!(line, ", last pass {} ms", latency.as_millis());
    }
    line
}

#[cfg(test)]
mod tests {
    use ac_core::{ShotId, SourceId};
    use ac_output::{EncoderChoice, EncoderStats, Progress, StreamHealth, StreamState};

    use super::*;
    use crate::logs::crash::MAX_SUMMARY;
    use crate::logs::fixtures;

    #[test]
    fn a_fresh_studio_reads_as_such() {
        let text = render(&fixtures::snapshot());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "engine: tick 0, media 0.000 s, running");
        assert!(
            lines[1]
                .starts_with("program: none, preview: none, program changes: 0; director: suggest"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2]
                .starts_with("source stage (stage): starting, 0.0 of 30 fps, 320x180, 0 subjects")
                && lines[2].contains("; detector preparing, 0 applied"),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "output: idle");
        assert_eq!(lines.len(), 4, "no sidecar, no render issues: {text}");
    }

    #[test]
    fn a_busy_studio_shows_its_shot_output_sidecar_and_errors() {
        let mut snap = fixtures::snapshot();
        snap.running = false;
        snap.fatal = Some("the engine thread panicked: boom".into());
        let wide = snap.shots[0].id.clone();
        snap.switcher.program = Some(wide.clone());
        snap.switcher.preview = Some(ShotId("gone".into()));
        snap.sources[0].discovery.preparing = false;
        snap.sources[0].discovery.errors = 3;
        snap.sources[0].discovery.last_error = Some("refused\nthree times".into());
        snap.output.state = OutputState::Failed("ffmpeg exited".into());
        snap.output.rtmp_url = Some("rtmp://host/live/<redacted>".into());
        snap.output.stats = Some(EncoderStats {
            encoder: EncoderChoice::Libx264,
            frames_in: 10,
            frames_dropped: 1,
            frames_repeated: 1,
            frames_written: 10,
            audio_samples_in: 0,
            audio_samples_dropped: 0,
            audio_silence_samples: 0,
            audio_samples_written: 0,
            audio_connected: false,
            ffmpeg_alive: false,
            exit_code: Some(1),
            last_error: Some("rtmp: refused".into()),
            progress: Progress::default(),
            stream: Some(StreamHealth {
                state: StreamState::Reconnecting { attempt: 2 },
                stalled: false,
                outages: 1,
                reconnects: 0,
                bitrate_kbps: Some(0.0),
            }),
        });
        snap.sidecar = Some(fixtures::sidecar());
        snap.render_issues = vec!["shot x drew black".into()];
        let text = render(&snap);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].ends_with(", stopped"), "{}", lines[0]);
        assert_eq!(lines[1], "fatal: the engine thread panicked: boom");
        assert!(
            lines[2].starts_with(&format!(
                "program: {} ({}), preview: gone,",
                snap.shots[0].name, wide.0
            )),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].ends_with("; detector ready, 0 applied, 0 stale, 3 errors, 0 timeouts, last error: refused\\nthree times"),
            "{}",
            lines[3]
        );
        assert_eq!(
            lines[4],
            "output: failed (ffmpeg exited), streaming to rtmp://host/live/<redacted>, \
             10 frames written, 1 dropped, stream reconnecting (attempt 2) at 0 kbit/s, \
             last error: rtmp: refused"
        );
        assert_eq!(
            lines[5],
            "sidecar: ready, pid 4242, model sam3.1-mlx, 2 starts (1 restarts), 1 failures, \
             0 timeouts, 39 of 40 requests answered, last pass 120 ms"
        );
        assert_eq!(lines[6], "render issues: shot x drew black");
        assert_eq!(lines.len(), 7);
    }

    #[test]
    fn an_unavailable_sidecar_says_why() {
        let mut status = fixtures::sidecar();
        status.state = HostState::Unavailable {
            reason: "exit status 1: no model".into(),
            retry_at: None,
        };
        status.pid = None;
        status.hello = None;
        status.last_latency = None;
        assert_eq!(
            sidecar_line(&status),
            "sidecar: unavailable (exit status 1: no model), 2 starts (1 restarts), \
             1 failures, 0 timeouts, 39 of 40 requests answered"
        );
    }

    #[test]
    fn a_huge_studio_stays_within_the_crash_reports_cap() {
        let mut snap = fixtures::snapshot();
        let source = snap.sources[0].clone();
        snap.sources = (0..100)
            .map(|i| {
                let mut s = source.clone();
                s.id = SourceId(format!("cam{i}"));
                s.name = "a very long name ".repeat(50);
                s.discovery.last_error = Some("é".repeat(500));
                s
            })
            .collect();
        snap.fatal = Some("x".repeat(100_000));
        snap.render_issues = vec!["y".repeat(100_000)];
        let text = render(&snap);
        assert!(text.len() < MAX_SUMMARY, "{} bytes", text.len());
        assert!(text.lines().all(|l| l.len() <= MAX_LINE + " [...]".len()));
        assert!(text.contains("source cam15 "));
        assert!(!text.contains("source cam16 "));
        assert!(text.contains("\nand 84 more sources\n"), "{text}");
    }
}
