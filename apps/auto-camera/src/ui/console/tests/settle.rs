//! Window tests of settle-before-take and of the preview monitor's
//! lost-subject warning, on a live engine core the tests tick themselves,
//! as the studio of [`super::switching`] does, but with one source and a
//! detector whose answers the test writes.
//!
//! The stage's detector sees one person standing still until frame
//! [`JUMP`], then bigger and to the left, so their medium and close shots
//! travel to the new box for about a second, and from frame [`GONE`] (only
//! when a test asks for it) sees no one, so the person is lost. Its answers
//! are written as a detector process sends them, in JSON
//! ([`answer`]), and reach the core on the tick after its request, as a
//! fast detector's would. The director is off unless a test turns it on.
//!
//! The rules (when a take waits, what forces it, when it is dropped) are
//! the engine's and are tested there (`ac_engine::settle`, and the core's
//! own tests); these check that the window's keys and buttons reach them,
//! that the preview caption says while a take waits and when its subject
//! is lost, and that a waiting take that is dropped is announced like the
//! refused cut it became.

use super::*;
use crate::ui::notices::notice_for;
use ac_core::Rect;
use ac_engine::discovery::DiscoveryResult;
use ac_engine::settle::{SettleGate, SettleRule, TakeKind};
use ac_engine::{
    CommandError, CommandReply, DetectorSettings, EngineCore, MonitorSettings, OutputView,
    QuerySettings, ShotStatus, SourceFeed, TimingView,
};
use ac_media::{SourceInfo, SourceState};
use ac_sources::{SourceConfig, SyntheticPattern};
use std::time::Duration;

/// Frame and tick rate of the stage.
const FPS: f64 = 30.0;
/// The frame from which the detector sees the person's new box.
const JUMP: u64 = 60;
/// The frame from which the detector sees no one, when a test asks.
const GONE: u64 = 75;
/// What the caption says while a cut waits.
const CUT_WAITS: &str = "CUT waiting for preview to settle · press again to take now";

/// The person's box before [`JUMP`].
fn standing() -> Rect {
    Rect::new(0.45, 0.3, 0.1, 0.5)
}

/// The person's box from [`JUMP`].
fn jumped() -> Rect {
    Rect::new(0.4, 0.25, 0.2, 0.6)
}

/// The detector's answer for frame `seq`, in JSON as a detector process
/// sends it; `leaves` says the person is gone from [`GONE`].
fn answer(seq: u64, leaves: bool) -> serde_json::Value {
    let bbox = match seq {
        s if leaves && s >= GONE => return serde_json::json!([]),
        s if s >= JUMP => jumped(),
        _ => standing(),
    };
    serde_json::json!([{ "query": "person", "bbox": bbox, "score": 0.9 }])
}

/// The window on a live one-source core whose detector the test answers.
struct Stage {
    /// The console.
    console: Console,
    /// The engine core the console's commands go to.
    core: EngineCore,
    /// What the source shows: an empty stage.
    pattern: SyntheticPattern,
    /// Sequence number of the source's next frame.
    seq: u64,
    /// The person leaves at [`GONE`].
    leaves: bool,
    /// The detector's answers, for the next tick.
    pending: Vec<DiscoveryResult>,
    /// Notices of the replies since the last frame.
    raised: Vec<(String, bool)>,
    /// Every command the console returned, with the core's reply.
    sent: Vec<(EngineCommand, Result<CommandReply, CommandError>)>,
}

impl Stage {
    /// The core's snapshot, without output or timing.
    fn snapshot(&self) -> EngineSnapshot {
        self.core
            .snapshot(OutputView::default(), TimingView::default(), true)
    }

    /// Hands `command` to the core, keeping the reply and its notice.
    fn apply(&mut self, command: EngineCommand) {
        let reply = self.core.apply(&command);
        self.raised.extend(notice_for(command.name(), &reply));
        self.sent.push((command, reply));
    }

