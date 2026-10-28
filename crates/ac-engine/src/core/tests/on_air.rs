//! What is on air at start, and whether it is black: with
//! `switcher.start_on_wide` the first source's wide is on program from the
//! first tick (before its source delivers, then with its picture), and the
//! director starts from it; without it the program starts black. The
//! black check (`render::is_black`) and the start of the run of black
//! frames the snapshot publishes, tick by tick on simulated time, and the
//! program-black alarm it raises once the output runs.

use std::sync::Arc;

use ac_core::SourceId;

use super::*;
use crate::core::render::is_black;

/// [`config`] with `subjects` subjects and `switcher.start_on_wide` set.
fn starting_on_wide(subjects: u32) -> StudioConfig {
    let mut c = config(subjects);
    c.switcher.start_on_wide = true;
    c
}

/// A `width` x `height` frame of the rig's source filled with `rgb`.
fn filled(width: u32, height: u32, rgb: [u8; 3]) -> VideoFrame {
    let px = [rgb[0], rgb[1], rgb[2], 255];
    let data: Vec<u8> = px.repeat(width as usize * height as usize);
    VideoFrame::new(cam(), 0, Duration::ZERO, width, height, data.into()).unwrap()
}

/// The master-clock time of tick `n` (from 0) at the rig's rate.
fn at(n: u64) -> Duration {
    Duration::from_secs_f64(n as f64 / FPS)
}

#[test]
fn start_on_wide_puts_the_wide_on_program_at_the_first_tick() {
    let mut rig = Rig::new(starting_on_wide(1));
    // Already before the first tick, so the first snapshot shows it.
    assert_eq!(rig.core.switcher().program(), Some(&shot("cam/wide")));
    let out = rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.tick, 1);
    assert_eq!(snap.switcher.program, Some(shot("cam/wide")));
    assert_eq!(snap.switcher.preview, None);
    assert!(snap.shot(&shot("cam/wide")).unwrap().tally.program);
    assert_eq!(snap.program_changes, 1, "booked on the first tick");
    // The source delivered on that tick, so the wide shows its picture.
    assert_eq!(out.program.pixel(1, 1), rig.frame().pixel(1, 1));
    assert!(snap.render_issues.is_empty());
    assert_eq!(snap.program_black_since, None);
}

#[test]
fn without_start_on_wide_the_program_starts_black_and_says_since_when() {
    let mut rig = Rig::new(config(1));
    assert!(!config(1).switcher.start_on_wide, "off by default");
    rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, None);
    assert_eq!(snap.program_changes, 0);
    assert_eq!(snap.program_black_since, Some(Duration::ZERO));
    // The run of black frames keeps its start while it lasts.
    rig.run(44);
    let snap = rig.snapshot();
    assert_eq!(snap.media_time, at(44));
    assert_eq!(snap.program_black_since, Some(Duration::ZERO));
    // A take ends it on the next tick.
    preview_and_cut(&mut rig, "cam/wide");
    rig.step();
    assert_eq!(rig.snapshot().program_black_since, None);
}

#[test]
fn start_on_wide_before_the_source_delivers_shows_black_then_the_picture() {
    let config = starting_on_wide(1);
    let info = SourceInfo {
        id: cam(),
        width: W,
        height: H,
        fps: FPS,
        audio: None,
    };
    let mut core = EngineCore::new(&config, vec![info], &[false]).unwrap();
    let waiting = SourceFeed {
        state: SourceState::Starting,
        frame: None,
        detector_loading: false,
        error: None,
        info: None,
    };
    for n in 0..5 {
        core.tick(at(n), std::slice::from_ref(&waiting), Vec::new());
    }
    let snap = core.snapshot(OutputView::default(), TimingView::default(), true);
    assert_eq!(snap.switcher.program, Some(shot("cam/wide")));
    assert_eq!(snap.program_black_since, Some(Duration::ZERO));
    assert_eq!(snap.render_issues, ["cam/wide: no picture"]);
    assert!(!snap.sources[0].available);
    // The first frame puts the picture on air with nobody taking a shot.
    let pattern = SyntheticPattern::new(W, H, FPS, 1).unwrap();
    let delivering = SourceFeed {
        state: SourceState::Running,
        frame: Some(pattern.frame(&cam(), 0)),
        ..waiting
    };
    let out = core.tick(at(5), &[delivering], Vec::new());
    let snap = core.snapshot(OutputView::default(), TimingView::default(), true);
    assert_eq!(snap.switcher.program, Some(shot("cam/wide")));
    assert_eq!(snap.program_black_since, None);
    assert!(snap.render_issues.is_empty());
    assert_eq!(snap.program_changes, 1);
    assert_eq!(
        out.program.pixel(1, 1),
        pattern.frame(&cam(), 0).pixel(1, 1)
    );
}

