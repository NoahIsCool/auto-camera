//! Program loudness through the engine runtime (spec M12): a file source
//! whose embedded audio is a known tone is the program audio of a
//! recording, and the snapshot's meter reads the paced audio the encoder
//! is given. Skipped when ffmpeg is not installed.

mod common;

use std::path::PathBuf;
use std::process::Command;

use ac_core::ShotId;
use ac_engine::{DetectorSettings, EngineCommand, EngineSnapshot, OutputState};
use ac_sources::SourceConfig;

use common::{WAIT, have, start, studio, temp_path};

/// Crest of the clip's tone, in dBFS: on both channels, so it reads this
/// many LUFS too.
const TONE_DBFS: f64 = -20.0;

/// Writes a 320x180 30 fps clip of `seconds` with a stereo 48 kHz 1 kHz
/// tone at [`TONE_DBFS`]. A whole number of seconds is a whole number of
/// cycles, so the clip loops without a click.
fn tone_clip(seconds: u32) -> PathBuf {
    let path = temp_path("tone.mkv");
    let amplitude = 10f64.powf(TONE_DBFS / 20.0);
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc=size=320x180:rate=30:duration={seconds}"))
        .args(["-f", "lavfi", "-i"])
        // Two channels in the source itself: ffmpeg would spread a mono
        // tone over a stereo pair at −3 dB each.
        .arg(format!(
            "aevalsrc={amplitude}*sin(2*PI*1000*t)|{amplitude}*sin(2*PI*1000*t):s=48000:d={seconds}"
        ))
        .args(["-c:a", "pcm_s16le", "-c:v", "mpeg4", "-q:v", "4"])
        .arg(&path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not make the clip");
    path
}

/// Asserts that the `what` reading of `snap` is within `tol` of `want`.
fn near(snap: &EngineSnapshot, what: &str, got: Option<f64>, want: f64, tol: f64) {
    let got = got.unwrap_or_else(|| panic!("no {what} reading: {:?}", snap.output));
    assert!(
        (got - want).abs() <= tol,
        "{what}: {got} vs {want}: {:?}",
        snap.output.loudness
    );
}

#[test]
fn the_snapshot_meters_the_program_audio() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let clip = tone_clip(4);
    let record = temp_path("loudness.mp4");
    let mut config = studio(1);
    config.sources[0].input = SourceConfig::File {
        path: clip.clone(),
        loop_playback: true,
        max_width: None,
        audio: true,
        start_s: None,
        duration_s: None,
    };
    config.detector = DetectorSettings::None;
    config.queries.clear();
    config.output.audio_source = Some("stage".into());
    let engine = start(config);
    let h = engine.handle();
    let wide = ShotId("stage/wide".into());
    let idle = h
        .wait_until(WAIT, |s| s.shot(&wide).is_some_and(|v| v.available))
        .expect("the source delivers frames");
    assert_eq!(idle.output.loudness, None, "no session, no meter");
    h.send(EngineCommand::StartOutput {
        rtmp_url: None,
        record_path: Some(record.clone()),
    })
    .unwrap();
    // The short-term window needs 3 s of program audio.
    let live = h
        .wait_until(WAIT, |s| {
            s.output.state == OutputState::Running
                && s.output.loudness.is_some_and(|l| l.short_term.is_some())
        })
        .expect("the meter fills its short-term window");
    let reading = live.output.loudness.unwrap();
    near(&live, "short-term", reading.short_term, TONE_DBFS, 0.5);
    near(&live, "integrated", reading.integrated, TONE_DBFS, 0.5);
    near(&live, "sample peak", reading.sample_peak, TONE_DBFS, 0.05);
    let (tp, sp) = (reading.true_peak.unwrap(), reading.sample_peak.unwrap());
    assert!(tp >= sp && tp < TONE_DBFS + 3.0, "{tp} vs {sp}");
    // Silence the pacer inserted counts too: the meter's timeline is the
    // encoder's, which follows the video frames pushed.
    let video = live.output.frames_pushed as f64 / 30.0;
    let audio = reading.measured.as_secs_f64();
    assert!(
        (audio - video).abs() < 0.25,
        "{audio} s of audio, {video} s of video"
    );

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
    // The session's readings stay, final, after it ends.
    let last = done.output.loudness.expect("the session's readings");
    assert!(last.measured >= reading.measured);
    near(&done, "integrated", last.integrated, TONE_DBFS, 0.5);
    let report = engine.shutdown().unwrap();
    assert_eq!(report.final_snapshot.output.loudness, Some(last));
    let _ = std::fs::remove_file(&record);
    let _ = std::fs::remove_file(&clip);
}
