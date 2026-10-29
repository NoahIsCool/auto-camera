//! Window tests of the operator's side of the director, on the live studio
//! of [`super::switching`] with the director on: a take refused because
//! the director restaged preview after the operator looked, and one that
//! goes through although a frame drawn before the engine applied the key
//! came in between; the operator's hold on preview in Suggest mode and what
//! the window says about it, Skip, and what a key press does to a director
//! in Auto mode with each takeover setting, the program monitor's AUTO
//! badge included.
//!
//! The studio's shots are the three wides ("a", "b", "c"), which the
//! director ranks in that order. In Auto mode it puts "a" on air with the
//! studio's 3 s mix as soon as it starts, and suggests "b" from then on; in
//! Suggest mode it stages "a" and leaves the takes to the operator. The
//! rules are the engine core's and are tested there; these tests check
//! that the window sends what the operator saw and shows what came of it.

use super::switching::{FPS, MIX_SECS, Studio, accepted, advance, shot_of, studio_with};
use super::*;
use crate::ui::notices::notice_for;
use crate::ui::text::shot_name;
use ac_director::{Handover, Takeover};
use ac_engine::{CommandError, CommandReply};
use std::time::Duration;

/// A studio whose director runs in `mode` with `takeover`, a 4 s pause and
/// a 2 s skip window; in Auto mode, after its establishing mix has put "a"
/// on air.
fn directed(mode: DirectorMode, takeover: Takeover) -> Harness<'static, Studio> {
    let mut h = studio_with(|c| {
        c.director.mode = mode;
        c.director.takeover = takeover;
        c.director.pause_s = 4.0;
        c.director.skip_s = 2.0;
    });
    if mode == DirectorMode::Auto {
        advance(&mut h, MIX_SECS);
        let snap = h.state().snapshot();
        assert_eq!(snap.switcher.program, Some(shot_of(&h, 0)));
        assert_eq!(snap.switcher.preview, Some(shot_of(&h, 1)));
        assert_eq!(snap.director.suggestion, Some(shot_of(&h, 1)));
    }
    h
}

/// Takes the commands sent so far with the core's replies.
fn replies(
    h: &mut Harness<'_, Studio>,
) -> Vec<(EngineCommand, Result<CommandReply, CommandError>)> {
    std::mem::take(&mut h.state_mut().sent)
}

/// True when the program monitor carries the badge whose text is `text`.
fn badge<S>(h: &Harness<'_, S>, text: &str) -> bool {
    h.query_by_label(&format!("program {text}")).is_some()
}

#[test]
fn the_director_restaging_between_a_key_press_and_space_refuses_the_cut() {
    // The director wins in Auto mode, so it stages its own shot over the
    // operator's on every tick.
    let mut h = directed(DirectorMode::Auto, Takeover::Director);
    let (a, b, c) = (shot_of(&h, 0), shot_of(&h, 1), shot_of(&h, 2));
    press(&mut h, Key::Num3);
    assert_eq!(
        replies(&mut h),
        [(
            EngineCommand::SetPreview { shot: c.clone() },
            Ok(CommandReply::Done)
        )]
    );
    assert_eq!(h.state().snapshot().switcher.preview, Some(c.clone()));
    // One tick before Space arrives, with no frame drawn in between: the
    // director restages "b", which the operator never saw on preview.
    h.state_mut().tick_for(1.0 / FPS);
    assert_eq!(h.state().snapshot().switcher.preview, Some(b.clone()));
    press(&mut h, Key::Space);
    let refused = CommandError::PreviewChanged {
        seen: Some(c.clone()),
        preview: Some(b.clone()),
    };
    assert_eq!(
        replies(&mut h),
        [(
            EngineCommand::Cut {
                preview: Some(c.clone())
            },
            Err(refused.clone())
        )]
    );
    h.run();
    let (text, error) = notice_for("cut", &Err(refused)).unwrap();
    assert!(error && text.contains("nothing was taken"), "{text}");
    assert_eq!(h.get_by_label(&text).accesskit_node().role(), Role::Label);
    let snap = h.state().snapshot();
    assert_eq!(
        snap.switcher.program,
        Some(a.clone()),
        "nothing went on air"
    );
    assert!(!snap.switcher.in_transition);
    // The window now shows "b" on preview, so Enter takes what it shows.
    press(&mut h, Key::Enter);
    assert_eq!(
        accepted(&mut h),
        [EngineCommand::Auto {
            preview: Some(b.clone())
        }]
    );
    assert!(h.state().snapshot().switcher.in_transition);
    advance(&mut h, MIX_SECS + 0.5);
    assert_eq!(h.state().snapshot().switcher.program, Some(b));
    // A key and Space in one frame leave the director no tick in between:
    // the cut takes the shot the key staged.
    h.key_press(Key::Num3);
    h.key_press(Key::Space);
    h.run();
    assert_eq!(
        accepted(&mut h),
        [
            EngineCommand::SetPreview { shot: c.clone() },
            EngineCommand::Cut {
                preview: Some(c.clone())
            }
        ]
    );
    assert_eq!(h.state().snapshot().switcher.program, Some(c));
}

