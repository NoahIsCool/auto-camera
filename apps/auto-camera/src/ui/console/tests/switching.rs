//! Window tests on a live engine core: the switching rules that came with
//! the queued take, as the operator meets them through the window.
//!
//! A [`Studio`] hands every command the console returns straight to a real
//! [`EngineCore`], as the engine thread does between ticks, and turns its
//! reply into the notice the app would raise ([`notice_for`]), handed back
//! on the next frame; every frame draws the core's newest snapshot. Engine
//! time moves only when a test says so ([`advance`]), and frames only draw
//! and apply commands, so a test decides exactly where in a transition each
//! press lands. The studio has three synthetic sources ("a", "b", "c") and
//! no queries, so its shots are the three wides, which need no detector; a
//! stalled source repeats its last frame, and its wide goes unavailable
//! [`SOURCE_STALE_AFTER`] later. The director is off, so only the
//! operator stages shots; [`studio_with`] changes the config, and the
//! tests of the operator's side of the director (`operator.rs`) turn it
//! on.
//!
//! The rules themselves (queued takes, Cut cutting a transition short, Cut
//! now, the T-bar's hold) are the switcher's and are tested there and in
//! the engine core; these tests check that the window's keys, buttons and
//! tiles reach them and that the window shows what they did.

use super::*;
use crate::ui::notices::notice_for;
use ac_core::SourceId;
use ac_director::DirectorMode;
use ac_engine::core::SOURCE_STALE_AFTER;
use ac_engine::{CommandError, CommandReply, EngineCore, OutputView, SourceFeed, TimingView};
use ac_media::{SourceInfo, SourceState};
use ac_sources::{SourceConfig, SyntheticPattern};
use ac_switcher::Queued;
use std::time::Duration;

/// Frame and tick rate of the studio.
pub(super) const FPS: f64 = 30.0;
/// Length of the studio's mix, long enough to press keys during it.
pub(super) const MIX_SECS: f64 = 3.0;

/// One synthetic source of the studio.
struct Feed {
    /// Its id.
    id: SourceId,
    /// What it shows.
    pattern: SyntheticPattern,
    /// Sequence number of its newest frame.
    seq: u64,
    /// When true, it repeats its newest frame (a stall).
    stalled: bool,
}

/// The window on a live engine core; see the module docs.
pub(super) struct Studio {
    /// The console.
    pub(super) console: Console,
    /// The engine core the console's commands go to.
    pub(super) core: EngineCore,
    /// The sources, in the core's order.
    feeds: Vec<Feed>,
    /// Notices of the replies since the last frame, handed to the console
    /// with the next.
    raised: Vec<(String, bool)>,
    /// Every command the console returned, with the core's reply.
    pub(super) sent: Vec<(EngineCommand, Result<CommandReply, CommandError>)>,
}

impl Studio {
    /// The core's snapshot, without output or timing.
    pub(super) fn snapshot(&self) -> EngineSnapshot {
        self.core
            .snapshot(OutputView::default(), TimingView::default(), true)
    }

    /// Ticks the core for `secs` seconds of frames.
    pub(super) fn tick_for(&mut self, secs: f64) {
        for _ in 0..(secs * FPS).round() as usize {
            let now = Duration::from_secs_f64(self.core.ticks() as f64 / FPS);
            let feeds: Vec<SourceFeed> = self
                .feeds
                .iter()
                .map(|f| SourceFeed {
                    state: SourceState::Running,
                    frame: Some(f.pattern.frame(&f.id, f.seq)),
                    detector_loading: false,
                })
                .collect();
            let _ = self.core.tick(now, &feeds, Vec::new());
            for f in self.feeds.iter_mut().filter(|f| !f.stalled) {
                f.seq += 1;
            }
        }
    }
}

/// A window on a fresh three-source studio whose sources have delivered
/// for a second, so all three wides are available; nothing is on air.
fn studio() -> Harness<'static, Studio> {
    studio_with(|_| {})
}

