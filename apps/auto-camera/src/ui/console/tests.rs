//! Window tests: the whole console, headless, driven like an operator would
//! (keys, clicks, drags) through egui's test harness, with widgets found in
//! the AccessKit tree. No renderer, GPU or display is involved, and the
//! snapshots come from a real engine core ([`crate::ui::fixtures`]), so
//! these run anywhere `cargo test` does. Here each test sets the snapshot
//! the window draws and checks the commands it sends; [`switching`] sends
//! them to a live core instead, to see what goes on air.

mod operator;
mod settle;
mod switching;

use super::*;
use crate::ui::close_guard::KEEP_RUNNING;
use crate::ui::fixtures::{self, nine_tiles, snapshot};
use crate::ui::multiview::tile_name;
use ac_director::DirectorMode;
use ac_engine::OutputState;
use ac_switcher::{SwitchError, TransitionKind};
use eframe::egui::accesskit::Role;
use eframe::egui::{
    Event, Key, Modifiers, PointerButton, ViewportCommand, ViewportEvent, ViewportId, pos2, vec2,
};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

/// The number keys, in tile order.
const DIGITS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

/// The window under test: a console, the snapshot it draws, and what it
/// sent.
struct Window {
    /// The console.
    console: Console,
    /// The snapshot drawn every frame.
    snap: EngineSnapshot,
    /// A termination signal came.
    quitting: bool,
    /// Every command the console returned, in order.
    sent: Vec<EngineCommand>,
}

/// A window the size of the app's default, drawing `snap`.
fn window(snap: EngineSnapshot) -> Harness<'static, Window> {
    let state = Window {
        console: Console::new(&fixtures::config(), &snap),
        snap,
        quitting: false,
        sent: Vec::new(),
    };
    Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .build_ui_state(
            |ui, w: &mut Window| {
                let sent = w.console.frame(ui, &w.snap, Vec::new(), w.quitting);
                w.sent.extend(sent);
            },
            state,
        )
}

/// Takes the commands sent so far.
fn sent(h: &mut Harness<'_, Window>) -> Vec<EngineCommand> {
    std::mem::take(&mut h.state_mut().sent)
}

/// Presses `key` and runs until the window settles.
fn press<S>(h: &mut Harness<'_, S>, key: Key) {
    h.key_press(key);
    h.run();
}

/// Double-clicks the centre of the node labelled `label` the way a mouse
/// does, two clicks 50 ms apart with each press and release on a frame of
/// its own, and runs until the window settles. (The harness's own clicks
/// are a step, 250 ms, apart: too slow for egui to count two as a double
/// click.)
fn double_click<S>(h: &mut Harness<'_, S>, label: &str) {
    let at = h.get_by_label(label).rect().center();
    h.input_mut().events.push(Event::PointerMoved(at));
    h.step();
    let start = h.ctx.input(|i| i.time);
    for (i, pressed) in [true, false, true, false].into_iter().enumerate() {
        h.input_mut().time = Some(start + 0.05 * (i + 1) as f64);
        h.input_mut().events.push(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    // Back to the harness's own clock, which goes on from here.
    h.input_mut().time = None;
    h.run();
}

/// Requests a close on the next frame, as the window's close button does.
fn request_close(h: &mut Harness<'_, Window>) {
    h.input_mut()
        .viewports
        .entry(ViewportId::ROOT)
        .or_default()
        .events
        .push(ViewportEvent::Close);
}

/// The viewport commands of the last frame.
fn viewport_commands(h: &Harness<'_, Window>) -> Vec<ViewportCommand> {
    h.output().viewport_output[&ViewportId::ROOT]
        .commands
        .clone()
}

/// True when the shortcuts are paused, as the status bar says.
fn paused<S>(h: &Harness<'_, S>) -> bool {
    h.query_by_label(keys::PAUSED).is_some()
}

/// [`snapshot`] with the output running.
fn on_air() -> EngineSnapshot {
    let mut snap = snapshot();
    snap.output.state = OutputState::Running;
    snap.output.record_path = Some("show.mp4".into());
    snap
}

#[test]
fn keys_1_to_9_stage_the_matching_tiles() {
    let snap = nine_tiles();
    let ids: Vec<ShotId> = snap.shots.iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids.len(), 9);
    let mut h = window(snap.clone());
    // Every tile is in the accessibility tree under its name.
    for (i, shot) in snap.shots.iter().enumerate() {
        let name = tile_name(i, shot);
        let tile = h.get_by_label(&name);
        assert_eq!(tile.accesskit_node().role(), Role::Button);
    }
    for (key, id) in DIGITS.into_iter().zip(&ids) {
        press(&mut h, key);
        assert_eq!(
            sent(&mut h),
            [EngineCommand::SetPreview { shot: id.clone() }],
            "{key:?}"
        );
    }
    // Unbound keys and system combinations stage nothing.
    press(&mut h, Key::Num0);
    press(&mut h, Key::A);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Num2);
    h.key_press_modifiers(Modifiers::ALT, Key::Num3);
    h.run();
    assert_eq!(sent(&mut h), []);
    // A held key stages once: egui marks the presses after the first as
    // repeats.
    h.key_down(Key::Num4);
    h.key_down(Key::Num4);
    h.key_down(Key::Num4);
    h.key_up(Key::Num4);
    h.run();
    assert_eq!(
        sent(&mut h),
        [EngineCommand::SetPreview {
            shot: ids[3].clone()
        }]
    );
    assert!(!paused(&h));
}

