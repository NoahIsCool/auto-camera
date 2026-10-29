//! Settle-before-take through the core, timed by the ticks' `dt`: with the
//! rule on, an operator's take of a shot still travelling to its subject
//! (an accepted suggestion's included) fires on the first tick preview is
//! settled or once 0.5 s have passed, whichever comes first; a second press
//! forces it at once, and one that comes just after it fired by itself
//! takes nothing; staging forces it first, Cut now and a T-bar move that
//! starts a transition supersede it (one that does nothing does not), and
//! it is dropped with the fenced take's notice when preview changes during
//! the wait, or with the switcher's reason when its source drops out. With
//! the rule off every take is immediate, and the director's own takes are
//! never delayed.
//!
//! The rig's person stands still until frame [`JUMP`], when the detector
//! sees them bigger and to the left: the medium and close shots travel to
//! the new box for about a second, their settledness rising from about a
//! quarter, while the wide stays settled.

use ac_core::Extent;
use ac_director::Takeover;

use super::*;
use crate::config::SettleSettings;
use crate::settle::{SettleRule, TakeKind, WaitingTake};

/// The frame from which the detector sees the person's new box.
const JUMP: u64 = 90;

/// The medium shot of the rig's person.
fn medium() -> ShotId {
    shot("cam/1/medium")
}

/// The close shot of the rig's person.
fn close() -> ShotId {
    shot("cam/1/close")
}

/// The spec's rule (0.5 s), settled at 0.6, as `[switcher.settle]` gives it.
fn on() -> SettleSettings {
    SettleSettings {
        enabled: true,
        ..SettleSettings::default()
    }
}

/// A rig on an empty stage whose detector (five passes a second) sees one
/// object-framed person, who jumps at [`JUMP`]; `settle` is the
/// `[switcher.settle]` table. It has run until the shots have started
/// travelling, with the wide on program.
fn jumping(settle: SettleSettings) -> Rig {
    let mut c = config(0);
    c.discovery.rate_hz = 5.0;
    c.switcher.settle = settle;
    let seen = |bbox: Rect| {
        vec![Detection {
            query: QueryId("person".into()),
            bbox,
            score: 0.9,
            extent: Extent::Object,
        }]
    };
    let detector = ScriptedDetector::from_table(
        "jumping",
        [
            (0, seen(Rect::new(0.45, 0.3, 0.1, 0.5))),
            (JUMP, seen(Rect::new(0.4, 0.25, 0.2, 0.6))),
        ],
    );
    let mut rig = Rig::new(c).with_detector(detector);
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    rig.run(JUMP as usize - 31 + 2);
    let settled = rig.snapshot().shot(&medium()).unwrap().settled;
    assert!(settled < 0.4, "the medium shot travels: {settled}");
    rig
}

/// Settledness of `id` in the newest snapshot.
fn settled(rig: &Rig, id: &ShotId) -> f64 {
    rig.snapshot().shot(id).unwrap().settled
}

/// The take waiting in the newest snapshot.
fn waiting(rig: &Rig) -> Option<WaitingTake> {
    rig.snapshot().switcher.settle.waiting().cloned()
}

/// Stages `id` as the operator does.
fn stage(rig: &mut Rig, id: &ShotId) {
    rig.apply(EngineCommand::SetPreview { shot: id.clone() })
        .unwrap();
}

/// Ticks until `id` is on program, at most `limit` ticks; the number of
/// ticks it took, checking before each that the take still waits, that
/// nothing went on air and that the core's snapshot holds it.
fn ticks_until_on_air(rig: &mut Rig, id: &ShotId, limit: usize) -> Option<usize> {
    for n in 1..=limit {
        assert!(waiting(rig).is_some(), "tick {n}");
        assert_eq!(rig.core.switcher().program(), Some(&shot("cam/wide")));
        rig.step();
        if rig.core.switcher().program() == Some(id) {
            return Some(n);
        }
    }
    None
}

