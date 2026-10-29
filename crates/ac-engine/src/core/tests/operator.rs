//! The operator owns preview: Cut and Auto are refused when preview changed
//! after the operator saw it, and Accept when the suggestion did; a shot
//! the operator staged holds preview against the director in Suggest mode
//! until it is taken or released; a skipped suggestion stays out for its
//! window; and each takeover setting decides what the operator's own
//! switching does to a director in Auto mode, timed by the ticks' `dt`,
//! and stays as configured when the operator changes the profile.

use ac_director::{Handover, Reason, Takeover};

use super::*;

/// A studio of three subjects whose director runs in `mode` with the music
/// profile (2.5 s minimum dwell), `takeover`, a 2 s pause and a 5 s skip
/// window, after 12 s: in Auto mode it has put the wide on air at once and
/// taken a subject shot at the wide's maximum dwell (10 s).
fn directed(mode: DirectorMode, takeover: Takeover) -> Rig {
    let mut c = config(3);
    c.director.mode = mode;
    c.director.profile = ContentProfile::Music;
    c.director.takeover = takeover;
    c.director.pause_s = 2.0;
    c.director.skip_s = 5.0;
    let mut rig = Rig::new(c);
    rig.run_secs(12.0);
    rig
}

/// An available subject shot that is neither on a bus nor suggested.
fn other_shot(rig: &Rig) -> ShotId {
    let snap = rig.snapshot();
    let busy = [
        snap.switcher.program.clone(),
        snap.switcher.preview.clone(),
        snap.director.suggestion.clone(),
    ];
    snap.subject_shots()
        .find(|s| s.available && !busy.contains(&Some(s.id.clone())))
        .map(|s| s.id.clone())
        .expect("a free shot")
}

/// The time the core's newest tick ran at.
fn now(rig: &Rig) -> Duration {
    rig.core.now()
}

#[test]
fn a_cut_is_refused_when_the_director_restaged_after_the_key_press() {
    let mut rig = directed(DirectorMode::Auto, Takeover::Director);
    let program = rig.snapshot().switcher.program.clone();
    assert!(program.is_some());
    // The operator stages a shot of their own: the director wins, so it
    // stays in Auto, and the snapshot the operator sees shows their shot.
    let mine = other_shot(&rig);
    assert_eq!(
        rig.apply(EngineCommand::SetPreview { shot: mine.clone() }),
        Ok(CommandReply::Done)
    );
    let seen = rig.snapshot().switcher.preview.clone();
    assert_eq!(seen, Some(mine.clone()));
    // Before Space arrives, the director restages its suggestion.
    rig.step();
    let theirs = rig.preview();
    assert!(theirs.is_some() && theirs != seen, "{theirs:?}");
    let changes = rig.snapshot().program_changes;
    assert_eq!(
        rig.apply(EngineCommand::Cut {
            preview: seen.clone()
        }),
        Err(CommandError::PreviewChanged {
            seen: seen.clone(),
            preview: theirs.clone(),
        })
    );
    assert_eq!(
        rig.apply(EngineCommand::Auto {
            preview: seen.clone()
        }),
        Err(CommandError::PreviewChanged {
            seen,
            preview: theirs.clone(),
        })
    );
    let snap = rig.snapshot();
    assert_eq!(snap.program_changes, changes, "nothing was taken");
    assert_eq!(snap.switcher.preview, theirs);
}

#[test]
fn a_cut_is_refused_when_auto_restages_as_its_pause_ends() {
    let mut rig = directed(DirectorMode::Auto, Takeover::Pause);
    let mine = other_shot(&rig);
    assert_eq!(
        rig.apply(EngineCommand::SetPreview { shot: mine.clone() }),
        Ok(CommandReply::TookOver(Handover::Paused(
            Duration::from_secs(2)
        )))
    );
    // Paused, the director holds off; then it stages its suggestion again.
    rig.run_secs(1.9);
    assert_eq!(rig.preview(), Some(mine.clone()));
    rig.run_secs(0.2);
    let theirs = rig.preview();
    assert_ne!(theirs, Some(mine.clone()));
    assert_eq!(
        rig.apply(EngineCommand::Cut {
            preview: Some(mine.clone())
        }),
        Err(CommandError::PreviewChanged {
            seen: Some(mine),
            preview: theirs,
        })
    );
}