    /// Ticks the core `ticks` times, a frame each, answering each
    /// discovery request on the next tick.
    fn tick(&mut self, ticks: u64) {
        let id = ac_core::SourceId("stage".into());
        for _ in 0..ticks {
            let now = Duration::from_secs_f64(self.core.ticks() as f64 / FPS);
            let feed = SourceFeed {
                state: SourceState::Running,
                frame: Some(self.pattern.frame(&id, self.seq)),
                detector_loading: false,
                error: None,
                info: None,
            };
            let results = std::mem::take(&mut self.pending);
            let out = self.core.tick(now, &[feed], results);
            for (source, request) in out.requests {
                let outcome = answer(request.frame.seq, self.leaves);
                self.pending.push(DiscoveryResult {
                    source,
                    queries: request.queries.iter().map(|q| q.id.clone()).collect(),
                    frame: request.frame,
                    outcome: Ok(serde_json::from_value(outcome).expect("detections")),
                    latency: Duration::ZERO,
                });
            }
            self.seq += 1;
        }
    }
}

/// A window on a fresh stage with `config` changed by `configure` first,
/// ticked until two frames after the jump: the wide and the person's
/// medium and close shots (tiles 1 to 3) are available, the last two
/// travelling, and nothing is on air.
fn stage_with(configure: impl FnOnce(&mut StudioConfig)) -> Harness<'static, Stage> {
    let mut config = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: FPS,
            subjects: 0,
        },
    );
    config.queries.push(QuerySettings::new("Person", "person"));
    config.detector = DetectorSettings::Synthetic { score: 0.9 };
    config.discovery.rate_hz = 5.0;
    config.director.mode = DirectorMode::Off;
    config.switcher.transition = TransitionKind::Mix;
    config.output.width = 320;
    config.output.height = 180;
    config.monitors = MonitorSettings::disabled();
    configure(&mut config);
    config.validate().expect("a valid stage");
    let info = SourceInfo {
        id: ac_core::SourceId("stage".into()),
        width: 320,
        height: 180,
        fps: FPS,
        audio: None,
    };
    let core = EngineCore::new(&config, vec![info], &[true]).expect("core");
    let first = core.snapshot(OutputView::default(), TimingView::default(), true);
    let mut stage = Stage {
        console: Console::new(&config, &first),
        core,
        pattern: SyntheticPattern::new(320, 180, FPS, 0).expect("pattern"),
        seq: 0,
        leaves: false,
        pending: Vec::new(),
        raised: Vec::new(),
        sent: Vec::new(),
    };
    stage.tick(JUMP + 2);
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .build_ui_state(
            |ui, s: &mut Stage| {
                let snap = s.snapshot();
                let raised = std::mem::take(&mut s.raised);
                let commands = s.console.frame(ui, &snap, raised, false);
                for command in commands {
                    s.apply(command);
                }
            },
            stage,
        );
    h.run();
    let snap = h.state().snapshot();
    let ids: Vec<&str> = snap.shots.iter().map(|s| s.id.0.as_str()).collect();
    assert_eq!(ids, ["stage/wide", "stage/1/medium", "stage/1/close"]);
    assert!(snap.shots.iter().all(|s| s.available), "{:?}", snap.shots);
    assert!(
        snap.shots[1].settled < 0.4,
        "travelling: {}",
        snap.shots[1].settled
    );
    h
}

/// [`stage_with`] settle-before-take on, at its defaults.
fn stage() -> Harness<'static, Stage> {
    stage_with(|c| c.switcher.settle.enabled = true)
}

/// Ticks the stage `ticks` times, then draws until the window settles.
fn advance(h: &mut Harness<'_, Stage>, ticks: u64) {
    h.state_mut().tick(ticks);
    h.run();
}

/// The id of the shot of tile `index` (0-based).
fn shot_of(h: &Harness<'_, Stage>, index: usize) -> ShotId {
    h.state().snapshot().shots[index].id.clone()
}

/// The shot on program.
fn program(h: &Harness<'_, Stage>) -> Option<ShotId> {
    h.state().snapshot().switcher.program
}

