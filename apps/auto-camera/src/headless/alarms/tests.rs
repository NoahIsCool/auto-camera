//! The alarm log of a headless run: on hand-made snapshots, every alarm
//! raised gets an entry with its clear time, one still raised at the end
//! has none and carries its newest message, and a cause raised twice gets
//! two entries; and a real recording run without program audio logs that
//! from the output's start until it stops.

use std::path::Path;
use std::time::Duration;

use ac_core::SourceId;
use ac_director::DirectorMode;
use ac_engine::{EncoderSetting, EngineCore, OutputState, OutputView, StudioConfig, TimingView};
use ac_media::SourceInfo;
use ac_output::{EncoderChoice, EncoderStats, Progress, StreamHealth, StreamState};
use ac_sources::SourceConfig;

use super::*;
use crate::headless::{HeadlessOptions, run};
use crate::signals::QuitSignal;
use crate::testing::{have, temp_path};

/// A one-source synthetic studio at 320x180, encoded with libx264 (the
/// same everywhere), with no detector, no director and no program audio.
fn studio() -> StudioConfig {
    let mut c = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 1,
        },
    );
    c.output.width = 320;
    c.output.height = 180;
    c.output.encoder = EncoderSetting::Libx264;
    c.director.mode = DirectorMode::Off;
    c
}

/// The snapshot of [`studio`] before its first tick, streaming (no
/// recording) with a connected stream.
fn streaming() -> EngineSnapshot {
    let info = SourceInfo {
        id: SourceId("stage".into()),
        width: 320,
        height: 180,
        fps: 30.0,
        audio: None,
    };
    let core = EngineCore::new(&studio(), vec![info], &[false]).unwrap();
    let mut snap = core.snapshot(OutputView::default(), TimingView::default(), true);
    snap.output.state = OutputState::Running;
    snap.output.stats = Some(EncoderStats {
        encoder: EncoderChoice::Libx264,
        frames_in: 0,
        frames_dropped: 0,
        frames_repeated: 0,
        frames_written: 0,
        audio_samples_in: 0,
        audio_samples_dropped: 0,
        audio_silence_samples: 0,
        audio_samples_written: 0,
        audio_connected: false,
        ffmpeg_alive: true,
        exit_code: None,
        last_error: None,
        progress: Progress::default(),
        stream: Some(stream(StreamState::Connected)),
    });
    snap
}

/// A stream's health in `state`, after one outage.
fn stream(state: StreamState) -> StreamHealth {
    StreamHealth {
        state,
        stalled: false,
        outages: 1,
        reconnects: 0,
        bitrate_kbps: None,
    }
}

/// `snap` at master-clock time `secs`, a tick per millisecond.
fn at(snap: &mut EngineSnapshot, secs: f64) -> &EngineSnapshot {
    snap.media_time = Duration::from_secs_f64(secs);
    snap.tick = (secs * 1000.0).round() as u64;
    snap
}

/// Sets the stream's state in `snap`.
fn set_stream(snap: &mut EngineSnapshot, state: StreamState) {
    snap.output.stats.as_mut().unwrap().stream = Some(stream(state));
}

#[test]
fn the_log_keeps_every_alarm_raised_with_when_it_cleared() {
    let mut log = AlarmLog::default();
    let mut snap = streaming();
    log.observe(at(&mut snap, 0.0));
    assert_eq!(log.summary(), []);
    // The output fails, and starts again: one entry, cleared.
    snap.output.state = OutputState::Failed("ffmpeg exited".into());
    log.observe(at(&mut snap, 1.0004));
    snap.output.state = OutputState::Starting;
    log.observe(at(&mut snap, 2.0));
    // The new session's stream reconnects and is still at it at the end.
    snap.output.state = OutputState::Running;
    set_stream(&mut snap, StreamState::Reconnecting { attempt: 1 });
    log.observe(at(&mut snap, 3.0));
    set_stream(&mut snap, StreamState::Reconnecting { attempt: 3 });
    log.observe(at(&mut snap, 9.0));
    // And the output fails again: a second entry of the same kind.
    snap.output.state = OutputState::Failed("killed".into());
    log.observe(at(&mut snap, 10.0));
    let summary = log.summary();
    let entry = |kind: &str, message: &str, raised_s: f64, cleared_s: Option<f64>| AlarmSummary {
        kind: kind.into(),
        message: message.into(),
        raised_s,
        cleared_s,
    };
    assert_eq!(
        summary,
        [
            entry(
                "output_failed",
                "output failed: ffmpeg exited",
                1.0,
                Some(2.0)
            ),
            entry(
                "stream_reconnecting",
                "stream reconnecting (attempt 3) · 1 outage",
                3.0,
                Some(10.0)
            ),
            entry("output_failed", "output failed: killed", 10.0, None),
        ]
    );
    let json = serde_json::to_value(&summary[2]).unwrap();
    assert_eq!(json["kind"], "output_failed");
    assert!(json["cleared_s"].is_null());
}

#[test]
fn an_alarm_still_raised_at_the_end_has_its_newest_message() {
    let mut log = AlarmLog::default();
    let mut snap = streaming();
    log.observe(at(&mut snap, 0.0));
    set_stream(&mut snap, StreamState::Reconnecting { attempt: 1 });
    log.observe(at(&mut snap, 0.5));
    set_stream(&mut snap, StreamState::Reconnecting { attempt: 4 });
    log.observe(at(&mut snap, 8.0));
    let summary = log.summary();
    assert_eq!(summary.len(), 1);
    assert_eq!(
        summary[0].message,
        "stream reconnecting (attempt 4) · 1 outage"
    );
    assert_eq!((summary[0].raised_s, summary[0].cleared_s), (0.5, None));
    // Snapshots of an engine that has stopped are not sampled by a run,
    // but an older snapshot handed in changes nothing.
    log.observe(at(&mut snap, 1.0));
    assert_eq!(log.summary(), summary);
}

#[test]
fn a_recording_run_logs_its_missing_audio_until_the_output_stops() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let path = temp_path("alarms.mp4");
    let options = HeadlessOptions {
        seconds: 0.6,
        record: Some(path.clone()),
        trace: None,
    };
    let s = run(studio(), Path::new("x.toml"), &options, &QuitSignal::new());
    assert!(s.ok, "{:?}", s.error);
    let silent: Vec<&AlarmSummary> = s
        .alarms
        .iter()
        .filter(|a| a.kind == "recording_without_audio")
        .collect();
    assert_eq!(silent.len(), 1, "{:?}", s.alarms);
    assert!(
        silent[0]
            .message
            .starts_with("recording has no audio track"),
        "{}",
        silent[0].message
    );
    let cleared = silent[0]
        .cleared_s
        .expect("cleared when the output stopped");
    // Raised for most of the 0.6 s the output ran, with room for a loaded
    // machine to sample late.
    assert!(cleared >= silent[0].raised_s + 0.4, "{:?}", silent[0]);
    assert!(
        cleared <= s.media_seconds,
        "{cleared} after {}",
        s.media_seconds
    );
    // The wide was on program before the output started: never black.
    assert!(
        s.alarms.iter().all(|a| a.kind != "program_black"),
        "{:?}",
        s.alarms
    );
    let json = serde_json::to_value(&s).unwrap();
    assert!(json["alarms"].is_array());
    std::fs::remove_file(path).unwrap();
}