#[test]
fn a_fenced_take_of_what_preview_shows_goes_through() {
    let mut rig = Rig::new(config(1));
    rig.run(31);
    assert_eq!(
        rig.apply(EngineCommand::Cut { preview: None }),
        Err(CommandError::Switch(SwitchError::NoPreview)),
        "an empty preview matches, and the switcher refuses"
    );
    assert_eq!(
        rig.apply(EngineCommand::Cut {
            preview: Some(shot("cam/wide"))
        }),
        Err(CommandError::PreviewChanged {
            seen: Some(shot("cam/wide")),
            preview: None
        })
    );
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/medium"),
    })
    .unwrap();
    rig.apply(EngineCommand::Auto {
        preview: Some(shot("cam/1/medium")),
    })
    .unwrap();
    rig.run(3);
    // During the mix preview shows the incoming shot: that is what Cut
    // checks, and what it lands.
    assert_eq!(
        rig.apply(EngineCommand::Cut {
            preview: Some(shot("cam/wide"))
        }),
        Err(CommandError::PreviewChanged {
            seen: Some(shot("cam/wide")),
            preview: Some(shot("cam/1/medium"))
        })
    );
    rig.apply(EngineCommand::Cut {
        preview: Some(shot("cam/1/medium")),
    })
    .unwrap();
    assert_eq!(rig.core.switcher().program(), Some(&shot("cam/1/medium")));
}

#[test]
fn suggest_mode_never_overwrites_an_operator_staged_preview() {
    let mut rig = directed(DirectorMode::Suggest, Takeover::Suggest);
    preview_and_cut(&mut rig, "cam/wide");
    rig.run_secs(1.0);
    let suggested = rig.snapshot().director.suggestion.clone();
    assert!(suggested.is_some());
    assert_eq!(rig.preview(), suggested, "the director staged it");
    let mine = other_shot(&rig);
    rig.apply(EngineCommand::SetPreview { shot: mine.clone() })
        .unwrap();
    assert_eq!(rig.snapshot().director.preview_held, Some(mine.clone()));
    // Every suggestion the director moves on to is passed over, counted,
    // and the operator's shot stays; skipping makes it move on.
    let mut passed = 0;
    for round in 0..3 {
        let current = rig.snapshot().director.suggestion.clone().unwrap();
        if current == mine {
            break;
        }
        rig.apply(EngineCommand::Skip { shot: current }).unwrap();
        for _ in 0..30 {
            rig.step();
            assert_eq!(rig.preview(), Some(mine.clone()), "round {round}");
        }
        let snap = rig.snapshot();
        if let Some(s) = snap.director.suggestion.clone().filter(|s| s != &mine) {
            assert_eq!(snap.director.last_held_back, Some(s));
            passed += 1;
        }
    }
    let snap = rig.snapshot();
    assert!(passed > 0, "the director wanted something else");
    assert!(
        snap.director.held_back >= passed,
        "{}",
        snap.director.held_back
    );
    assert_eq!(snap.director.preview_held, Some(mine.clone()));
    // Taken, the hold ends, and the director stages its suggestion again on
    // the next tick.
    rig.apply(EngineCommand::Cut {
        preview: Some(mine.clone()),
    })
    .unwrap();
    assert_eq!(rig.snapshot().director.preview_held, None);
    rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(mine));
    assert_eq!(snap.switcher.preview, snap.director.suggestion);
    // Released, likewise.
    let next = other_shot(&rig);
    rig.apply(EngineCommand::SetPreview { shot: next.clone() })
        .unwrap();
    rig.run(5);
    assert_eq!(rig.preview(), Some(next));
    rig.apply(EngineCommand::ReleasePreview).unwrap();
    rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.director.preview_held, None);
    assert_eq!(snap.switcher.preview, snap.director.suggestion);
}

#[test]
fn a_hold_ends_when_its_shot_leaves_preview_or_the_mode_changes() {
    let mut rig = directed(DirectorMode::Suggest, Takeover::Suggest);
    preview_and_cut(&mut rig, "cam/wide");
    let mine = other_shot(&rig);
    rig.apply(EngineCommand::SetPreview { shot: mine.clone() })
        .unwrap();
    // Cut now to another shot: the wide moves to preview in its place.
    let elsewhere = other_shot(&rig);
    rig.apply(EngineCommand::CutTo { shot: elsewhere }).unwrap();
    assert_eq!(rig.snapshot().director.preview_held, None);
    rig.apply(EngineCommand::SetPreview { shot: mine.clone() })
        .unwrap();
    rig.apply(EngineCommand::SetDirectorMode {
        mode: DirectorMode::Suggest,
    })
    .unwrap();
    assert_eq!(rig.snapshot().director.preview_held, None);
    rig.apply(EngineCommand::SetPreview { shot: mine.clone() })
        .unwrap();
    rig.apply(EngineCommand::Skip { shot: mine.clone() })
        .unwrap();
    assert_eq!(rig.snapshot().director.preview_held, None);
    rig.step();
    assert_ne!(rig.preview(), Some(mine), "skipped, it is replaced");
}