#[test]
fn a_take_of_a_travelling_shot_fires_at_the_max_wait() {
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    assert_eq!(rig.cut(), Ok(CommandReply::Done));
    // Nothing went on air yet: the cut waits, carrying the shot seen.
    assert_eq!(rig.core.switcher().program(), Some(&shot("cam/wide")));
    assert_eq!(
        waiting(&rig),
        Some(WaitingTake::new(TakeKind::Cut, medium()))
    );
    let rule = rig.snapshot().switcher.settle.rule().copied();
    assert_eq!(
        rule,
        Some(SettleRule {
            threshold: 0.6,
            max_wait: 0.5
        })
    );
    // 15 ticks of 1/30 s are the 0.5 s; the shot is still far off by then.
    let n = ticks_until_on_air(&mut rig, &medium(), 20);
    assert_eq!(n, Some(15));
    assert!(
        settled(&rig, &medium()) < 0.6,
        "{}",
        settled(&rig, &medium())
    );
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.preview, Some(shot("cam/wide")));
    assert_eq!(snap.switcher.settle.waiting(), None);
    assert_eq!(snap.switcher.settle.dropped(), 0);
}

#[test]
fn a_take_fires_on_the_first_tick_preview_is_settled_if_that_comes_first() {
    let mut rig = jumping(on());
    // Wait until the medium shot is nearly there, then take it with Auto.
    while settled(&rig, &medium()) < 0.55 {
        rig.step();
    }
    stage(&mut rig, &medium());
    assert_eq!(rig.auto(), Ok(CommandReply::Done));
    assert_eq!(
        waiting(&rig),
        Some(WaitingTake::new(TakeKind::Auto, medium()))
    );
    let mut n = 0;
    loop {
        n += 1;
        rig.step();
        let snap = rig.snapshot();
        let now = snap.shot(&medium()).unwrap().settled;
        if now >= 0.6 {
            // The mix starts on this very tick, advanced by its dt.
            assert!(snap.switcher.in_transition, "tick {n}: {now}");
            assert_eq!(snap.switcher.preview, Some(medium()));
            assert_eq!(snap.switcher.settle.waiting(), None);
            break;
        }
        assert!(!snap.switcher.in_transition, "tick {n}: {now}");
        assert!(snap.switcher.settle.waiting().is_some(), "tick {n}");
    }
    assert!(n < 15, "settled before the wait ran out: {n} ticks");
    rig.run_secs(1.0);
    assert_eq!(rig.core.switcher().program(), Some(&medium()));
}

#[test]
fn with_the_rule_off_every_take_is_immediate() {
    let mut rig = jumping(SettleSettings::default());
    assert_eq!(rig.snapshot().switcher.settle.rule(), None);
    stage(&mut rig, &close());
    assert!(settled(&rig, &close()) < 0.4);
    rig.cut().unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&close()), "as before");
    stage(&mut rig, &medium());
    rig.auto().unwrap();
    assert!(rig.core.switcher().in_transition(), "mixing at once");
    assert_eq!(waiting(&rig), None);
}

#[test]
fn a_settled_shot_goes_at_once_with_the_rule_on() {
    let mut rig = jumping(on());
    // The wide is always settled: taking it back is immediate.
    rig.apply(EngineCommand::CutTo { shot: close() }).unwrap();
    assert_eq!(rig.preview(), Some(shot("cam/wide")));
    rig.cut().unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&shot("cam/wide")));
    // So is a subject shot that has arrived.
    while settled(&rig, &medium()) < 0.6 {
        rig.step();
    }
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&medium()));
    assert_eq!(waiting(&rig), None);
}

#[test]
fn a_second_press_forces_the_take_at_once() {
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.run(3);
    assert!(waiting(&rig).is_some());
    // Space again, before the next tick: on program at once.
    assert_eq!(rig.cut(), Ok(CommandReply::Done));
    assert_eq!(rig.core.switcher().program(), Some(&medium()));
    assert_eq!(waiting(&rig), None);
    assert!(settled(&rig, &medium()) < 0.6, "forced, not settled");
    // Enter, then Space: the second press goes on air as pressed, a cut.
    let mut rig = jumping(on());
    stage(&mut rig, &close());
    rig.auto().unwrap();
    rig.step();
    assert!(waiting(&rig).is_some());
    rig.cut().unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&close()));
    assert!(!rig.core.switcher().in_transition());
    rig.run(20);
    assert_eq!(rig.core.switcher().program(), Some(&close()));
    assert_eq!(rig.snapshot().switcher.settle.dropped(), 0);
}

