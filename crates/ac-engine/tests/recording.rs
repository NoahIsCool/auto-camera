//! A recording session through the engine runtime: start, run, stop and
//! check the file. Skipped when ffmpeg is not installed.

mod common;

use ac_core::ShotId;
use ac_engine::{CommandError, CommandReply, EngineCommand, OutputState};

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
    h.send(EngineCommand::Cut {
        preview: Some(ShotId("stage/wide".into())),
    })
    .unwrap();
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