#[test]
fn a_skipped_shot_is_not_suggested_again_within_its_window() {
    let mut rig = directed(DirectorMode::Suggest, Takeover::Suggest);
    preview_and_cut(&mut rig, "cam/wide");
    rig.run_secs(1.0);
    assert_eq!(
        rig.apply(EngineCommand::Skip { shot: shot("nope") }),
        Err(CommandError::UnknownShot(shot("nope")))
    );
    let skipped = rig.snapshot().director.suggestion.clone().unwrap();
    let at = now(&rig);
    rig.apply(EngineCommand::Skip {
        shot: skipped.clone(),
    })
    .unwrap();
    assert_eq!(
        rig.snapshot().director.skipped,
        [(skipped.clone(), Duration::from_secs(5))]
    );
    // The director stages another suggestion on the next tick (preview was
    // its own, not held), and does not come back to the skipped shot for
    // the 5 s window, while it stays a candidate the whole time.
    rig.step();
    assert_ne!(rig.preview(), Some(skipped.clone()));
    while now(&rig) < at + Duration::from_secs(5) {
        let snap = rig.snapshot();
        assert_ne!(snap.director.suggestion, Some(skipped.clone()));
        assert!(snap.shot(&skipped).is_some_and(|s| s.available));
        rig.step();
    }
    assert!(rig.snapshot().director.skipped.is_empty());
}

#[test]
fn by_default_a_manual_action_drops_auto_to_suggest() {
    let mut rig = directed(DirectorMode::Auto, Takeover::Suggest);
    let takes = rig.snapshot().director.takes;
    assert!(takes >= 2, "{takes}");
    let mine = other_shot(&rig);
    assert_eq!(
        rig.apply(EngineCommand::SetPreview { shot: mine.clone() }),
        Ok(CommandReply::TookOver(Handover::ToSuggest))
    );
    assert_eq!(rig.snapshot().director.mode, DirectorMode::Suggest);
    // From now on the operator switches: no take of the director's for
    // 20 s, and their shot holds preview.
    let program = rig.snapshot().switcher.program.clone();
    for _ in 0..(20.0 * FPS) as usize {
        rig.step();
        assert_eq!(rig.preview(), Some(mine.clone()));
    }
    let snap = rig.snapshot();
    assert_eq!(snap.director.takes, takes);
    assert_eq!(snap.switcher.program, program);
    assert!(snap.director.suggestion.is_some(), "it still suggests");
    // In Suggest mode further actions are plain switching.
    assert_eq!(
        rig.apply(EngineCommand::Cut {
            preview: Some(mine)
        }),
        Ok(CommandReply::Done)
    );
}

#[test]
fn with_the_pause_setting_a_manual_action_holds_auto_takes_for_its_window() {
    let mut rig = directed(DirectorMode::Auto, Takeover::Pause);
    let mine = other_shot(&rig);
    assert_eq!(
        rig.apply(EngineCommand::CutTo { shot: mine.clone() }),
        Ok(CommandReply::TookOver(Handover::Paused(
            Duration::from_secs(2)
        )))
    );
    let at = now(&rig);
    let takes = rig.snapshot().director.takes;
    assert_eq!(rig.snapshot().director.paused, Some(Duration::from_secs(2)));
    // The pause counts down with the ticks, and nothing is taken (not even
    // a fallback), until the first tick 2 s after the action.
    let mut last = Duration::from_secs(2);
    loop {
        rig.step();
        let snap = rig.snapshot();
        let Some(left) = snap.director.paused else {
            break;
        };
        assert!(left < last, "{left:?} after {last:?}");
        assert_eq!(left, Duration::from_secs(2) - (now(&rig) - at));
        assert!(matches!(snap.director.reason, Reason::Paused { .. }));
        assert_eq!(snap.director.mode, DirectorMode::Auto);
        assert_eq!(snap.director.takes, takes);
        assert_eq!(snap.switcher.program, Some(mine.clone()));
        last = left;
    }
    let ended = now(&rig) - at;
    let tick = Duration::from_secs_f64(1.0 / FPS);
    assert!(ended >= Duration::from_secs(2) && ended < Duration::from_secs(2) + tick);
    // Auto then resumes: it takes again within its dwell times.
    rig.run_secs(10.0);
    assert!(rig.snapshot().director.takes > takes);
    // Each further action restarts the pause from its own time.
    let next = other_shot(&rig);
    rig.apply(EngineCommand::SetPreview { shot: next.clone() })
        .unwrap();
    rig.run_secs(1.5);
    rig.apply(EngineCommand::SetPreview { shot: next.clone() })
        .unwrap();
    rig.run_secs(1.5);
    let snap = rig.snapshot();
    assert!(snap.director.paused.is_some(), "restarted 1.5 s ago");
    assert_eq!(snap.switcher.preview, Some(next), "held while paused");
}