#[test]
fn a_click_on_a_tile_stages_it() {
    let snap = nine_tiles();
    let mut h = window(snap.clone());
    h.get_by_label(&tile_name(4, &snap.shots[4])).click();
    h.run();
    assert_eq!(
        sent(&mut h),
        [EngineCommand::SetPreview {
            shot: snap.shots[4].id.clone()
        }]
    );
}

#[test]
fn a_double_click_on_a_tile_cuts_it_now() {
    let snap = nine_tiles();
    let mut h = window(snap.clone());
    let shot = snap.shots[6].id.clone();
    double_click(&mut h, &tile_name(6, &snap.shots[6]));
    // The first click stages it, as egui reports it as a click too; the
    // second is one Cut now that names the shot, so nothing the engine
    // does between the two can make it cut something else.
    assert_eq!(
        sent(&mut h),
        [
            EngineCommand::SetPreview { shot: shot.clone() },
            EngineCommand::CutTo { shot }
        ]
    );
    // Two single clicks (a step apart) are two stagings, never a cut.
    let other = snap.shots[2].id.clone();
    let name = tile_name(2, &snap.shots[2]);
    h.get_by_label(&name).click();
    h.run();
    h.get_by_label(&name).click();
    h.run();
    let staged = EngineCommand::SetPreview { shot: other };
    assert_eq!(sent(&mut h), [staged.clone(), staged]);
}

/// The status bar's text for a take of `shot` the switcher dropped because
/// the shot was no longer available.
fn dropped_text(shot: &ShotId) -> String {
    format!("queued take: switcher: shot {shot:?} is not available")
}

#[test]
fn a_dropped_take_raises_one_notice_until_it_expires() {
    let mut h = window(snapshot());
    let wide = h.state().snap.shots[0].id.clone();
    let text = dropped_text(&wide);
    assert!(h.query_by_label(&text).is_none());
    // The switcher dropped a queued take: the window says why, as it
    // would for a refused command, without any command of its own.
    h.state_mut().snap.switcher.dropped_takes = 1;
    h.state_mut().snap.switcher.last_dropped = Some(SwitchError::Unavailable(wide));
    h.run();
    assert_eq!(h.get_by_label(&text).accesskit_node().role(), Role::Label);
    assert_eq!(sent(&mut h), []);
    // It stays for NOTICE_FOR (egui's clock: a step is 250 ms), then goes,
    // and the same count does not raise it again.
    let steps = (crate::ui::notices::NOTICE_FOR.as_secs_f64() / 0.25) as usize;
    h.run_steps(steps - 2);
    h.get_by_label(&text);
    h.run_steps(4);
    assert!(h.query_by_label(&text).is_none());
    // The next drop raises it again.
    h.state_mut().snap.switcher.dropped_takes = 2;
    h.run();
    h.get_by_label(&text);
}