/// [`studio`] with its config changed by `configure` first; with the
/// director on, it has run for that first second too.
pub(super) fn studio_with(configure: impl FnOnce(&mut StudioConfig)) -> Harness<'static, Studio> {
    let source = || SourceConfig::Synthetic {
        width: 64,
        height: 36,
        fps: FPS,
        subjects: 0,
    };
    let mut config = StudioConfig::with_source("a", source());
    for id in ["b", "c"] {
        let mut entry = config.sources[0].clone();
        entry.id = id.into();
        config.sources.push(entry);
    }
    config.switcher.transition = TransitionKind::Mix;
    config.switcher.duration = MIX_SECS;
    config.director.mode = DirectorMode::Off;
    config.output.width = 64;
    config.output.height = 36;
    configure(&mut config);
    config.validate().expect("a valid studio");
    let feeds: Vec<Feed> = config
        .sources
        .iter()
        .map(|s| Feed {
            id: s.source_id(),
            pattern: SyntheticPattern::new(64, 36, FPS, 0).expect("pattern"),
            seq: 0,
            stalled: false,
        })
        .collect();
    let infos = feeds
        .iter()
        .map(|f| SourceInfo {
            id: f.id.clone(),
            width: 64,
            height: 36,
            fps: FPS,
            audio: None,
        })
        .collect();
    let core = EngineCore::new(&config, infos, &[false; 3]).expect("core");
    let first = core.snapshot(OutputView::default(), TimingView::default(), true);
    let mut studio = Studio {
        console: Console::new(&config, &first),
        core,
        feeds,
        raised: Vec::new(),
        sent: Vec::new(),
    };
    studio.tick_for(1.0);
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .build_ui_state(
            |ui, s: &mut Studio| {
                let snap = s.snapshot();
                let raised = std::mem::take(&mut s.raised);
                for command in s.console.frame(ui, &snap, raised, false) {
                    let reply = s.core.apply(&command);
                    s.raised.extend(notice_for(command.name(), &reply));
                    s.sent.push((command, reply));
                }
            },
            studio,
        );
    h.run();
    let snap = h.state().snapshot();
    assert_eq!(snap.shots.len(), 3);
    assert!(snap.shots.iter().all(|s| s.available), "{:?}", snap.shots);
    h
}

/// Ticks the engine for `secs` seconds, then draws until the window
/// settles.
pub(super) fn advance(h: &mut Harness<'_, Studio>, secs: f64) {
    h.state_mut().tick_for(secs);
    h.run();
}

/// The id of the shot of tile `index` (0-based).
pub(super) fn shot_of(h: &Harness<'_, Studio>, index: usize) -> ShotId {
    h.state().snapshot().shots[index].id.clone()
}

/// The accessible name of tile `index` (0-based) in the core's snapshot,
/// which the window's tree has once a frame has drawn it.
fn tile_of(h: &Harness<'_, Studio>, index: usize) -> String {
    tile_name(index, &h.state().snapshot().shots[index])
}

/// Takes the commands sent so far, failing on any the core refused.
pub(super) fn accepted(h: &mut Harness<'_, Studio>) -> Vec<EngineCommand> {
    std::mem::take(&mut h.state_mut().sent)
        .into_iter()
        .map(|(command, reply)| {
            assert!(reply.is_ok(), "{command:?} was refused: {reply:?}");
            command
        })
        .collect()
}

/// Puts tile 1 ("a") on air with keys 1 and Space, then stages tile 2
/// ("b") with key 2 and starts the mix to it with Enter, and lets it run
/// for half a second.
fn mixing_to_b(h: &mut Harness<'_, Studio>) {
    press(h, Key::Num1);
    press(h, Key::Space);
    press(h, Key::Num2);
    press(h, Key::Enter);
    assert_eq!(
        accepted(h),
        [
            EngineCommand::SetPreview {
                shot: shot_of(h, 0)
            },
            EngineCommand::Cut {
                preview: Some(shot_of(h, 0))
            },
            EngineCommand::SetPreview {
                shot: shot_of(h, 1)
            },
            EngineCommand::Auto {
                preview: Some(shot_of(h, 1))
            },
        ]
    );
    advance(h, 0.5);
    let snap = h.state().snapshot();
    assert_eq!(snap.switcher.program, Some(shot_of(h, 0)));
    assert_eq!(snap.switcher.preview, Some(shot_of(h, 1)));
    assert!(snap.switcher.in_transition);
}