#[test]
fn a_studio_that_starts_on_the_wide_raises_no_alarm_if_its_source_delivers_in_time() {
    use crate::alarms::{AlarmCause, AlarmChange, Alarms, STALE_AFTER};
    let config = starting_on_wide(1);
    let info = SourceInfo {
        id: cam(),
        width: W,
        height: H,
        fps: FPS,
        audio: None,
    };
    let waiting = SourceFeed {
        state: SourceState::Starting,
        frame: None,
        detector_loading: false,
        error: None,
        info: None,
    };
    let pattern = SyntheticPattern::new(W, H, FPS, 1).unwrap();
    // The source's first frame comes on tick `first` (`None`: never); the
    // alarms observe the snapshot before the first tick, then every tick's
    // for three seconds.
    let run = |first: Option<u64>| {
        let mut core = EngineCore::new(&config, vec![info.clone()], &[false]).unwrap();
        let mut alarms = Alarms::new();
        let snap =
            |core: &EngineCore| core.snapshot(OutputView::default(), TimingView::default(), true);
        let mut changes: Vec<(Duration, AlarmChange)> = alarms
            .observe(&snap(&core))
            .into_iter()
            .map(|c| (Duration::ZERO, c))
            .collect();
        for n in 0..90 {
            let feed = match first {
                Some(first) if n >= first => SourceFeed {
                    state: SourceState::Running,
                    frame: Some(pattern.frame(&cam(), n - first)),
                    ..waiting.clone()
                },
                _ => waiting.clone(),
            };
            core.tick(at(n), &[feed], Vec::new());
            let snap = snap(&core);
            assert_eq!(snap.switcher.program, Some(shot("cam/wide")));
            changes.extend(alarms.observe(&snap).into_iter().map(|c| (at(n), c)));
        }
        changes
    };
    // The first frame just inside half a second: nothing, from the very
    // first snapshot on.
    assert!(at(15) <= STALE_AFTER);
    assert_eq!(run(Some(0)), []);
    assert_eq!(run(Some(15)), []);
    // A source that has sent nothing for over half a second since the
    // engine started is stale, as one whose newest frame is that old.
    let changes = run(None);
    let [(when, AlarmChange::Raised(alarm))] = &changes[..] else {
        panic!("{changes:?}");
    };
    assert_eq!(*when, at(16), "the first tick past half a second");
    assert_eq!(alarm.cause, AlarmCause::ProgramSourceStale(cam()));
    assert_eq!(alarm.message, "program source cam has sent no picture yet");
}

#[test]
fn the_director_starts_from_the_wide_in_auto_mode() {
    let mut c = starting_on_wide(2);
    c.director.mode = DirectorMode::Auto;
    c.director.profile = ContentProfile::Music;
    let mut rig = Rig::new(c);
    rig.step();
    assert_eq!(rig.snapshot().switcher.program, Some(shot("cam/wide")));
    rig.run_secs(30.0);
    let snap = rig.snapshot();
    assert!(snap.director.takes >= 1, "takes {}", snap.director.takes);
    assert!(snap.program_changes >= 2, "{}", snap.program_changes);
}

#[test]
fn a_source_that_sends_black_makes_the_program_black_until_it_shows_a_picture() {
    let mut rig = Rig::new(starting_on_wide(1));
    rig.run(30);
    assert_eq!(rig.snapshot().program_black_since, None);
    rig.still = Some(filled(W, H, [0, 0, 0]));
    rig.run(10);
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(shot("cam/wide")));
    assert!(snap.render_issues.is_empty(), "drawn, but black");
    assert_eq!(snap.program_black_since, Some(at(30)));
    rig.still = None;
    rig.step();
    assert_eq!(rig.snapshot().program_black_since, None);
    // A new run of black starts its own count.
    rig.still = Some(filled(W, H, [8, 8, 8]));
    rig.run(3);
    assert_eq!(rig.snapshot().program_black_since, Some(at(41)));
}

