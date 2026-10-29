//! Sources other than the phase reference, shown through their presentation
//! buffers (`ac_engine::presentation`), in the running engine on a sped-up
//! clock: the core is fed exactly the frames the buffer presents, the
//! buffer is filled between ticks too, a stalled source is shown stale in
//! the snapshot, and the snapshot carries what the runtime reports.
//!
//! These runs are paced in real time, so they check the wiring and what the
//! base engine could not do; the exact cadence (one repeat per 1,001 ticks,
//! no double skips) is checked on simulated clocks next to the buffer.

mod common;

use std::time::Duration;

use ac_core::{ShotId, SourceId};
use ac_engine::presentation::PresentationStats;
use ac_engine::{
    DetectorSettings, EngineCommand, EngineCore, EngineSnapshot, OutputView, SourceEntry,
    SourceView, StudioConfig, TimingView,
};
use ac_media::{SourceInfo, SourceState};
use ac_sources::SourceConfig;

use common::{WAIT, have, start, studio, temp_path};

/// The shot with `id`.
fn shot(id: &str) -> ShotId {
    ShotId(id.into())
}

/// `config` with a second source `id` reading `input`.
fn with_source(mut config: StudioConfig, id: &str, input: SourceConfig) -> StudioConfig {
    config.sources.push(SourceEntry {
        id: id.into(),
        name: None,
        input,
    });
    config
}

/// The presentation counts of the source at `i`, which must have a buffer.
fn counts(snap: &EngineSnapshot, i: usize) -> PresentationStats {
    snap.sources[i]
        .presentation
        .expect("a source other than the phase reference has a buffer")
}

/// Checks that the phase reference (the first source, without an audio
/// source) is read as it is and that the core was fed exactly the frames
/// `view`'s buffer presented.
fn assert_fed_from_the_buffer(snap: &EngineSnapshot, view: &SourceView) {
    assert_eq!(snap.sources[0].presentation, None, "the phase reference");
    let p = view.presentation.expect("a buffer");
    assert_eq!(view.frames, p.new, "{view:?}");
    assert!(p.locks >= 1, "{p:?}");
}

#[test]
fn a_second_source_is_presented_tracked_and_put_on_air() {
    let mut config = studio(1);
    let second = config.sources[0].clone();
    config.sources.push(SourceEntry {
        id: "side".into(),
        ..second
    });
    let engine = start(config);
    let h = engine.handle();
    h.wait_until(WAIT, |s| {
        s.sources[1].frames > 60 && s.shot(&shot("side/1/medium")).is_some_and(|v| v.available)
    })
    .expect("the second source runs and its subject is framed");
    h.send(EngineCommand::SetPreview {
        shot: shot("side/1/medium"),
    })
    .unwrap();
    h.send(EngineCommand::Cut {
        preview: Some(shot("side/1/medium")),
    })
    .unwrap();
    let snap = h
        .wait_until(WAIT, |s| s.switcher.program == Some(shot("side/1/medium")))
        .expect("on air");
    let (stage, side) = (&snap.sources[0], &snap.sources[1]);
    assert!(side.available && side.vision.frames > 0, "{side:?}");
    assert_fed_from_the_buffer(&snap, side);
    // The buffer shows the source a frame or two late but keeps up with it.
    assert!(side.frames + 10 >= stage.frames, "{stage:?} {side:?}");
    assert!(snap.render_issues.is_empty(), "{:?}", snap.render_issues);
    let report = engine.shutdown().unwrap();
    assert!(!report.final_snapshot.running);
}

/// A 60 fps source into the 30 fps program is shown every other frame. The
/// frames in between are passed over by the buffer only because the engine
/// collects frames between ticks; collected at the ticks alone, they would
/// be lost in the mailbox before the buffer saw them, and none skipped.
#[test]
fn a_faster_source_is_decimated_from_frames_collected_between_ticks() {
    let mut config = with_source(
        studio(1),
        "fast",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 60.0,
            subjects: 1,
        },
    );
    config.detector = DetectorSettings::None;
    let engine = start(config);
    let h = engine.handle();
    let snap = h
        .wait_until(WAIT, |s| {
            s.sources[1].presentation.is_some_and(|p| p.new >= 150)
        })
        .expect("the fast source is presented");
    let fast = &snap.sources[1];
    assert_fed_from_the_buffer(&snap, fast);
    let p = counts(&snap, 1);
    // About one frame passed over per frame presented; a quarter leaves
    // room for frames lost to a busy machine.
    assert!(p.skipped * 4 >= p.new, "{p:?}");
    assert!(fast.missed >= p.new / 4, "{fast:?}");
    assert!(!p.stale && p.stale_ticks == 0, "{p:?}");
    engine.shutdown().unwrap();
}