#[test]
fn a_take_pressed_during_a_mix_is_queued_and_its_drop_is_shown() {
    let mut h = studio();
    mixing_to_b(&mut h);
    let c = shot_of(&h, 2);
    // AUTO stays live during the mix: staging "c" and pressing it queues
    // a take of "c" for the end of the mix; nothing is refused. Preview
    // still shows "b", which the mix brings in, so that is what AUTO says
    // the operator saw.
    press(&mut h, Key::Num3);
    h.get_by_label("AUTO").click();
    h.run();
    assert_eq!(
        accepted(&mut h),
        [
            EngineCommand::SetPreview { shot: c.clone() },
            EngineCommand::Auto {
                preview: Some(shot_of(&h, 1))
            }
        ]
    );
    let snap = h.state().snapshot();
    assert_eq!(
        snap.switcher.queued,
        Some(Queued {
            shot: c.clone(),
            take: true
        })
    );
    assert_eq!(snap.switcher.preview, Some(shot_of(&h, 1)), "b comes in");
    h.get_by_label_contains("take queued");
    assert_eq!(h.state().console.notice(&snap), None, "no notice yet");
    // "c" stalls, so by the end of the mix its wide is unavailable: the
    // take is dropped, and the window says why.
    h.state_mut().feeds[2].stalled = true;
    assert!(SOURCE_STALE_AFTER.as_secs_f64() < MIX_SECS - 0.5);
    advance(&mut h, MIX_SECS);
    let snap = h.state().snapshot();
    assert!(!snap.switcher.in_transition, "nothing more is taken");
    assert_eq!(snap.switcher.program, Some(shot_of(&h, 1)));
    assert_eq!(snap.switcher.dropped_takes, 1);
    let text = dropped_text(&c);
    assert_eq!(h.get_by_label(&text).accesskit_node().role(), Role::Label);
    assert!(h.query_by_label_contains("take queued").is_none());
    assert_eq!(accepted(&mut h), [], "the drop is no command of the window");
}

#[test]
fn keys_during_a_mix_stage_the_next_shot_queue_a_take_and_cut_it_short() {
    let mut h = studio();
    mixing_to_b(&mut h);
    let (a, b, c) = (shot_of(&h, 0), shot_of(&h, 1), shot_of(&h, 2));
    // 3 stages "c" for the end of the mix; Enter queues a take of it.
    press(&mut h, Key::Num3);
    let snap = h.state().snapshot();
    assert_eq!(
        snap.switcher.queued,
        Some(Queued {
            shot: c.clone(),
            take: false
        })
    );
    h.get_by_label_contains(&format!("next {}", snap.shots[2].name));
    assert!(h.query_by_label_contains("take queued").is_none());
    press(&mut h, Key::Enter);
    h.get_by_label_contains("take queued");
    // CUT stays live too, and Space cuts the mix short: "b" is on air at
    // once, "c" lands on preview and its queued take is cancelled.
    assert!(!h.get_by_label("CUT").accesskit_node().is_disabled());
    press(&mut h, Key::Space);
    let snap = h.state().snapshot();
    assert_eq!(snap.switcher.program, Some(b.clone()));
    assert_eq!(snap.switcher.preview, Some(c.clone()));
    assert!(!snap.switcher.in_transition);
    assert_eq!(snap.switcher.queued, None);
    assert!(h.query_by_label_contains("take queued").is_none());
    advance(&mut h, MIX_SECS + 1.0);
    let snap = h.state().snapshot();
    assert_eq!(
        snap.switcher.program,
        Some(b.clone()),
        "the take never fires"
    );
    assert_eq!(
        accepted(&mut h),
        [
            EngineCommand::SetPreview { shot: c.clone() },
            EngineCommand::Auto {
                preview: Some(b.clone())
            },
            EngineCommand::Cut {
                preview: Some(b.clone())
            }
        ]
    );
    // With no transition running, the keys do what the README table says:
    // 1 stages tile 1, Space cuts to it, Enter mixes back.
    press(&mut h, Key::Num1);
    press(&mut h, Key::Space);
    assert_eq!(h.state().snapshot().switcher.program, Some(a.clone()));
    press(&mut h, Key::Enter);
    advance(&mut h, MIX_SECS + 0.5);
    let snap = h.state().snapshot();
    assert_eq!(snap.switcher.program, Some(b.clone()));
    assert_eq!(snap.switcher.preview, Some(a.clone()));
    assert_eq!(
        accepted(&mut h),
        [
            EngineCommand::SetPreview { shot: a.clone() },
            EngineCommand::Cut { preview: Some(a) },
            EngineCommand::Auto { preview: Some(b) }
        ]
    );
}

#[test]
fn a_double_click_during_a_mix_puts_the_tile_on_air_at_once() {
    let mut h = studio();
    mixing_to_b(&mut h);
    let c = shot_of(&h, 2);
    let tile = tile_of(&h, 2);
    double_click(&mut h, &tile);
    // One Cut now, after the first click's staging: "c" is on air in the
    // same step that ends the mix, and "b" waits on preview.
    assert_eq!(
        accepted(&mut h),
        [
            EngineCommand::SetPreview { shot: c.clone() },
            EngineCommand::CutTo { shot: c.clone() }
        ]
    );
    let snap = h.state().snapshot();
    assert_eq!(snap.switcher.program, Some(c.clone()));
    assert_eq!(snap.switcher.preview, Some(shot_of(&h, 1)));
    assert!(!snap.switcher.in_transition);
    assert_eq!(snap.switcher.queued, None);
    let tile = tile_of(&h, 2);
    assert!(tile.contains("on program"), "{tile}");
    h.get_by_label(&tile);
    // Without a transition, a double click cuts at once as well.
    let tile = tile_of(&h, 0);
    double_click(&mut h, &tile);
    let a = shot_of(&h, 0);
    assert_eq!(
        accepted(&mut h),
        [
            EngineCommand::SetPreview { shot: a.clone() },
            EngineCommand::CutTo { shot: a.clone() }
        ]
    );
    let snap = h.state().snapshot();
    assert_eq!(snap.switcher.program, Some(a));
    assert_eq!(snap.switcher.preview, Some(c));
}