#[test]
fn drops_counted_before_the_window_opened_raise_no_notice() {
    let mut snap = snapshot();
    let wide = snap.shots[0].id.clone();
    snap.switcher.dropped_takes = 3;
    snap.switcher.last_dropped = Some(SwitchError::Unavailable(wide.clone()));
    let mut h = window(snap);
    h.run();
    assert!(h.query_by_label(&dropped_text(&wide)).is_none());
}

#[test]
fn space_cuts_and_enter_runs_the_transition() {
    let mut h = window(snapshot());
    // Both carry the preview the window shows: none here.
    press(&mut h, Key::Space);
    assert_eq!(sent(&mut h), [EngineCommand::Cut { preview: None }]);
    press(&mut h, Key::Enter);
    assert_eq!(sent(&mut h), [EngineCommand::Auto { preview: None }]);
    // The shortcut is the press: Shift does not change it, Ctrl takes it.
    h.key_press_modifiers(Modifiers::SHIFT, Key::Space);
    h.run();
    assert_eq!(sent(&mut h), [EngineCommand::Cut { preview: None }]);
    h.key_press_modifiers(Modifiers::CTRL, Key::Enter);
    h.run();
    assert_eq!(sent(&mut h), []);
}

#[test]
fn a_focused_drag_value_pauses_space_and_enter() {
    let mut h = window(snapshot());
    assert!(!paused(&h));
    let kind = h.state().console.switch.transition.kind;
    assert_ne!(kind, TransitionKind::Cut, "the duration field is enabled");
    h.get_by_role(Role::SpinButton).focus();
    h.run();
    assert!(paused(&h), "the status bar says the shortcuts are paused");
    let status = h.get_by_label(keys::PAUSED);
    assert_eq!(status.accesskit_node().role(), Role::Label);
    press(&mut h, Key::Space);
    // Enter ends the edit; it is not also a take.
    press(&mut h, Key::Enter);
    let taken = sent(&mut h);
    assert!(
        !taken
            .iter()
            .any(|c| matches!(c, EngineCommand::Cut { .. } | EngineCommand::Auto { .. })),
        "{taken:?}"
    );
    // The edit is over, so the shortcuts are back.
    assert!(!paused(&h));
    press(&mut h, Key::Space);
    assert_eq!(sent(&mut h), [EngineCommand::Cut { preview: None }]);
}

#[test]
fn a_focused_text_field_pauses_every_shortcut_until_escape() {
    let mut h = window(nine_tiles());
    // The query's name field (the first one) gets the keys, each as a
    // press and as text, the way a keyboard sends them.
    h.get_all_by_role(Role::TextInput)
        .next()
        .expect("a field")
        .focus();
    h.run();
    assert!(paused(&h));
    for (key, text) in [(Key::Num1, "1"), (Key::Space, " "), (Key::Num9, "9")] {
        h.key_press(key);
        h.event(egui::Event::Text(text.into()));
        h.run();
    }
    assert_eq!(h.state().console.query_form.name, "1 9");
    // Enter ends the edit, and is not also a take.
    press(&mut h, Key::Enter);
    assert_eq!(sent(&mut h), []);
    assert!(!paused(&h));
    // Back in the field, Escape gives the keyboard back.
    h.get_all_by_role(Role::TextInput).next().unwrap().focus();
    h.run();
    assert!(paused(&h));
    press(&mut h, Key::Escape);
    assert!(!paused(&h));
    assert!(
        !h.get_all_by_role(Role::TextInput)
            .next()
            .unwrap()
            .is_focused()
    );
    press(&mut h, Key::Num2);
    let second = h.state().snap.shots[1].id.clone();
    assert_eq!(sent(&mut h), [EngineCommand::SetPreview { shot: second }]);
}