/// Takes the commands sent so far, failing on any the core refused.
fn accepted(h: &mut Harness<'_, Stage>) -> Vec<EngineCommand> {
    std::mem::take(&mut h.state_mut().sent)
        .into_iter()
        .map(|(command, reply)| {
            assert!(reply.is_ok(), "{command:?} was refused: {reply:?}");
            command
        })
        .collect()
}

/// True when the preview caption says `note`.
fn says<S>(h: &Harness<'_, S>, note: &str) -> bool {
    h.query_by_label_contains(note).is_some()
}

/// Puts the wide on air with keys 1 and Space (it is settled, so at once),
/// then stages the travelling medium shot with key 2 and presses Space.
fn cut_to_the_travelling_medium(h: &mut Harness<'_, Stage>) {
    press(h, Key::Num1);
    press(h, Key::Space);
    assert_eq!(program(h), Some(shot_of(h, 0)), "a wide never waits");
    press(h, Key::Num2);
    press(h, Key::Space);
    let (wide, medium) = (shot_of(h, 0), shot_of(h, 1));
    assert_eq!(
        accepted(h),
        [
            EngineCommand::SetPreview { shot: wide.clone() },
            EngineCommand::Cut {
                preview: Some(wide.clone())
            },
            EngineCommand::SetPreview {
                shot: medium.clone()
            },
            EngineCommand::Cut {
                preview: Some(medium)
            },
        ]
    );
}

#[test]
fn space_on_a_travelling_shot_waits_says_so_and_a_second_space_takes_it_at_once() {
    let mut h = stage();
    cut_to_the_travelling_medium(&mut h);
    let medium = shot_of(&h, 1);
    // The cut waits: nothing new on air, and the caption says why and what
    // to do about it.
    assert_eq!(program(&h), Some(shot_of(&h, 0)));
    assert!(says(&h, CUT_WAITS));
    advance(&mut h, 3);
    assert!(says(&h, CUT_WAITS), "still travelling");
    // Space again: on air at once, before any tick.
    press(&mut h, Key::Space);
    assert_eq!(program(&h), Some(medium.clone()));
    assert_eq!(
        accepted(&mut h),
        [EngineCommand::Cut {
            preview: Some(medium.clone())
        }]
    );
    assert!(!says(&h, "waiting for preview"));
    assert_eq!(h.state().console.notice(&h.state().snapshot()), None);
    // The CUT button forces a waiting take too, and so does AUTO, which
    // runs the mix to it at once.
    for button in ["CUT", "AUTO"] {
        let mut h = stage();
        cut_to_the_travelling_medium(&mut h);
        h.get_by_label(button).click();
        h.run();
        let snap = h.state().snapshot();
        assert_eq!(snap.switcher.settle.waiting(), None, "{button}");
        if button == "CUT" {
            assert_eq!(snap.switcher.program, Some(medium.clone()));
        } else {
            assert!(snap.switcher.in_transition);
            assert_eq!(snap.switcher.preview, Some(medium.clone()), "coming in");
        }
    }
}

#[test]
fn a_waiting_cut_left_alone_goes_on_air_within_half_a_second() {
    let mut h = stage();
    cut_to_the_travelling_medium(&mut h);
    // 15 ticks of 1/30 s: the last of them fires it.
    advance(&mut h, 14);
    assert!(says(&h, CUT_WAITS));
    assert_eq!(program(&h), Some(shot_of(&h, 0)));
    advance(&mut h, 1);
    assert_eq!(program(&h), Some(shot_of(&h, 1)));
    assert!(!says(&h, "waiting for preview"));
    assert_eq!(h.state().console.notice(&h.state().snapshot()), None);
    assert_eq!(accepted(&mut h), [], "it fired by itself");
}

