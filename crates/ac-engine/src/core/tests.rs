//! Deterministic tests of the engine core: no threads, no clocks.
//!
//! A [`Rig`] plays a synthetic pattern (or a still of it) into the core one
//! tick at a time and runs discovery inline, answering each request on the
//! next tick, which is what the runtime does with a fast detector.

use std::time::Duration;

use ac_core::{Preset, QueryId, Rect, ShotId, ShotSize, SourceId, SubjectId, TrackState};
use ac_director::{ContentProfile, DirectorMode};
use ac_media::{SourceInfo, SourceState, VideoFrame};
use ac_sources::{SourceConfig, SyntheticPattern};
use ac_switcher::SwitchError;
use ac_vision::{Detection, Detector, PromptQuery, ScriptedDetector};

use super::*;
use crate::command::{CommandError, CommandReply, EngineCommand};
use crate::config::{DetectorSettings, MonitorSettings, PresetSpec, QuerySettings, StudioConfig};
use crate::discovery::synthetic_detector;
use crate::snapshot::{EngineSnapshot, OutputView, TimingView};

mod feedback;
mod framing;
mod protection;
mod shared;
mod switching;
mod vision;

/// Source and output width.
const W: u32 = 320;
/// Source and output height.
const H: u32 = 180;
/// Frame rate.
const FPS: f64 = 30.0;

/// The rig's source id.
fn cam() -> SourceId {
    SourceId("cam".into())
}

/// A synthetic studio with `subjects` subjects, ground-truth discovery,
/// one query with medium and close framings, no director and no monitors.
fn config(subjects: u32) -> StudioConfig {
    let mut c = StudioConfig::with_source(
        "cam",
        SourceConfig::Synthetic {
            width: W,
            height: H,
            fps: FPS,
            subjects,
        },
    );
    c.detector = DetectorSettings::Synthetic { score: 0.9 };
    c.queries.push(QuerySettings {
        id: None,
        name: "Person".into(),
        prompt: "person".into(),
        presets: Some(vec![
            PresetSpec::Size(ShotSize::Medium),
            PresetSpec::Size(ShotSize::Close),
        ]),
        literal: false,
    });
    c.output.width = W;
    c.output.height = H;
    c.director.mode = DirectorMode::Off;
    c.monitors = MonitorSettings::disabled();
    c
}

/// Plays frames into a core and runs discovery inline.
struct Rig {
    core: EngineCore,
    pattern: SyntheticPattern,
    detector: Box<dyn Detector>,
    pending: Vec<DiscoveryResult>,
    seq: u64,
    /// When false, the source repeats its last frame (a stall).
    advancing: bool,
    /// Overrides the picture (for example a blank frame).
    picture: Option<SyntheticPattern>,
    /// Freezes the picture: every frame repeats these pixels under its own
    /// sequence number and PTS, so the subjects in it stand still.
    still: Option<VideoFrame>,
    /// Reports the detector as still loading.
    loading: bool,
}

impl Rig {
    /// A rig for `config`, whose first source must be synthetic.
    fn new(config: StudioConfig) -> Self {
        let SourceConfig::Synthetic {
            width,
            height,
            fps,
            subjects,
        } = config.sources[0].input
        else {
            panic!("rig needs a synthetic source");
        };
        let pattern = SyntheticPattern::new(width, height, fps, subjects).unwrap();
        let info = SourceInfo {
            id: cam(),
            width,
            height,
            fps,
            audio: None,
        };
        let core = EngineCore::new(&config, vec![info], &[true]).unwrap();
        Self {
            core,
            detector: Box::new(synthetic_detector(pattern.clone(), 0.9)),
            pattern,
            pending: Vec::new(),
            seq: 0,
            advancing: true,
            picture: None,
            still: None,
            loading: false,
        }
    }

    /// Replaces the ground-truth detector.
    fn with_detector(mut self, d: impl Detector + 'static) -> Self {
        self.detector = Box::new(d);
        self
    }

    /// The frame the source delivers now.
    fn frame(&self) -> VideoFrame {
        let pattern = self.picture.as_ref().unwrap_or(&self.pattern);
        match &self.still {
            Some(still) => VideoFrame {
                seq: self.seq,
                pts: pattern.frame_rate().pts(self.seq),
                ..still.clone()
            },
            None => pattern.frame(&cam(), self.seq),
        }
    }

