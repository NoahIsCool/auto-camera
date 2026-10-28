//! Switching: cut, auto and the T-bar move program and preview, with tally
//! following both buses during a transition; invalid switcher commands are
//! refused and change nothing; a take pressed during a transition fires once
//! at its end, after a re-check of its shot; Cut cuts it short when its
//! incoming shot is still available, and Cut now cuts to a named shot,
//! neither while the T-bar holds it; a shot staged during a transition
//! keeps its subject protected and, once landed, is not replaced by the
//! director's suggestion.

use ac_switcher::{Queued, Tally, Transition, TransitionKind};

use super::*;

#[test]
fn cut_and_auto_change_program_with_tally() {
    let mut rig = Rig::new(config(2));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    assert_eq!(rig.core.switcher().program(), Some(&shot("cam/wide")));
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/medium"),
    })
    .unwrap();
    rig.auto().unwrap();
    rig.run(6);
    let snap = rig.snapshot();
    assert!(snap.switcher.in_transition);
    assert!(snap.shot(&shot("cam/wide")).unwrap().tally.program);
    assert!(snap.shot(&shot("cam/1/medium")).unwrap().tally.program);
    rig.run(30);
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(shot("cam/1/medium")));
    assert_eq!(snap.switcher.preview, Some(shot("cam/wide")));
    let wide = snap.shot(&shot("cam/wide")).unwrap().tally;
    assert!(!wide.program && wide.preview);
    assert_eq!(snap.program_changes, 2);
    assert!(
        snap.subjects
            .iter()
            .filter(|s| s.protected)
            .all(|s| s.id == SubjectId(1))
    );
}

#[test]
fn invalid_switcher_commands_are_refused_without_effect() {
    let mut rig = Rig::new(config(1));
    rig.step();
    assert_eq!(
        rig.apply(EngineCommand::SetPreview { shot: shot("nope") }),
        Err(CommandError::UnknownShot(shot("nope")))
    );
    assert_eq!(rig.cut(), Err(CommandError::Switch(SwitchError::NoPreview)));
    assert!(matches!(
        rig.apply(EngineCommand::SetTBar { position: f64::NAN }),
        Err(CommandError::Invalid(_))
    ));
    let bad = Transition {
        kind: TransitionKind::Mix,
        duration: -1.0,
    };
    assert!(matches!(
        rig.apply(EngineCommand::SetTransition { transition: bad }),
        Err(CommandError::Switch(SwitchError::InvalidDuration(_)))
    ));
    assert!(matches!(
        rig.apply(EngineCommand::StopOutput),
        Err(CommandError::Invalid(_))
    ));
    assert_eq!(rig.core.switcher().program(), None);
}

#[test]
fn tbar_takes_the_preview_shot() {
    let mut rig = Rig::new(config(1));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/close"),
    })
    .unwrap();
    rig.apply(EngineCommand::SetTBar { position: 0.5 }).unwrap();
    rig.step();
    assert!(rig.snapshot().switcher.overlay.is_some());
    rig.apply(EngineCommand::SetTBar { position: 1.0 }).unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&shot("cam/1/close")));
}

/// A rig with "cam/wide" on program and a mix to "cam/1/medium" running for
/// `ticks` ticks.
fn mixing_to_medium(subjects: u32, ticks: usize) -> Rig {
    let mut rig = Rig::new(config(subjects));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/medium"),
    })
    .unwrap();
    rig.auto().unwrap();
    rig.run(ticks);
    assert!(rig.snapshot().switcher.in_transition);
    rig
}

#[test]
fn a_take_during_a_mix_fires_once_at_its_end() {
    let mut rig = mixing_to_medium(2, 6);
    let next = shot("cam/2/medium");
    assert_eq!(
        rig.apply(EngineCommand::SetPreview { shot: shot("nope") }),
        Err(CommandError::UnknownShot(shot("nope")))
    );
    rig.apply(EngineCommand::SetPreview { shot: next.clone() })
        .unwrap();
    rig.auto().unwrap();
    rig.auto().unwrap();
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.preview, Some(shot("cam/1/medium")));
    assert_eq!(
        snap.switcher.queued,
        Some(Queued {
            shot: next.clone(),
            take: true
        })
    );
    assert_eq!(snap.shot(&next).unwrap().tally, Tally::default());
    let mut ticks = 0;
    while rig.core.switcher().program() == Some(&shot("cam/wide")) {
        rig.step();
        ticks += 1;
        assert!(ticks < 30, "the first mix ends");
    }
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(shot("cam/1/medium")));
    assert_eq!(snap.switcher.preview, Some(next.clone()));
    assert!(
        snap.switcher.in_transition,
        "the queued take started as the first mix ended"
    );
    assert_eq!(snap.switcher.queued, None);
    rig.run(60);
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(next));
    assert_eq!(snap.switcher.preview, Some(shot("cam/1/medium")));
    assert!(!snap.switcher.in_transition);
    assert_eq!(snap.program_changes, 3, "the wide, then each take once");
    assert_eq!(snap.switcher.dropped_takes, 0);
}