/// A key event as the keyboard sends it, for a test that puts each on a
/// frame of its own (the harness's own key presses run a frame for the
/// press and one for the release).
fn key(key: Key, pressed: bool) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn a_frame_drawn_before_the_engine_applies_a_key_still_cuts_the_staged_shot() {
    let mut h = studio_with(|_| {});
    h.state_mut().lagging = true;
    let b = shot_of(&h, 1);
    // Key 2 stages "b"; the engine has not seen it yet.
    h.input_mut().events.push(key(Key::Num2, true));
    h.step();
    assert_eq!(h.state().snapshot().switcher.preview, None);
    // The next frame (the key's release) draws the snapshot from before
    // the staging, with an empty preview; the engine applies the staging
    // after it.
    h.input_mut().events.push(key(Key::Num2, false));
    h.step();
    assert_eq!(h.state().snapshot().switcher.preview, Some(b.clone()));
    // So Space, pressed looking at that frame, takes "b": the operator
    // staged it, and no picture since could have shown it.
    h.input_mut().events.push(key(Key::Space, true));
    h.step();
    h.input_mut().events.push(key(Key::Space, false));
    h.step();
    assert_eq!(
        accepted(&mut h),
        [
            EngineCommand::SetPreview { shot: b.clone() },
            EngineCommand::Cut {
                preview: Some(b.clone())
            }
        ]
    );
    assert_eq!(h.state().snapshot().switcher.program, Some(b));
}

#[test]
fn in_suggest_mode_the_operators_shot_holds_preview_and_the_window_says_so() {
    let mut h = directed(DirectorMode::Suggest, Takeover::Suggest);
    let (a, b, c) = (shot_of(&h, 0), shot_of(&h, 1), shot_of(&h, 2));
    let snap = h.state().snapshot();
    assert_eq!(
        snap.switcher.preview,
        Some(a.clone()),
        "the director staged"
    );
    assert!(h.query_by_label_contains("Preview held").is_none());
    // The operator stages "c": it holds preview, and the panel says so.
    press(&mut h, Key::Num3);
    let _ = accepted(&mut h);
    advance(&mut h, 0.5);
    let snap = h.state().snapshot();
    assert_eq!(snap.director.preview_held, Some(c.clone()));
    let c_name = shot_name(&snap, Some(&c));
    h.get_by_label(&format!("Preview held: {c_name}"));
    // Skipping "a" moves the suggestion on to "b", which is not staged over
    // "c"; the window says what it held back, once.
    h.get_by_label("Skip").click();
    h.run();
    assert_eq!(accepted(&mut h), [EngineCommand::Skip { shot: a.clone() }]);
    advance(&mut h, 1.0);
    let snap = h.state().snapshot();
    assert_eq!(snap.director.suggestion, Some(b.clone()));
    assert_eq!(snap.switcher.preview, Some(c.clone()), "the hold stands");
    assert_eq!(snap.director.held_back, 1);
    let held = format!(
        "director: {} not staged, your {c_name} holds preview until you take or release it",
        shot_name(&snap, Some(&b))
    );
    h.get_by_label(&held);
    advance(&mut h, 3.0);
    assert_eq!(h.state().snapshot().switcher.preview, Some(c.clone()));
    assert_eq!(h.state().snapshot().director.held_back, 1, "said once");
    // Release hands preview back: the director stages its suggestion on the
    // next tick.
    h.get_by_label("Release").click();
    h.run();
    assert_eq!(accepted(&mut h), [EngineCommand::ReleasePreview]);
    assert!(h.query_by_label_contains("Preview held").is_none());
    advance(&mut h, 1.0 / FPS);
    let snap = h.state().snapshot();
    assert_eq!(snap.switcher.preview, snap.director.suggestion);
    assert_ne!(snap.switcher.preview, Some(c));
}