    /// One tick: feed the frame, apply last tick's results, answer requests.
    fn step(&mut self) -> TickOutput {
        let now = Duration::from_secs_f64(self.core.ticks() as f64 / FPS);
        let frame = self.frame();
        let feed = SourceFeed {
            state: SourceState::Running,
            frame: Some(frame),
            detector_loading: self.loading,
            error: None,
            info: None,
        };
        let results = std::mem::take(&mut self.pending);
        let out = self.core.tick(now, &[feed], results);
        for (i, req) in &out.requests {
            let outcome = self
                .detector
                .detect(&req.frame, &req.queries)
                .map_err(|e| e.to_string());
            self.pending.push(DiscoveryResult {
                source: *i,
                frame: req.frame.clone(),
                queries: req.queries.iter().map(|q| q.id.clone()).collect(),
                outcome,
                latency: Duration::ZERO,
            });
        }
        if self.advancing {
            self.seq += 1;
        }
        out
    }

    /// Runs `ticks` ticks.
    fn run(&mut self, ticks: usize) {
        for _ in 0..ticks {
            self.step();
        }
    }

    /// Runs `secs` seconds of ticks.
    fn run_secs(&mut self, secs: f64) {
        self.run((secs * FPS).round() as usize);
    }

    /// Applies a command between ticks.
    fn apply(&mut self, cmd: EngineCommand) -> Result<CommandReply, CommandError> {
        self.core.apply(&cmd)
    }

    /// The core's snapshot, without output or timing.
    fn snapshot(&self) -> EngineSnapshot {
        self.core
            .snapshot(OutputView::default(), TimingView::default(), true)
    }
}

/// The shot with `id`.
fn shot(id: &str) -> ShotId {
    ShotId(id.into())
}

/// Stages `id` and cuts it to program.
fn preview_and_cut(rig: &mut Rig, id: &str) {
    rig.apply(EngineCommand::SetPreview { shot: shot(id) })
        .unwrap();
    rig.apply(EngineCommand::Cut).unwrap();
}

#[test]
fn subjects_are_discovered_and_shots_appear() {
    let mut rig = Rig::new(config(2));
    rig.step();
    assert_eq!(
        rig.core.catalog().len(),
        1,
        "only the wide before discovery"
    );
    rig.run(30);
    let snap = rig.snapshot();
    let ids: Vec<&str> = snap.shots.iter().map(|s| s.id.0.as_str()).collect();
    assert_eq!(
        ids,
        [
            "cam/wide",
            "cam/1/medium",
            "cam/1/close",
            "cam/2/medium",
            "cam/2/close"
        ]
    );
    assert_eq!(snap.subjects.len(), 2);
    assert!(
        snap.subjects
            .iter()
            .all(|s| s.state == TrackState::Tracking)
    );
    let src = &snap.sources[0];
    assert!(src.available && src.discovery.applied >= 1 && src.frames == 31);
    assert_eq!(src.subjects, 2);
    assert!(snap.subject_shots().all(|s| s.available));
}

#[test]
fn program_is_black_until_something_is_taken() {
    let mut rig = Rig::new(config(1));
    let out = rig.step();
    assert_eq!((out.program.width, out.program.height), (W, H));
    assert!(out.program.data.chunks(4).all(|p| p == [0, 0, 0, 255]));
    preview_and_cut(&mut rig, "cam/wide");
    let out = rig.step();
    assert_eq!(
        out.program.pixel(1, 1).unwrap(),
        rig.frame().pixel(1, 1).unwrap()
    );
    assert!(rig.snapshot().render_issues.is_empty());
}

#[test]
fn a_stalled_source_makes_its_shots_unavailable() {
    let mut rig = Rig::new(config(1));
    rig.run(31);
    rig.advancing = false;
    rig.run_secs(1.5);
    let snap = rig.snapshot();
    assert!(!snap.sources[0].available);
    assert!(snap.shots.iter().all(|s| !s.available));
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/wide"),
    })
    .unwrap();
    assert_eq!(
        rig.apply(EngineCommand::Cut),
        Err(CommandError::Switch(SwitchError::Unavailable(shot(
            "cam/wide"
        ))))
    );
    rig.advancing = true;
    rig.run(2);
    assert!(rig.snapshot().sources[0].available);
    rig.apply(EngineCommand::Cut).unwrap();
    // Repeats are not misses; a jump in sequence numbers is.
    assert_eq!(rig.snapshot().sources[0].missed, 0);
    rig.seq += 4;
    rig.run(1);
    assert_eq!(rig.snapshot().sources[0].missed, 4);
}

