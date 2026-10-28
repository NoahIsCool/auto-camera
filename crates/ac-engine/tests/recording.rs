//! A recording session through the engine runtime: start, run, stop and
//! check the file. Skipped when ffmpeg is not installed.

mod common;

use std::net::TcpListener;

use ac_core::ShotId;
use ac_engine::{CommandError, CommandReply, EngineCommand, OutputState};
use ac_output::StreamState;

use common::{WAIT, have, start, studio, temp_path};

#[test]
fn a_recording_session_writes_a_playable_file() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let path = temp_path("program.mp4");
    let engine = start(studio(1));
    let h = engine.handle();
    let wide = ShotId("stage/wide".into());
    h.wait_until(WAIT, |s| s.shot(&wide).is_some_and(|v| v.available))
        .expect("the source delivers frames");
    h.send(EngineCommand::SetPreview {
        shot: ShotId("stage/wide".into()),
    })
    .unwrap();
    h.send(EngineCommand::Cut).unwrap();
    assert_eq!(
        h.send(EngineCommand::StartOutput {
            rtmp_url: None,
            record_path: Some(path.clone()),
        }),
        Ok(CommandReply::OutputStarting)
    );
    assert_eq!(
        h.send(EngineCommand::StartOutput {
            rtmp_url: None,
            record_path: Some(temp_path("second.mp4")),
        }),
        Err(CommandError::OutputBusy("starting"))
    );
    let running = h
        .wait_until(WAIT, |s| s.output.state == OutputState::Running)
        .expect("the encoder starts");
    let frames = running.output.frames_pushed;
    h.wait_until(WAIT, |s| s.output.frames_pushed >= frames + 60)
        .expect("frames reach the encoder");
    assert_eq!(
        h.send(EngineCommand::StopOutput),
        Ok(CommandReply::OutputStopping)
    );
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
    let stats = done.output.stats.as_ref().expect("final stats");
    assert!(stats.frames_written >= 60, "{stats:?}");
    assert!(!stats.ffmpeg_alive);
    assert_eq!(stats.exit_code, Some(0));
    engine.shutdown().unwrap();

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    assert!(size > 0, "{} is empty", path.display());
    if have("ffprobe") {
        let out = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .expect("ffprobe runs");
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text.trim(), "h264,320,180", "ffprobe: {text}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_stream_outage_shows_in_the_view_and_leaves_the_session_running() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let path = temp_path("streamed.mp4");
    // A loopback port that was free a moment ago refuses the connection, so
    // the stream reconnects while the recording goes on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let engine = start(studio(1));
    let h = engine.handle();
    h.send(EngineCommand::StartOutput {
        rtmp_url: Some(format!("rtmp://127.0.0.1:{port}/live/engine-test-key")),
        record_path: Some(path.clone()),
    })
    .unwrap();
    let stream_state = |s: &ac_engine::EngineSnapshot| {
        s.output
            .stats
            .as_ref()
            .and_then(|st| st.stream)
            .map(|st| st.state)
    };
    let out = h
        .wait_until(WAIT, |s| {
            matches!(stream_state(s), Some(StreamState::Reconnecting { .. }))
        })
        .expect("the refused stream reconnects");
    assert_eq!(out.output.state, OutputState::Running, "{:?}", out.output);
    let stats = out.output.stats.as_ref().unwrap();
    assert!(stats.stream.unwrap().outages >= 1, "{stats:?}");
    let err = stats.last_error.as_deref().expect("the outage is reported");
    assert!(!err.contains("engine-test-key"), "key leaked: {err}");
    let url = out.output.rtmp_url.as_deref().unwrap();
    assert!(!url.contains("engine-test-key"), "key shown: {url}");

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
    assert_eq!(stream_state(&done), Some(StreamState::Down));
    engine.shutdown().unwrap();
    assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 0));
    let _ = std::fs::remove_file(&path);
}