#[test]
fn a_queued_take_whose_shot_became_unavailable_is_dropped_with_a_reason() {
    let mut rig = Rig::new(config(2));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    let slow = Transition {
        kind: TransitionKind::Mix,
        duration: 3.0,
    };
    rig.apply(EngineCommand::SetTransition { transition: slow })
        .unwrap();
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/medium"),
    })
    .unwrap();
    rig.auto().unwrap();
    rig.step();
    let next = shot("cam/2/medium");
    rig.apply(EngineCommand::SetPreview { shot: next.clone() })
        .unwrap();
    rig.auto().unwrap();
    // The source stalls; a second later its shots are unavailable, and the
    // mix ends two seconds after that.
    rig.advancing = false;
    rig.run_secs(3.0);
    let snap = rig.snapshot();
    assert!(!snap.shot(&next).unwrap().available);
    assert!(!snap.switcher.in_transition, "nothing more is taken");
    assert_eq!(snap.switcher.program, Some(shot("cam/1/medium")));
    assert_eq!(snap.switcher.preview, Some(next.clone()), "still staged");
    assert_eq!(snap.switcher.dropped_takes, 1);
    assert_eq!(
        snap.switcher.last_dropped,
        Some(SwitchError::Unavailable(next))
    );
    assert_eq!(snap.program_changes, 2);
}

#[test]
fn cut_during_a_mix_lands_the_new_shot_in_the_same_tick() {
    let mut rig = mixing_to_medium(1, 6);
    assert!(rig.snapshot().switcher.overlay.is_some());
    assert_eq!(
        rig.apply(EngineCommand::SetTBar { position: 0.5 }),
        Err(CommandError::Switch(SwitchError::Busy)),
        "the T-bar cannot take over the mix"
    );
    rig.cut().unwrap();
    let cut_short = rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(shot("cam/1/medium")));
    assert!(!snap.switcher.in_transition);
    assert_eq!(snap.switcher.overlay, None);
    let wide = snap.shot(&shot("cam/wide")).unwrap().tally;
    assert!(!wide.program && wide.preview);
    assert!(snap.shot(&shot("cam/1/medium")).unwrap().tally.program);
    assert_eq!(snap.program_changes, 2);
    // The same studio cut straight to the shot draws the same picture on
    // that tick: the shot alone, nothing of the mix left.
    let mut direct = Rig::new(config(1));
    direct.run(31);
    preview_and_cut(&mut direct, "cam/wide");
    direct.run(6);
    preview_and_cut(&mut direct, "cam/1/medium");
    let cut = direct.step();
    assert_eq!(cut_short.program.data, cut.program.data);
}

#[test]
fn cut_during_a_mix_refuses_an_incoming_shot_whose_source_stalled() {
    let mut rig = Rig::new(config(1));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    let slow = Transition {
        kind: TransitionKind::Mix,
        duration: 3.0,
    };
    rig.apply(EngineCommand::SetTransition { transition: slow })
        .unwrap();
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/medium"),
    })
    .unwrap();
    rig.auto().unwrap();
    rig.advancing = false;
    rig.run_secs(1.5);
    let incoming = shot("cam/1/medium");
    assert!(!rig.snapshot().shot(&incoming).unwrap().available);
    assert_eq!(
        rig.cut(),
        Err(CommandError::Switch(SwitchError::Unavailable(incoming)))
    );
    let snap = rig.snapshot();
    assert!(snap.switcher.in_transition, "the mix carries on");
    assert_eq!(snap.switcher.program, Some(shot("cam/wide")));
}

#[test]
fn cut_now_ends_a_mix_on_the_named_shot_in_one_command() {
    let mut rig = mixing_to_medium(2, 6);
    let named = shot("cam/2/close");
    assert_eq!(
        rig.apply(EngineCommand::CutTo { shot: shot("nope") }),
        Err(CommandError::UnknownShot(shot("nope")))
    );
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/2/medium"),
    })
    .unwrap();
    rig.auto().unwrap();
    rig.apply(EngineCommand::CutTo {
        shot: named.clone(),
    })
    .unwrap();
    let cut = rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(named.clone()));
    assert_eq!(snap.switcher.preview, Some(shot("cam/1/medium")));
    assert!(!snap.switcher.in_transition);
    assert_eq!(snap.switcher.queued, None, "the queued take is cancelled");
    assert!(snap.shot(&named).unwrap().tally.program);
    // The program is the named shot alone on that tick.
    let mut direct = Rig::new(config(2));
    direct.run(31);
    preview_and_cut(&mut direct, "cam/wide");
    direct.run(6);
    preview_and_cut(&mut direct, "cam/2/close");
    assert_eq!(cut.program.data, direct.step().program.data);
    rig.run(60);
    assert_eq!(rig.snapshot().switcher.program, Some(named));
}