#[test]
fn queries_can_be_added_and_removed() {
    let mut rig = Rig::new(config(2));
    rig.run(31);
    let added = rig.apply(EngineCommand::AddQuery {
        name: "Lead Guitar".into(),
        prompt: "guitar".into(),
        presets: Some(vec![Preset::new("full", "Full", ShotSize::Full)]),
        literal: false,
    });
    assert_eq!(
        added,
        Ok(CommandReply::QueryAdded {
            id: QueryId("lead-guitar".into())
        })
    );
    assert_eq!(
        rig.apply(EngineCommand::AddQuery {
            name: "lead guitar".into(),
            prompt: "guitar".into(),
            presets: Some(vec![Preset::new("full", "Full", ShotSize::Full)]),
            literal: false,
        }),
        Err(CommandError::DuplicateQuery(QueryId("lead-guitar".into())))
    );
    for (name, prompt, presets) in [
        ("", "x", Some(vec![Preset::new("m", "M", ShotSize::Medium)])),
        ("A", "", Some(vec![Preset::new("m", "M", ShotSize::Medium)])),
        ("B", "x", Some(vec![])),
        // Shot words with nothing to frame.
        ("C", "close-up", None),
    ] {
        assert!(matches!(
            rig.apply(EngineCommand::AddQuery {
                name: name.into(),
                prompt: prompt.into(),
                presets,
                literal: false,
            }),
            Err(CommandError::Invalid(_))
        ));
    }
    assert_eq!(rig.core.queries().len(), 2);
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId("person".into()),
    })
    .unwrap();
    rig.run(2);
    // The guitar query aliased the same boxes, so the subjects live on under it.
    let snap = rig.snapshot();
    assert!(
        snap.subjects
            .iter()
            .all(|s| s.query == QueryId("lead-guitar".into()))
    );
    assert!(
        snap.shots
            .iter()
            .all(|s| s.preset.as_deref() != Some("medium"))
    );
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId("lead-guitar".into()),
    })
    .unwrap();
    rig.run(2);
    assert_eq!(rig.core.catalog().len(), 1);
    assert_eq!(
        rig.apply(EngineCommand::RemoveQuery {
            query: QueryId("lead-guitar".into())
        }),
        Err(CommandError::UnknownQuery(QueryId("lead-guitar".into())))
    );
}

#[test]
fn hiding_removes_shots_and_pin_and_reacquire_validate() {
    let mut rig = Rig::new(config(2));
    rig.run(31);
    let hide = |hidden| EngineCommand::Hide {
        source: cam(),
        subject: SubjectId(2),
        hidden,
    };
    rig.apply(hide(true)).unwrap();
    rig.step();
    assert!(rig.snapshot().shot(&shot("cam/2/medium")).is_none());
    assert!(rig.snapshot().subjects.iter().any(|s| s.hidden));
    rig.apply(hide(false)).unwrap();
    rig.step();
    assert!(rig.snapshot().shot(&shot("cam/2/medium")).is_some());
    rig.apply(EngineCommand::Pin {
        source: cam(),
        subject: SubjectId(1),
        pinned: true,
    })
    .unwrap();
    assert!(rig.snapshot().subjects[0].pinned);
    assert!(matches!(
        rig.apply(EngineCommand::Pin {
            source: cam(),
            subject: SubjectId(99),
            pinned: true
        }),
        Err(CommandError::Store(_))
    ));
    assert_eq!(
        rig.apply(EngineCommand::Pin {
            source: SourceId("x".into()),
            subject: SubjectId(1),
            pinned: true
        }),
        Err(CommandError::UnknownSource(SourceId("x".into())))
    );
    let gt = rig.pattern.ground_truth(rig.frame().pts)[0].bbox;
    rig.apply(EngineCommand::Reacquire {
        source: cam(),
        subject: SubjectId(1),
        bbox: gt,
    })
    .unwrap();
    assert!(matches!(
        rig.apply(EngineCommand::Reacquire {
            source: cam(),
            subject: SubjectId(1),
            bbox: Rect::new(f64::NAN, 0.0, 0.1, 0.1)
        }),
        Err(CommandError::Store(_))
    ));
}