#[test]
fn skip_leaves_the_suggestion_out_for_its_window() {
    let mut h = directed(DirectorMode::Suggest, Takeover::Suggest);
    let a = shot_of(&h, 0);
    h.get_by_label("Skip").click();
    h.run();
    assert_eq!(accepted(&mut h), [EngineCommand::Skip { shot: a.clone() }]);
    assert_eq!(
        h.state().snapshot().director.skipped,
        [(a.clone(), Duration::from_secs(2))]
    );
    // For the 2 s window the panel suggests something else, which the
    // director stages, since preview held its own shot.
    for _ in 0..19 {
        advance(&mut h, 0.1);
        let snap = h.state().snapshot();
        assert_ne!(snap.director.suggestion, Some(a.clone()));
        assert_eq!(snap.switcher.preview, snap.director.suggestion);
    }
    advance(&mut h, 0.2);
    assert!(h.state().snapshot().director.skipped.is_empty());
}

#[test]
fn by_default_a_key_press_drops_auto_to_suggest_and_the_badge_goes() {
    let mut h = directed(DirectorMode::Auto, Takeover::Suggest);
    let (a, c) = (shot_of(&h, 0), shot_of(&h, 2));
    assert!(badge(&h, "AUTO"), "the program monitor says AUTO");
    let takes = h.state().snapshot().director.takes;
    press(&mut h, Key::Num3);
    let reply = Ok(CommandReply::TookOver(Handover::ToSuggest));
    assert_eq!(
        replies(&mut h),
        [(EngineCommand::SetPreview { shot: c.clone() }, reply.clone())]
    );
    h.run();
    let (text, error) = notice_for("set_preview", &reply).unwrap();
    assert!(!error);
    assert_eq!(
        text,
        "the director dropped to Suggest, as you switched by hand"
    );
    h.get_by_label(&text);
    assert!(!badge(&h, "AUTO"), "no badge in Suggest mode");
    // From here the operator switches: the director takes nothing, and the
    // operator's shot holds preview.
    advance(&mut h, 12.0);
    let snap = h.state().snapshot();
    assert_eq!(snap.director.mode, DirectorMode::Suggest);
    assert_eq!(snap.director.takes, takes);
    assert_eq!(snap.switcher.program, Some(a));
    assert_eq!(snap.switcher.preview, Some(c.clone()));
    press(&mut h, Key::Space);
    assert_eq!(
        accepted(&mut h),
        [EngineCommand::Cut {
            preview: Some(c.clone())
        }]
    );
    assert_eq!(h.state().snapshot().switcher.program, Some(c));
}

#[test]
fn with_the_pause_setting_the_badge_counts_the_pause_down() {
    let mut h = directed(DirectorMode::Auto, Takeover::Pause);
    let (b, c) = (shot_of(&h, 1), shot_of(&h, 2));
    press(&mut h, Key::Num3);
    let reply = Ok(CommandReply::TookOver(Handover::Paused(
        Duration::from_secs(4),
    )));
    assert_eq!(
        replies(&mut h),
        [(EngineCommand::SetPreview { shot: c.clone() }, reply.clone())]
    );
    h.run();
    let (text, _) = notice_for("set_preview", &reply).unwrap();
    assert_eq!(
        text,
        "the director's takes are paused for 4 s, as you switched by hand"
    );
    h.get_by_label(&text);
    assert!(badge(&h, "AUTO · paused 4 s"));
    // While paused the director holds off, and the operator's shot holds
    // preview.
    advance(&mut h, 1.5);
    assert!(badge(&h, "AUTO · paused 3 s"));
    assert_eq!(h.state().snapshot().switcher.preview, Some(c.clone()));
    // Once the pause has passed it is the director again, which stages
    // its own shot over the operator's.
    advance(&mut h, 2.6);
    assert!(badge(&h, "AUTO"));
    let snap = h.state().snapshot();
    assert_eq!(snap.director.mode, DirectorMode::Auto);
    assert_eq!(snap.director.paused, None);
    assert_eq!(snap.switcher.preview, Some(b));
}
