//! Switching: cut, auto and the T-bar move program and preview, with tally
//! following both buses during a transition; invalid switcher commands are
//! refused and change nothing.

use ac_switcher::{Transition, TransitionKind};

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
    rig.apply(EngineCommand::Auto).unwrap();
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
    assert_eq!(
        rig.apply(EngineCommand::Cut),
        Err(CommandError::Switch(SwitchError::NoPreview))
    );
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