/// A file played to its end stands in for a stalled input: no new frame
/// arrives. About a second after its last frame the snapshot shows it
/// stale, and its last frame is not presented again for as long as that
/// lasts, while the core keeps the source unavailable.
#[test]
fn a_source_whose_frames_stop_is_shown_stale_and_not_repeated() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not found");
        return;
    }
    let path = temp_path("stops.mkv");
    let made = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=30:duration=2",
        ])
        .args(["-c:v", "mpeg4", "-q:v", "4"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success(), "ffmpeg could not make a clip");
    let mut config = with_source(
        studio(1),
        "clip",
        SourceConfig::File {
            path: path.clone(),
            loop_playback: false,
            max_width: None,
            audio: false,
            start_s: None,
            duration_s: None,
        },
    );
    // The synthetic detector needs every source synthetic.
    config.detector = DetectorSettings::None;
    let engine = start(config);
    let h = engine.handle();
    h.wait_until(WAIT, |s| {
        s.sources[1].presentation.is_some_and(|p| p.new >= 20)
    })
    .expect("the clip plays");
    let stalled = h
        .wait_until(WAIT, |s| s.sources[1].presentation.is_some_and(|p| p.stale))
        .expect("the clip goes stale once its frames stop");
    let clip = &stalled.sources[1];
    assert_fed_from_the_buffer(&stalled, clip);
    assert!(!clip.available, "{clip:?}");
    assert!(
        clip.frame_age
            .is_some_and(|age| age >= Duration::from_millis(900)),
        "stale only after about a second: {clip:?}"
    );
    let before = counts(&stalled, 1);
    assert!(before.stale_ticks >= 1, "{before:?}");
    let later = h
        .wait_until(WAIT, |s| {
            s.media_time >= stalled.media_time + Duration::from_secs(1)
        })
        .expect("the engine runs on");
    let after = counts(&later, 1);
    assert!(after.stale, "still stale: {after:?}");
    assert_eq!(
        (after.new, after.repeats),
        (before.new, before.repeats),
        "nothing presented while stale"
    );
    assert!(after.stale_ticks >= before.stale_ticks + 20, "{after:?}");
    assert_eq!(later.sources[1].frames, clip.frames);
    assert_eq!(later.sources[1].state, SourceState::Stopped);
    assert!(later.sources[0].available, "the stage runs on");
    engine.shutdown().unwrap();
    let _ = std::fs::remove_file(&path);
}

/// The core carries what the runtime reports of its buffers into the
/// snapshot, by source, and shows nothing for a source without a report.
#[test]
fn the_snapshot_shows_the_reported_presentation_counts() {
    let config = with_source(
        studio(1),
        "side",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 1,
        },
    );
    let info = |id: &str| SourceInfo {
        id: SourceId(id.into()),
        width: 320,
        height: 180,
        fps: 30.0,
        audio: None,
    };
    let mut core = EngineCore::new(&config, vec![info("stage"), info("side")], &[false, false])
        .expect("a core");
    let snapshot =
        |core: &EngineCore| core.snapshot(OutputView::default(), TimingView::default(), true);
    assert!(
        snapshot(&core)
            .sources
            .iter()
            .all(|s| s.presentation.is_none())
    );
    let reported = PresentationStats {
        new: 3,
        repeats: 1,
        skipped: 2,
        stale_ticks: 4,
        stale: true,
        locks: 1,
    };
    // An entry past the last source is ignored.
    core.note_presentation([None, Some(reported), Some(reported)]);
    let snap = snapshot(&core);
    assert_eq!(snap.sources[0].presentation, None);
    assert_eq!(snap.sources[1].presentation, Some(reported));
    // A source past the last entry keeps its report.
    core.note_presentation([None]);
    assert_eq!(snapshot(&core).sources[1].presentation, Some(reported));
}