#[test]
fn with_settle_before_take_off_space_cuts_a_travelling_shot_at_once() {
    let mut h = stage_with(|_| {});
    press(&mut h, Key::Num2);
    press(&mut h, Key::Space);
    assert_eq!(program(&h), Some(shot_of(&h, 1)));
    assert!(!says(&h, "waiting for preview"));
    assert_eq!(h.state().snapshot().switcher.settle, SettleGate::default());
}

#[test]
fn a_waiting_cut_whose_preview_changes_is_announced_as_a_refused_cut() {
    let mut h = stage_with(|c| {
        c.switcher.settle.enabled = true;
        c.director.mode = DirectorMode::Suggest;
    });
    // The close shot on air at once (a double click is Cut now, never
    // delayed), the medium staged by key and taken: the cut waits.
    let tile = tile_name(2, &h.state().snapshot().shots[2]);
    double_click(&mut h, &tile);
    let (wide, medium, close) = (shot_of(&h, 0), shot_of(&h, 1), shot_of(&h, 2));
    assert_eq!(program(&h), Some(close.clone()));
    press(&mut h, Key::Num2);
    press(&mut h, Key::Space);
    assert!(says(&h, CUT_WAITS));
    // Release ends the operator's hold; the director stages its suggestion,
    // the wide, and the next tick finds preview changed under the cut.
    h.get_by_label("Release").click();
    h.run();
    advance(&mut h, 2);
    let snap = h.state().snapshot();
    assert_eq!(snap.switcher.preview, Some(wide.clone()));
    assert_eq!(snap.switcher.program, Some(close));
    assert_eq!(snap.switcher.settle.dropped(), 1);
    let refused = notice_for(
        "cut",
        &Err(CommandError::PreviewChanged {
            seen: Some(medium),
            preview: Some(wide),
        }),
    )
    .unwrap();
    assert_eq!(h.state().console.notice(&snap), Some(refused.clone()));
    assert_eq!(
        h.get_by_label(&refused.0).accesskit_node().role(),
        Role::Label
    );
    assert!(!says(&h, "waiting for preview"));
}

#[test]
fn the_preview_caption_warns_when_its_subject_is_lost() {
    let mut h = stage();
    h.state_mut().leaves = true;
    press(&mut h, Key::Num2);
    assert!(!says(&h, "lost"), "tracking");
    advance(&mut h, 90);
    let snap = h.state().snapshot();
    assert_eq!(
        snap.shots[1].status,
        ShotStatus::Lost,
        "{:?}",
        snap.shots[1]
    );
    assert!(says(&h, "subject 1 lost"));
    // A Cut is not refused or delayed for it: there is nothing left to
    // settle, and whether to cut to a lost subject is the operator's call.
    press(&mut h, Key::Space);
    assert_eq!(program(&h), Some(shot_of(&h, 1)));
    assert!(!says(&h, "subject 1 lost"), "preview no longer shows it");
}

#[test]
fn the_caption_shows_a_waiting_auto_and_a_lost_subject_from_any_snapshot() {
    let mut snap = nine_tiles();
    let shot = snap.shots[1].id.clone();
    snap.switcher.preview = Some(shot.clone());
    snap.shots[1].status = ShotStatus::Lost;
    let mut gate = SettleGate::new(Some(SettleRule {
        threshold: 0.6,
        max_wait: 0.5,
    }));
    assert!(gate.press(TakeKind::Auto, Some(&shot), Some(0.1), true));
    snap.switcher.settle = gate;
    let mut h = window(snap);
    h.run();
    // A label keeps its text as its value in the accessibility tree.
    let caption = h.get_by_label_contains("AUTO waiting for preview to settle");
    let text = caption.accesskit_node().value().unwrap_or_default();
    assert!(text.starts_with("PREVIEW"), "{text}");
    assert!(text.ends_with("subject 1 lost"), "{text}");
    // Enter again sends an Auto of the preview seen: the engine takes it as
    // the second press.
    press(&mut h, Key::Enter);
    assert_eq!(
        sent(&mut h),
        [EngineCommand::Auto {
            preview: Some(shot)
        }]
    );
}