#[test]
fn a_refused_second_press_leaves_the_first_waiting() {
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    // A second press that saw another preview is refused by the fence, and
    // the first one waits on.
    assert_eq!(
        rig.apply(EngineCommand::Cut {
            preview: Some(close())
        }),
        Err(CommandError::PreviewChanged {
            seen: Some(close()),
            preview: Some(medium()),
        })
    );
    assert_eq!(
        waiting(&rig),
        Some(WaitingTake::new(TakeKind::Cut, medium()))
    );
}

#[test]
fn staging_forces_the_waiting_take_first_and_cut_now_supersedes_it() {
    // A cut, then the next shot staged at once: in that order.
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.step();
    stage(&mut rig, &close());
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(medium()));
    assert_eq!(snap.switcher.preview, Some(close()));
    assert_eq!(snap.switcher.settle.waiting(), None);
    // Cut now is never delayed, and takes over from a waiting take, which
    // ends unannounced.
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.step();
    rig.apply(EngineCommand::CutTo { shot: close() }).unwrap();
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(close()));
    assert_eq!(snap.switcher.settle.waiting(), None);
    rig.run(20);
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(close()), "the cut never fires");
    assert_eq!(snap.switcher.settle.dropped(), 0);
    // So does the T-bar, which takes preview by hand.
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.apply(EngineCommand::SetTBar { position: 0.3 }).unwrap();
    assert_eq!(waiting(&rig), None);
    assert!(rig.core.switcher().tbar_held());
}

#[test]
fn a_waiting_take_whose_preview_changes_is_dropped_with_the_fence_s_reason() {
    let mut rig = jumping(on());
    let rig = &mut rig;
    rig.apply(EngineCommand::SetDirectorMode {
        mode: DirectorMode::Suggest,
    })
    .unwrap();
    // Close on air (Cut now is never delayed), medium staged by the
    // operator and taken: it waits.
    rig.apply(EngineCommand::CutTo { shot: close() }).unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&close()));
    stage(rig, &medium());
    rig.cut().unwrap();
    assert!(waiting(rig).is_some());
    let changes = rig.snapshot().program_changes;
    // Released, the operator's hold ends, and the director stages its
    // suggestion over the medium shot after the tick has judged the take.
    rig.apply(EngineCommand::ReleasePreview).unwrap();
    rig.step();
    let theirs = rig.preview();
    assert!(theirs.is_some() && theirs != Some(medium()), "{theirs:?}");
    assert!(waiting(rig).is_some(), "judged before the director ran");
    // The next tick finds preview changed: dropped, nothing taken.
    rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.settle.waiting(), None);
    assert_eq!(snap.switcher.settle.dropped(), 1);
    assert_eq!(
        snap.switcher.settle.last_dropped(),
        Some(&(
            TakeKind::Cut,
            CommandError::PreviewChanged {
                seen: Some(medium()),
                preview: theirs,
            }
        ))
    );
    assert_eq!(snap.switcher.program, Some(close()));
    assert_eq!(snap.program_changes, changes, "nothing was taken");
}

#[test]
fn a_waiting_take_the_switcher_refuses_when_it_fires_is_dropped_with_its_reason() {
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.step();
    // The source drops out: its shots are unavailable at once, so there
    // is nothing left to wait for, and the switcher refuses the take.
    let now = Duration::from_secs_f64(rig.core.ticks() as f64 / FPS);
    let feed = SourceFeed {
        state: SourceState::Offline,
        frame: Some(rig.frame()),
        detector_loading: false,
        error: Some("unplugged".into()),
        info: None,
    };
    rig.core.tick(now, &[feed], Vec::new());
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(shot("cam/wide")));
    assert_eq!(snap.switcher.settle.waiting(), None);
    assert_eq!(snap.switcher.settle.dropped(), 1);
    assert_eq!(
        snap.switcher.settle.last_dropped(),
        Some(&(
            TakeKind::Cut,
            CommandError::Switch(SwitchError::Unavailable(medium()))
        ))
    );
}