#[test]
fn cut_and_cut_now_are_refused_while_the_tbar_holds_a_transition() {
    let mut rig = Rig::new(config(1));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/close"),
    })
    .unwrap();
    assert!(!rig.snapshot().switcher.tbar_held);
    rig.apply(EngineCommand::SetTBar { position: 0.5 }).unwrap();
    rig.step();
    assert!(rig.snapshot().switcher.tbar_held);
    assert_eq!(rig.cut(), Err(CommandError::Switch(SwitchError::TBarHeld)));
    assert_eq!(
        rig.apply(EngineCommand::CutTo {
            shot: shot("cam/1/medium")
        }),
        Err(CommandError::Switch(SwitchError::TBarHeld))
    );
    rig.apply(EngineCommand::SetTBar { position: 1.0 }).unwrap();
    let snap = rig.snapshot();
    assert!(!snap.switcher.tbar_held);
    assert_eq!(snap.switcher.program, Some(shot("cam/1/close")));
}

/// A suggesting studio with a mix from "cam/wide" to "cam/1/medium"
/// running and "cam/1/close" staged meanwhile; the staged shot.
fn suggesting_while_mixing() -> (Rig, ShotId) {
    let mut c = config(2);
    c.director.mode = DirectorMode::Suggest;
    let mut rig = Rig::new(c);
    rig.run_secs(2.0);
    preview_and_cut(&mut rig, "cam/wide");
    rig.run_secs(1.0);
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/medium"),
    })
    .unwrap();
    rig.auto().unwrap();
    rig.run(3);
    let mine = shot("cam/1/close");
    rig.apply(EngineCommand::SetPreview { shot: mine.clone() })
        .unwrap();
    assert_eq!(
        rig.snapshot().switcher.queued,
        Some(Queued {
            shot: mine.clone(),
            take: false
        })
    );
    (rig, mine)
}

/// Checks, on the tick after `staged` was the director's last staging,
/// that the director wanted to stage a new suggestion and the operator's
/// shot `mine` stayed on preview instead.
fn kept_on_preview(rig: &Rig, mine: &ShotId, staged: &Option<ShotId>) {
    let snap = rig.snapshot();
    assert!(!snap.switcher.in_transition);
    assert_eq!(snap.switcher.program, Some(shot("cam/1/medium")));
    let suggestion = rig.core.decision().suggestion.clone();
    assert!(
        suggestion.is_some() && suggestion != *staged && suggestion != Some(mine.clone()),
        "the director has a new suggestion to stage: {suggestion:?}"
    );
    assert_eq!(snap.switcher.preview.as_ref(), Some(mine));
    assert_eq!(rig.core.last_staged, suggestion, "the edge is spent");
}

#[test]
fn a_shot_staged_during_a_mix_is_not_replaced_by_the_suggestion_when_it_lands() {
    // The mix ends by the clock.
    let (mut rig, mine) = suggesting_while_mixing();
    let staged = rig.core.last_staged.clone();
    let mut ticks = 0;
    while rig.snapshot().switcher.in_transition {
        rig.step();
        ticks += 1;
        assert!(ticks < 60, "the mix ends");
    }
    kept_on_preview(&rig, &mine, &staged);
    assert_eq!(rig.snapshot().director.preview_held, Some(mine.clone()));
    // It stays, as a shot staged by hand does, until it is taken or
    // released.
    for _ in 0..30 {
        rig.step();
        assert_eq!(rig.snapshot().switcher.preview, Some(mine.clone()));
    }

    // Cut ends the same mix a tick earlier; the director runs on the tick
    // the clock would have ended it.
    let (mut rig, mine) = suggesting_while_mixing();
    let staged = rig.core.last_staged.clone();
    rig.run(ticks - 1);
    assert!(rig.snapshot().switcher.in_transition);
    rig.cut().unwrap();
    assert_eq!(rig.snapshot().switcher.preview, Some(mine.clone()));
    rig.step();
    kept_on_preview(&rig, &mine, &staged);
}

#[test]
fn the_subject_of_a_shot_staged_during_a_mix_is_protected() {
    let mut rig = mixing_to_medium(2, 3);
    let mine = shot("cam/2/close");
    rig.apply(EngineCommand::SetPreview { shot: mine.clone() })
        .unwrap();
    let protected = |rig: &Rig, id: u32| {
        rig.snapshot()
            .subjects
            .iter()
            .any(|s| s.id == SubjectId(id) && s.protected)
    };
    assert!(protected(&rig, 2), "protected as soon as it is staged");
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId("person".into()),
    })
    .unwrap();
    rig.run(60);
    let snap = rig.snapshot();
    assert!(!snap.switcher.in_transition);
    assert_eq!(snap.switcher.program, Some(shot("cam/1/medium")));
    assert_eq!(snap.switcher.preview, Some(mine.clone()));
    assert!(snap.shot(&mine).is_some(), "preview names a listed shot");
    assert!(protected(&rig, 2));
}