#[test]
fn suggest_mode_stages_the_suggestion_without_taking() {
    let mut c = config(2);
    c.director.mode = DirectorMode::Suggest;
    let mut rig = Rig::new(c);
    rig.run_secs(2.0);
    let snap = rig.snapshot();
    assert_eq!(snap.director.suggestion, Some(shot("cam/wide")));
    assert_eq!(snap.switcher.preview, Some(shot("cam/wide")));
    assert_eq!(snap.switcher.program, None);
    assert!(!snap.director.reason_text.is_empty());
    rig.apply(EngineCommand::AcceptSuggestion).unwrap();
    rig.run_secs(1.0);
    assert_eq!(rig.snapshot().switcher.program, Some(shot("cam/wide")));
    rig.apply(EngineCommand::SetDirectorMode {
        mode: DirectorMode::Off,
    })
    .unwrap();
    rig.step();
    assert_eq!(
        rig.apply(EngineCommand::AcceptSuggestion),
        Err(CommandError::NoSuggestion)
    );
}

#[test]
fn auto_mode_switches_between_shots_on_its_own() {
    let mut c = config(2);
    c.director.mode = DirectorMode::Auto;
    c.director.profile = ContentProfile::Music;
    let mut rig = Rig::new(c);
    rig.run_secs(30.0);
    let snap = rig.snapshot();
    assert!(snap.switcher.program.is_some());
    assert!(snap.director.takes >= 2, "takes {}", snap.director.takes);
    assert!(
        snap.program_changes >= 3,
        "changes {}",
        snap.program_changes
    );
    rig.apply(EngineCommand::SetDirectorProfile {
        profile: ContentProfile::Talk,
    })
    .unwrap();
    assert_eq!(rig.snapshot().director.profile, ContentProfile::Talk);
    assert_eq!(rig.snapshot().director.mode, DirectorMode::Auto);
}

#[test]
fn monitors_and_tiles_refresh_at_their_rates() {
    let mut c = config(1);
    c.monitors = MonitorSettings {
        width: 160,
        height: 90,
        rate_hz: 10.0,
        thumb_width: 64,
        thumb_height: 36,
        thumb_rate_hz: 5.0,
    };
    let mut rig = Rig::new(c);
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/medium"),
    })
    .unwrap();
    rig.run(7);
    let snap = rig.snapshot();
    let pgm = snap.program_monitor.as_ref().unwrap();
    assert_eq!((pgm.width, pgm.height), (160, 90));
    assert!(snap.preview_monitor.is_some());
    let tile = snap.shots[0].thumbnail.as_ref().unwrap();
    assert_eq!((tile.width, tile.height), (64, 36));
    // Tiles are reused between refreshes: at 5 Hz under 30 fps, six ticks
    // share one tile.
    let mut seqs = std::collections::BTreeSet::new();
    for _ in 0..12 {
        rig.step();
        seqs.insert(rig.snapshot().shots[0].thumbnail.as_ref().unwrap().seq);
    }
    assert!((2..=3).contains(&seqs.len()), "{seqs:?}");
}

#[test]
fn the_core_is_deterministic() {
    let run = || {
        let mut c = config(3);
        c.director.mode = DirectorMode::Auto;
        let mut rig = Rig::new(c);
        let mut last = None;
        for _ in 0..240 {
            last = Some(rig.step().program);
        }
        let snap = rig.snapshot();
        let crops: Vec<Rect> = snap.shots.iter().map(|s| s.crop).collect();
        (last.unwrap().data, crops, snap.switcher.program)
    };
    let (a, b) = (run(), run());
    assert_eq!(a.1, b.1);
    assert_eq!(a.2, b.2);
    assert!(a.0 == b.0, "program frames differ");
}

#[test]
fn core_rejects_mismatched_source_descriptions() {
    let c = config(1);
    assert!(EngineCore::new(&c, vec![], &[]).is_err());
    let empty = SourceInfo {
        id: cam(),
        width: 0,
        height: 0,
        fps: 30.0,
        audio: None,
    };
    assert!(EngineCore::new(&c, vec![empty], &[true]).is_err());
}