#[test]
fn a_take_that_cannot_go_on_air_is_refused_at_once_not_after_a_wait() {
    let mut rig = jumping(on());
    rig.advancing = false;
    rig.run_secs(1.5);
    stage(&mut rig, &medium());
    assert_eq!(
        rig.cut(),
        Err(CommandError::Switch(SwitchError::Unavailable(medium())))
    );
    assert_eq!(waiting(&rig), None);
}

#[test]
fn the_director_s_own_takes_are_never_delayed() {
    // The same studio with the rule off and with a rule that would hold any
    // operator take of a subject shot not exactly on target: the director
    // in Auto mode switches identically.
    let program = |settle: SettleSettings| {
        let mut c = config(3);
        c.director.mode = DirectorMode::Auto;
        c.director.profile = ContentProfile::Music;
        c.director.takeover = Takeover::Director;
        c.switcher.settle = settle;
        let mut rig = Rig::new(c);
        let mut history = Vec::new();
        for _ in 0..(25.0 * FPS) as usize {
            rig.step();
            history.push(rig.core.switcher().program().cloned());
        }
        assert_eq!(waiting(&rig), None);
        (history, rig.snapshot().director.takes)
    };
    let strict = SettleSettings {
        threshold: 1.0,
        ..on()
    };
    let (off, takes) = program(SettleSettings::default());
    assert!(takes >= 3, "the director switched: {takes}");
    assert_eq!(program(strict), (off, takes));
}

#[test]
fn a_tbar_move_that_takes_nothing_leaves_the_waiting_take_waiting() {
    // The lever at its rest end with no transition running does nothing,
    // so the cut waits on and fires as if it had not been touched.
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.apply(EngineCommand::SetTBar { position: 0.0 }).unwrap();
    assert_eq!(
        waiting(&rig),
        Some(WaitingTake::new(TakeKind::Cut, medium()))
    );
    assert!(!rig.core.switcher().in_transition());
    assert_eq!(ticks_until_on_air(&mut rig, &medium(), 20), Some(15));
    assert_eq!(rig.snapshot().switcher.settle.dropped(), 0);
    // Nor at the other end, once a T-bar transition has flipped the lever:
    // there 1 is its rest end.
    let mut rig = jumping(on());
    stage(&mut rig, &close());
    rig.apply(EngineCommand::SetTBar { position: 1.0 }).unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&close()), "never waits");
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.apply(EngineCommand::SetTBar { position: 1.0 }).unwrap();
    assert_eq!(
        waiting(&rig),
        Some(WaitingTake::new(TakeKind::Cut, medium()))
    );
    rig.run_secs(1.0);
    assert_eq!(rig.core.switcher().program(), Some(&medium()));
}

#[test]
fn a_late_second_press_of_a_take_that_fired_by_itself_takes_nothing() {
    let wide = shot("cam/wide");
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    assert_eq!(ticks_until_on_air(&mut rig, &medium(), 20), Some(15));
    assert_eq!(rig.preview(), Some(wide.clone()), "the swap");
    let changes = rig.snapshot().program_changes;
    // The operator answers the caption just as the take goes on air, from
    // a window that drew it on air (the wide back on preview), one that
    // still drew it waiting (the medium), or one that drew a suggestion
    // staged as it went on air. Every Cut, Auto or Accept is the take's
    // second press, and its take is on air already: nothing is taken, and
    // nothing refused.
    let late = |rig: &mut Rig| {
        for seen in [wide.clone(), medium(), close()] {
            let preview = Some(seen.clone());
            for take in [
                EngineCommand::Cut {
                    preview: preview.clone(),
                },
                EngineCommand::Auto { preview },
                EngineCommand::AcceptSuggestion { shot: seen },
            ] {
                assert_eq!(rig.apply(take.clone()), Ok(CommandReply::Done), "{take:?}");
            }
        }
        let snap = rig.snapshot();
        assert_eq!(snap.switcher.program, Some(medium()));
        assert_eq!(snap.switcher.preview, Some(wide.clone()));
        assert!(!snap.switcher.in_transition);
        assert_eq!(snap.program_changes, changes);
        assert_eq!(snap.switcher.settle.dropped(), 0);
    };
    late(&mut rig);
    rig.run(14);
    late(&mut rig);
    // Half a second after it fired, a cut is a fresh take: back to the wide.
    rig.step();
    rig.cut().unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&wide));
    // Staging ends it at once: the operator has moved on.
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.run(15);
    assert_eq!(rig.core.switcher().program(), Some(&medium()));
    stage(&mut rig, &wide);
    rig.cut().unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&wide));
    // A take the second press forced has had its second press: the next is
    // a fresh take.
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.cut().unwrap();
    rig.cut().unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&medium()));
    rig.cut().unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&wide));
}