#[test]
fn escape_drops_the_focus_of_any_widget() {
    let mut h = window(snapshot());
    // Tab focuses a widget that does not edit text, a button: Space then
    // presses that button, not the switcher's Cut.
    press(&mut h, Key::Tab);
    assert!(h.ctx.memory(|m| m.focused()).is_some());
    assert!(paused(&h));
    press(&mut h, Key::Space);
    let taken = sent(&mut h);
    assert!(
        !taken.iter().any(|c| matches!(c, EngineCommand::Cut { .. })),
        "{taken:?}"
    );
    press(&mut h, Key::Escape);
    assert!(h.ctx.memory(|m| m.focused()).is_none());
    assert!(!paused(&h));
    press(&mut h, Key::Space);
    assert_eq!(sent(&mut h), [EngineCommand::Cut { preview: None }]);
    // egui alone keeps the focus on Escape with a modifier; the console
    // drops it all the same.
    h.get_all_by_role(Role::TextInput).next().unwrap().focus();
    h.run();
    assert!(paused(&h));
    h.key_press_modifiers(Modifiers::SHIFT, Key::Escape);
    h.run();
    assert!(!paused(&h));
}

#[test]
fn cut_and_auto_buttons_take_when_a_shot_is_staged() {
    // Nothing staged: the buttons are disabled.
    let mut h = window(snapshot());
    assert!(h.get_by_label("CUT").accesskit_node().is_disabled());
    h.get_by_label("CUT").click();
    h.run();
    assert_eq!(sent(&mut h), []);
    let mut snap = snapshot();
    let staged = Some(snap.shots[0].id.clone());
    snap.switcher.preview = staged.clone();
    let mut h = window(snap);
    h.get_by_label("CUT").click();
    h.run();
    assert_eq!(
        sent(&mut h),
        [EngineCommand::Cut {
            preview: staged.clone()
        }]
    );
    h.get_by_label("AUTO").click();
    h.run();
    assert_eq!(sent(&mut h), [EngineCommand::Auto { preview: staged }]);
    // Clicking leaves no widget holding the keyboard.
    assert!(!paused(&h));
}

#[test]
fn a_staged_shot_stays_seen_until_a_snapshot_shows_what_came_of_it() {
    let snap = nine_tiles();
    assert_eq!(snap.switcher.preview, None);
    let tile = |i: usize| Some(snap.shots[i].id.clone());
    let shown = |h: &Harness<'_, Window>| h.state().console.shown.clone();
    let mut h = window(snap.clone());
    // The snapshot never shows the shot key 2 staged: every frame draws one
    // from before the staging, so the shot stays the preview seen.
    press(&mut h, Key::Num2);
    let _ = sent(&mut h);
    h.run_steps(3);
    assert_eq!(shown(&h), tile(1));
    // The engine has answered, but the snapshots drawn are not that late.
    let tick = snap.tick;
    h.state_mut().console.answered(tick + 1);
    h.run_steps(3);
    assert_eq!(shown(&h), tile(1));
    // One that late shows what came of the staging: an empty preview here
    // (the engine refused it, say), and that is what Space now takes.
    h.state_mut().snap.tick = tick + 1;
    h.step();
    assert_eq!(shown(&h), None);
    press(&mut h, Key::Space);
    assert_eq!(sent(&mut h), [EngineCommand::Cut { preview: None }]);
    // An answer from before a staging does not count for it...
    h.state_mut().console.answered(tick + 2);
    press(&mut h, Key::Num3);
    let _ = sent(&mut h);
    h.state_mut().snap.tick = tick + 5;
    h.run_steps(2);
    assert_eq!(shown(&h), tile(2));
    // ...and a snapshot showing the shot on preview ends it at once: the
    // window follows the snapshots from there.
    h.state_mut().snap.switcher.preview = tile(2);
    h.step();
    h.state_mut().snap.switcher.preview = tile(3);
    h.step();
    assert_eq!(shown(&h), tile(3));
    press(&mut h, Key::Enter);
    assert_eq!(sent(&mut h), [EngineCommand::Auto { preview: tile(3) }]);
}