#[test]
fn black_means_nearly_every_sampled_pixel_dark() {
    // The synthetic picture's dark background is not black, nor is a
    // dim grey; video black and pure black are.
    let pattern = SyntheticPattern::new(W, H, FPS, 0).unwrap();
    assert!(!is_black(&pattern.frame(&cam(), 0)));
    assert!(!is_black(&filled(W, H, [20, 20, 20])));
    assert!(is_black(&filled(W, H, [16, 16, 16])));
    assert!(is_black(&filled(W, H, [0, 0, 0])));
    // Luma weighs green most: dark red and blue pass, dark green does not.
    assert!(is_black(&filled(W, H, [60, 0, 0])));
    assert!(is_black(&filled(W, H, [0, 0, 200])));
    assert!(!is_black(&filled(W, H, [0, 30, 0])));
}

#[test]
fn a_small_caption_on_black_is_still_black_but_a_picture_is_not() {
    let black = filled(W, H, [0, 0, 0]);
    let with_box = |w: u32, h: u32| {
        let mut data = black.data.to_vec();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * W + x) * 4) as usize;
                data[i..i + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        VideoFrame {
            data: Arc::from(data),
            ..black.clone()
        }
    };
    // One grid cell in white is under 2% of the samples...
    assert!(is_black(&with_box(W / 32, H / 18)));
    // ...a quarter of the picture is not.
    assert!(!is_black(&with_box(W / 2, H / 2)));
}

#[test]
fn tiny_empty_and_malformed_frames_are_judged_without_panicking() {
    // Smaller than the grid: every pixel is a sample.
    assert!(is_black(&filled(1, 1, [0, 0, 0])));
    assert!(!is_black(&filled(1, 1, [255, 255, 255])));
    assert!(!is_black(&filled(3, 2, [128, 128, 128])));
    // No pixels at all shows nothing.
    let empty = VideoFrame {
        width: 0,
        height: 0,
        data: Arc::from(Vec::new()),
        ..filled(1, 1, [255, 255, 255])
    };
    assert!(is_black(&empty));
    // Data shorter than its size: the missing samples count as dark.
    let short = VideoFrame {
        data: Arc::from(vec![255u8; 4 * W as usize]),
        ..filled(W, H, [255, 255, 255])
    };
    assert!(is_black(&short));
    let other = VideoFrame {
        source: SourceId("elsewhere".into()),
        ..filled(4, 4, [200, 200, 200])
    };
    assert!(!is_black(&other), "the source does not matter");
}

#[test]
fn a_black_program_on_air_raises_its_alarm_after_two_seconds_of_ticks() {
    use crate::alarms::{AlarmCause, Alarms, BLACK_AFTER};
    use crate::snapshot::OutputState;
    let mut rig = Rig::new(config(1));
    let mut alarms = Alarms::new();
    let on_air = || OutputView {
        state: OutputState::Running,
        ..OutputView::default()
    };
    let mut raised = None;
    for _ in 0..90 {
        rig.step();
        let snap = rig.core.snapshot(on_air(), TimingView::default(), true);
        if !alarms.observe(&snap).is_empty() && raised.is_none() {
            raised = Some(snap.media_time);
        }
    }
    assert_eq!(
        raised,
        Some(BLACK_AFTER),
        "nothing taken: black from tick 0"
    );
    assert!(alarms.get(&AlarmCause::ProgramBlack).is_some());
    // Taking the wide clears it a second later.
    preview_and_cut(&mut rig, "cam/wide");
    for _ in 0..30 {
        rig.step();
        alarms.observe(&rig.core.snapshot(on_air(), TimingView::default(), true));
    }
    assert!(alarms.get(&AlarmCause::ProgramBlack).is_some());
    rig.step();
    alarms.observe(&rig.core.snapshot(on_air(), TimingView::default(), true));
    assert!(alarms.active().is_empty(), "{:?}", alarms.active());
}