#[test]
fn a_late_press_during_the_mix_of_an_auto_that_fired_by_itself_lands_it_or_does_nothing() {
    let mut rig = jumping(on());
    stage(&mut rig, &medium());
    rig.auto().unwrap();
    rig.run(15);
    assert!(rig.core.switcher().in_transition(), "mixing to the medium");
    // Enter again: the mix carries on as it was.
    assert_eq!(rig.auto(), Ok(CommandReply::Done));
    assert!(rig.core.switcher().in_transition());
    assert_eq!(rig.core.switcher().queued(), None);
    // Space: the medium lands at once, as a Cut during any mix does.
    assert_eq!(rig.cut(), Ok(CommandReply::Done));
    assert!(!rig.core.switcher().in_transition());
    assert_eq!(rig.core.switcher().program(), Some(&medium()));
}

#[test]
fn accepting_a_suggestion_waits_like_auto_and_a_second_accept_forces_it() {
    // Moving subjects, the wide on air and the director suggesting; the
    // first suggestion it stages that is well short of settled.
    let suggesting = || {
        let mut c = config(3);
        c.director.mode = DirectorMode::Suggest;
        c.director.profile = ContentProfile::Music;
        c.switcher.settle = on();
        let mut rig = Rig::new(c);
        rig.run(5);
        preview_and_cut(&mut rig, "cam/wide");
        for _ in 0..(20.0 * FPS) as usize {
            rig.step();
            let snap = rig.snapshot();
            if let Some(s) = snap.director.suggestion.clone()
                && snap.switcher.preview.as_ref() == Some(&s)
                && snap.shot(&s).is_some_and(|v| v.settled < 0.5)
            {
                return (rig, s);
            }
        }
        panic!("no suggestion of a travelling shot");
    };
    let (mut rig, s) = suggesting();
    assert_eq!(
        rig.apply(EngineCommand::AcceptSuggestion { shot: s.clone() }),
        Ok(CommandReply::Done)
    );
    assert_eq!(
        waiting(&rig),
        Some(WaitingTake::new(TakeKind::Auto, s.clone()))
    );
    assert!(!rig.core.switcher().in_transition());
    // Accept again: the mix starts at once.
    rig.apply(EngineCommand::AcceptSuggestion { shot: s.clone() })
        .unwrap();
    assert!(rig.core.switcher().in_transition());
    assert_eq!(rig.preview(), Some(s.clone()), "coming in");
    assert_eq!(waiting(&rig), None);
    // Left alone, the mix starts within half a second, as Enter's does.
    let (mut rig, s) = suggesting();
    rig.apply(EngineCommand::AcceptSuggestion { shot: s.clone() })
        .unwrap();
    let mut n = 0;
    while !rig.core.switcher().in_transition() {
        let snap = rig.snapshot();
        assert!(snap.switcher.settle.waiting().is_some(), "tick {n}");
        // The accepted shot holds preview while it waits, as a shot the
        // operator staged does, so the director's next suggestion cannot
        // stage over it and drop the take.
        assert_eq!(snap.director.preview_held, Some(s.clone()), "tick {n}");
        n += 1;
        assert!(n <= 15, "past the wait");
        rig.step();
    }
    assert_eq!(rig.preview(), Some(s.clone()));
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.settle.dropped(), 0);
    assert!(
        snap.director.held_back > 0,
        "the director moved on meanwhile"
    );
    rig.run_secs(1.0);
    assert_eq!(rig.core.switcher().program(), Some(&s));
}