#[test]
fn while_the_tbar_holds_a_mix_cut_is_disabled_and_space_and_double_clicks_are_refused() {
    let mut h = studio();
    press(&mut h, Key::Num1);
    press(&mut h, Key::Space);
    press(&mut h, Key::Num2);
    // Halfway up the T-bar: a mix the lever holds.
    let rail = h.get_by_role(Role::Slider).rect();
    h.drag_at(pos2(rail.center().x, rail.bottom() - 1.0));
    h.hover_at(pos2(rail.center().x, rail.center().y + 10.0));
    h.hover_at(rail.center());
    h.drop_at(rail.center());
    h.run();
    let _ = accepted(&mut h);
    let snap = h.state().snapshot();
    assert!(snap.switcher.tbar_held, "{:?}", snap.switcher);
    assert!(!paused(&h), "the lever does not keep the keyboard");
    let cut = h.get_by_label("CUT");
    assert!(cut.accesskit_node().is_disabled());
    // The key still reaches the switcher, which refuses it; the window
    // says why on the next frame.
    press(&mut h, Key::Space);
    let sent = std::mem::take(&mut h.state_mut().sent);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].0,
        EngineCommand::Cut {
            preview: Some(shot_of(&h, 1))
        }
    );
    assert_eq!(
        sent[0].1,
        Err(CommandError::Switch(SwitchError::TBarHeld)),
        "{sent:?}"
    );
    h.run();
    h.get_by_label("cut: switcher: the T-bar holds the transition in progress");
    let snap = h.state().snapshot();
    assert!(snap.switcher.tbar_held);
    assert_eq!(snap.switcher.program, Some(shot_of(&h, 0)));
    // A double click on tile 3 is refused the same way: its first click
    // stages "c" for the end of the mix, and its Cut now is refused, so
    // "c" does not go on air and the window says why.
    let (a, b, c) = (shot_of(&h, 0), shot_of(&h, 1), shot_of(&h, 2));
    let tile = tile_of(&h, 2);
    double_click(&mut h, &tile);
    let sent = std::mem::take(&mut h.state_mut().sent);
    assert_eq!(
        sent,
        [
            (
                EngineCommand::SetPreview { shot: c.clone() },
                Ok(CommandReply::Done)
            ),
            (
                EngineCommand::CutTo { shot: c.clone() },
                Err(CommandError::Switch(SwitchError::TBarHeld))
            ),
        ]
    );
    h.run();
    h.get_by_label("cut_to: switcher: the T-bar holds the transition in progress");
    let snap = h.state().snapshot();
    assert!(snap.switcher.tbar_held);
    assert!(snap.switcher.in_transition);
    assert_eq!(snap.switcher.program, Some(a.clone()));
    assert_eq!(snap.switcher.preview, Some(b.clone()));
    assert_eq!(
        snap.switcher.queued,
        Some(Queued {
            shot: c.clone(),
            take: false
        })
    );
    h.get_by_label_contains(&format!("next {}", snap.shots[2].name));
    // The lever finishes the mix: "b" is on air and "c" lands on preview.
    h.drag_at(rail.center());
    h.hover_at(pos2(rail.center().x, rail.center().y - 10.0));
    h.hover_at(pos2(rail.center().x, rail.top() + 1.0));
    h.drop_at(pos2(rail.center().x, rail.top() + 1.0));
    h.run();
    let moves = accepted(&mut h);
    assert!(
        moves
            .iter()
            .all(|m| matches!(m, EngineCommand::SetTBar { .. })),
        "{moves:?}"
    );
    let snap = h.state().snapshot();
    assert!(!snap.switcher.in_transition, "{:?}", snap.switcher);
    assert!(!snap.switcher.tbar_held);
    assert_eq!(snap.switcher.program, Some(b));
    assert_eq!(snap.switcher.preview, Some(c));
    assert_eq!(snap.switcher.queued, None);
}
