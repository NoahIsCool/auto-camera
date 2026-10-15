//! Tests for the operator-facing text.

use super::*;
use crate::ui::fixtures::{encoder_stats, recording, snapshot, subject_shot};

#[test]
fn numbers_are_rounded_and_clamped() {
    assert_eq!(percent(0.874), "87%");
    assert_eq!(percent(1.7), "100%");
    assert_eq!(percent(-1.0), "0%");
    assert_eq!(millis(Duration::from_micros(4250)), "4.2 ms");
    assert_eq!(clock(Duration::from_secs(75)), "1:15");
    assert_eq!(clock(Duration::from_secs(3725)), "1:02:05");
    assert_eq!(clock(Duration::ZERO), "0:00");
}

#[test]
fn shot_status_reads_like_a_caption() {
    assert_eq!(
        shot_status(&subject_shot("a", ShotStatus::Tracking, 0.81)),
        "tracking · 81%"
    );
    assert_eq!(
        shot_status(&subject_shot("a", ShotStatus::Lost, 0.0)),
        "lost"
    );
    assert_eq!(
        shot_status(&subject_shot("a", ShotStatus::Unavailable, 0.0)),
        "no signal"
    );
    let mut snap = snapshot();
    assert_eq!(shot_status(&snap.shots[0]), "no signal", "no frames yet");
    snap.shots[0].status = ShotStatus::Wide;
    assert_eq!(shot_status(&snap.shots[0]), "wide");
}

#[test]
fn shot_names_fall_back_to_ids_and_dashes() {
    let snap = snapshot();
    let wide = ShotId("stage/wide".into());
    assert_eq!(shot_name(&snap, Some(&wide)), "Wide");
    assert_eq!(
        shot_name(&snap, Some(&ShotId("gone/1/close".into()))),
        "gone/1/close"
    );
    assert_eq!(shot_name(&snap, None), NONE);
}

#[test]
fn status_bar_lines_show_rates_timing_and_drops() {
    let mut snap = snapshot();
    let src = &mut snap.sources[0];
    assert_eq!(source_line(src), "stage –/30 fps · 0 missed · NO SIGNAL");
    src.frames = 100;
    src.fps = 29.94;
    src.missed = 3;
    src.available = true;
    assert_eq!(source_line(src), "stage 29.9/30 fps · 3 missed");
    src.discovery.last_latency = Some(Duration::from_millis(12));
    assert_eq!(
        source_line(src),
        "stage 29.9/30 fps · 3 missed · detect 12.0 ms"
    );

    snap.timing.budget_ms = 33.33;
    assert_eq!(timing_line(&snap.timing), "tick – / 33.3 ms");
    snap.timing.tick.count = 10;
    snap.timing.tick.avg_ms = 4.21;
    snap.timing.tick.max_ms = 9.0;
    snap.timing.late_ticks = 1;
    assert_eq!(
        timing_line(&snap.timing),
        "tick 4.2 ms avg · 9.0 max / 33.3 ms · 1 late · 0 skipped"
    );

    snap.timing.skipped_ticks = 4;
    snap.output.stats = Some(encoder_stats());
    assert_eq!(
        dropped_line(&snap),
        "dropped: 3 source · 4 ticks · 2 encoder"
    );
}

#[test]
fn output_text_covers_every_state_and_stat() {
    assert_eq!(output_state(&OutputState::Running), "ON AIR");
    assert_eq!(output_state(&OutputState::Idle), "idle");
    assert_eq!(
        output_state(&OutputState::Failed("ffmpeg exited".into())),
        "failed: ffmpeg exited"
    );
    let mut stats = encoder_stats();
    let lines = encoder_lines(&stats);
    assert_eq!(lines[0], "encoder libx264");
    assert_eq!(lines[1], "frames 300 written · 2 dropped · 2 repeated");
    assert_eq!(lines[2], "30.0 fps · 4000 kbit/s · speed 1.00x");
    assert_eq!(lines[3], "encoded 0:09");
    assert_eq!(lines.len(), 4);
    stats.progress.fps = None;
    stats.last_error = Some("Connection refused".into());
    stats.audio_connected = true;
    let lines = encoder_lines(&stats);
    assert!(lines[2].starts_with("– fps"));
    assert!(lines[4].starts_with("audio 0 samples"));
    assert_eq!(lines[5], "last error: Connection refused");

    assert_eq!(destinations(&OutputView::default()), NONE);
    let mut out = recording("/tmp/show.mp4");
    assert_eq!(destinations(&out), "/tmp/show.mp4");
    out.rtmp_url = Some("rtmp://example.com/app/<key>".into());
    assert_eq!(
        destinations(&out),
        "rtmp://example.com/app/<key> + /tmp/show.mp4"
    );
}