#[test]
fn with_the_director_setting_auto_carries_on_over_the_operator() {
    let mut rig = directed(DirectorMode::Auto, Takeover::Director);
    let takes = rig.snapshot().director.takes;
    let mine = other_shot(&rig);
    assert_eq!(
        rig.apply(EngineCommand::SetPreview { shot: mine.clone() }),
        Ok(CommandReply::Done)
    );
    assert_eq!(rig.snapshot().director.preview_held, Some(mine.clone()));
    rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.director.mode, DirectorMode::Auto);
    assert_eq!(snap.director.paused, None);
    assert_ne!(snap.switcher.preview, Some(mine), "restaged over it");
    assert_eq!(snap.director.preview_held, None, "which ends the hold");
    rig.run_secs(10.0);
    assert!(rig.snapshot().director.takes > takes);
}

#[test]
fn accepting_a_suggestion_that_changed_since_it_was_seen_is_refused() {
    let mut rig = directed(DirectorMode::Suggest, Takeover::Suggest);
    preview_and_cut(&mut rig, "cam/wide");
    rig.run_secs(1.0);
    let seen = rig.snapshot().director.suggestion.clone().unwrap();
    // The suggestion moves on (here because it was skipped) before the
    // operator's Accept arrives.
    rig.apply(EngineCommand::Skip { shot: seen.clone() })
        .unwrap();
    rig.step();
    let before = rig.snapshot();
    let now = before.director.suggestion.clone().unwrap();
    assert_ne!(now, seen);
    assert_eq!(
        rig.apply(EngineCommand::AcceptSuggestion { shot: seen.clone() }),
        Err(CommandError::SuggestionChanged {
            seen,
            suggestion: now.clone()
        })
    );
    // Nothing changed: no take, preview as it was.
    let after = rig.snapshot();
    assert_eq!(after.switcher, before.switcher);
    assert!(!after.switcher.in_transition);
    // Accepting the suggestion as it is now takes it.
    rig.apply(EngineCommand::AcceptSuggestion { shot: now.clone() })
        .unwrap();
    assert_eq!(rig.preview(), Some(now.clone()));
    rig.run_secs(1.0);
    assert_eq!(rig.snapshot().switcher.program, Some(now));
}

#[test]
fn changing_the_profile_keeps_the_takeover_pause_and_skip_settings() {
    let mut rig = directed(DirectorMode::Auto, Takeover::Pause);
    let before = rig.snapshot().director;
    assert_eq!(before.profile, ContentProfile::Music);
    // None of them is the default, so a reset would show.
    let defaults = ac_director::DirectorConfig::new(DirectorMode::Auto, ContentProfile::Talk);
    assert_ne!(before.takeover, defaults.takeover);
    assert_ne!(before.pause, defaults.pause);
    assert_ne!(before.skip, defaults.skip);
    rig.apply(EngineCommand::SetDirectorProfile {
        profile: ContentProfile::Talk,
    })
    .unwrap();
    let after = rig.snapshot().director;
    assert_eq!(after.profile, ContentProfile::Talk);
    assert_eq!(after.mode, DirectorMode::Auto);
    assert_eq!(after.takeover, Takeover::Pause);
    assert_eq!(after.pause, Duration::from_secs(2));
    assert_eq!(after.skip, Duration::from_secs(5));
    // And the pause still works as configured.
    let mine = other_shot(&rig);
    assert_eq!(
        rig.apply(EngineCommand::SetPreview { shot: mine }),
        Ok(CommandReply::TookOver(Handover::Paused(
            Duration::from_secs(2)
        )))
    );
}