#[test]
fn accept_and_skip_name_the_suggestion_the_window_showed() {
    let mut snap = nine_tiles();
    let (first, second) = (snap.shots[1].id.clone(), snap.shots[2].id.clone());
    snap.director.mode = DirectorMode::Suggest;
    snap.director.suggestion = Some(first.clone());
    let mut h = window(snap);
    h.get_by_label("Accept").click();
    h.run();
    assert_eq!(
        sent(&mut h),
        [EngineCommand::AcceptSuggestion {
            shot: first.clone()
        }]
    );
    // The suggestion changes on the frame that handles a click (the
    // button's release), which was made looking at the frame before: the
    // click names the shot that frame showed (an Accept of it the engine
    // refuses).
    let at = h.get_by_label("Skip").rect().center();
    h.input_mut().events.push(Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        if !pressed {
            h.state_mut().snap.director.suggestion = Some(second.clone());
        }
        h.input_mut().events.push(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    assert_eq!(sent(&mut h), [EngineCommand::Skip { shot: first }]);
    h.run();
    h.get_by_label("Accept").click();
    h.run();
    assert_eq!(
        sent(&mut h),
        [EngineCommand::AcceptSuggestion { shot: second }]
    );
}

/// The T-bar positions in `commands`.
fn tbar_positions(commands: &[EngineCommand]) -> Vec<f64> {
    commands
        .iter()
        .filter_map(|c| match c {
            EngineCommand::SetTBar { position } => Some(*position),
            _ => None,
        })
        .collect()
}

#[test]
fn dragging_the_tbar_sends_its_positions() {
    let mut h = window(snapshot());
    let rail = h.get_by_role(Role::Slider).rect();
    assert!(rail.height() > rail.width(), "vertical: {rail:?}");
    // From the rest end (bottom) halfway up.
    h.drag_at(pos2(rail.center().x, rail.bottom() - 1.0));
    h.hover_at(pos2(rail.center().x, rail.center().y + 10.0));
    h.hover_at(rail.center());
    h.drop_at(rail.center());
    h.run();
    let positions = tbar_positions(&sent(&mut h));
    let last = *positions.last().expect("positions were sent");
    assert!((last - 0.5).abs() < 0.1, "{positions:?}");
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{positions:?}");
    assert_eq!(h.state().console.switch.tbar, last, "the lever stays put");
    // On to the far end, which completes the transition.
    h.drag_at(rail.center());
    h.hover_at(pos2(rail.center().x, rail.top()));
    h.drop_at(pos2(rail.center().x, rail.top()));
    h.run();
    assert_eq!(tbar_positions(&sent(&mut h)).last(), Some(&1.0));
}

#[test]
fn director_mode_buttons_switch_the_mode() {
    let snap = snapshot();
    assert_eq!(snap.director.mode, DirectorMode::Suggest);
    let mut h = window(snap);
    // The snapshot keeps saying Suggest: the console asks, the engine
    // decides, so only the other modes are asked for.
    for (label, mode) in [
        ("Off", Some(DirectorMode::Off)),
        ("Suggest", None),
        ("Auto", Some(DirectorMode::Auto)),
    ] {
        h.get_by_role_and_label(Role::Button, label).click();
        h.run();
        let expected: Vec<EngineCommand> = mode
            .map(|mode| EngineCommand::SetDirectorMode { mode })
            .into_iter()
            .collect();
        assert_eq!(sent(&mut h), expected, "{label}");
    }
}

#[test]
fn a_close_request_during_output_asks_first() {
    let mut h = window(on_air());
    request_close(&mut h);
    h.step();
    assert!(viewport_commands(&h).contains(&ViewportCommand::CancelClose));
    h.run();
    h.get_by_label(Question::Close.title());
    // While asking, Enter answers nothing and takes nothing.
    press(&mut h, Key::Enter);
    press(&mut h, Key::Space);
    assert_eq!(sent(&mut h), []);
    h.get_by_label(Question::Close.title());
    // Keep running: the dialog goes, the window stays.
    h.get_by_label(KEEP_RUNNING).click();
    h.step();
    assert!(!viewport_commands(&h).contains(&ViewportCommand::Close));
    h.run();
    assert!(h.query_by_label(Question::Close.title()).is_none());
    // Escape cancels too.
    request_close(&mut h);
    h.step();
    h.run();
    press(&mut h, Key::Escape);
    assert!(h.query_by_label(Question::Close.title()).is_none());
    // Confirmed, the window closes, and its own close request goes through.
    request_close(&mut h);
    h.step();
    h.run();
    h.get_by_label(Question::Close.confirm()).click();
    h.step();
    assert!(viewport_commands(&h).contains(&ViewportCommand::Close));
    request_close(&mut h);
    h.step();
    assert!(!viewport_commands(&h).contains(&ViewportCommand::CancelClose));
    assert_eq!(sent(&mut h), [], "closing sends no command itself");
}

#[test]
fn the_close_question_goes_when_the_output_ends_first() {
    let mut failed = on_air();
    failed.output.state = OutputState::Failed("rtmp gone".into());
    let mut finished = on_air();
    finished.output.state = OutputState::Finished;
    let mut dead = on_air();
    dead.running = false;
    for (end, ended) in [("failed", failed), ("finished", finished), ("dead", dead)] {
        let mut h = window(on_air());
        request_close(&mut h);
        h.step();
        h.run();
        h.get_by_label(Question::Close.title());
        // Nothing is live before the operator answers: the dialog would
        // say something untrue, so it goes, and the window stays open.
        h.state_mut().snap = ended;
        h.step();
        assert!(
            !viewport_commands(&h).contains(&ViewportCommand::Close),
            "{end}"
        );
        h.run();
        assert!(h.query_by_label(Question::Close.title()).is_none(), "{end}");
        assert!(!paused(&h), "{end}: the shortcuts are back");
        assert_eq!(sent(&mut h), [], "{end}");
        // Closing again acts at once.
        request_close(&mut h);
        h.step();
        assert!(
            !viewport_commands(&h).contains(&ViewportCommand::CancelClose),
            "{end}"
        );
    }
}

#[test]
fn without_live_output_or_after_a_signal_the_window_closes_at_once() {
    let mut h = window(snapshot());
    request_close(&mut h);
    h.step();
    assert!(!viewport_commands(&h).contains(&ViewportCommand::CancelClose));
    h.run();
    assert!(h.query_by_label(Question::Close.title()).is_none());
    // A termination signal closes even a live show without asking.
    let mut h = window(on_air());
    h.state_mut().quitting = true;
    request_close(&mut h);
    h.step();
    assert!(!viewport_commands(&h).contains(&ViewportCommand::CancelClose));
    h.run();
    assert!(h.query_by_label(Question::Close.title()).is_none());
}

#[test]
fn stop_asks_before_stopping_a_live_output() {
    let mut h = window(on_air());
    h.get_by_label("Stop").click();
    h.run();
    h.get_by_label(Question::Stop.title());
    assert_eq!(sent(&mut h), []);
    h.get_by_label(KEEP_RUNNING).click();
    h.run();
    assert!(h.query_by_label(Question::Stop.title()).is_none());
    assert_eq!(sent(&mut h), []);
    h.get_by_label("Stop").click();
    h.run();
    h.get_by_label(Question::Stop.confirm()).click();
    h.run();
    assert_eq!(sent(&mut h), [EngineCommand::StopOutput]);
    assert!(h.query_by_label(Question::Stop.title()).is_none());
    // Asked again, but the output ends on its own: the question goes.
    h.get_by_label("Stop").click();
    h.run();
    h.state_mut().snap.output.state = OutputState::Finished;
    h.run();
    assert!(h.query_by_label(Question::Stop.title()).is_none());
    assert_eq!(sent(&mut h), []);
}
