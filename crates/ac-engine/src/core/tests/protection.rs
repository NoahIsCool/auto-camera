//! Protection: subjects on program or preview are never removed, whether
//! they vanish from the picture, their query is removed or they are hidden,
//! until they leave the air.

use super::*;

#[test]
fn protected_subjects_are_not_retired() {
    let mut c = config(2);
    c.subjects.lost_after_s = 0.5;
    c.subjects.retire_after_s = 1.0;
    let pattern = SyntheticPattern::new(W, H, FPS, 2).unwrap();
    let truth = synthetic_detector(pattern, 0.9);
    let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let g = gone.clone();
    let mut truth = truth;
    let detector = ScriptedDetector::from_fn("vanishing", move |frame, queries| {
        if g.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(Vec::new())
        } else {
            truth.detect(frame, queries)
        }
    });
    let mut rig = Rig::new(c).with_detector(detector);
    rig.run(31);
    preview_and_cut(&mut rig, "cam/1/medium");
    // Both subjects vanish from the picture and from the detector.
    gone.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.picture = Some(SyntheticPattern::new(W, H, FPS, 0).unwrap());
    rig.run_secs(3.0);
    let snap = rig.snapshot();
    assert_eq!(snap.subjects.len(), 1, "{:?}", snap.subjects);
    let kept = &snap.subjects[0];
    assert_eq!(kept.id, SubjectId(1));
    assert!(kept.protected);
    assert_eq!(kept.state, TrackState::Lost);
    let on_air = snap.shot(&shot("cam/1/medium")).unwrap();
    assert!(on_air.tally.program);
    assert_eq!(on_air.quality, 0.0);
    assert!(snap.shot(&shot("cam/2/medium")).is_none());
    assert!(snap.sources[0].vision.retired >= 1);
}

#[test]
fn a_subject_cut_to_program_is_protected_before_the_next_tick() {
    let mut rig = Rig::new(config(2));
    rig.run(31);
    // No tick between the cut and the removal.
    preview_and_cut(&mut rig, "cam/1/close");
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId("person".into()),
    })
    .unwrap();
    rig.run(1);
    let snap = rig.snapshot();
    let ids: Vec<SubjectId> = snap.subjects.iter().map(|s| s.id).collect();
    assert_eq!(ids, [SubjectId(1)]);
    assert_eq!(snap.switcher.program, Some(shot("cam/1/close")));
}

#[test]
fn hiding_an_on_air_subject_keeps_its_shot_until_it_leaves_the_air() {
    let mut rig = Rig::new(config(2));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/2/close");
    rig.apply(EngineCommand::Hide {
        source: cam(),
        subject: SubjectId(2),
        hidden: true,
    })
    .unwrap();
    rig.run(2);
    assert!(rig.snapshot().shot(&shot("cam/2/close")).is_some());
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/close"),
    })
    .unwrap();
    rig.run(2);
    assert!(rig.snapshot().shot(&shot("cam/2/close")).is_none());
}

/// [`config`] with a second query, "Body", framing full shots; the ground
/// truth answers it with the same boxes, so each subject answers both.
fn two_queries() -> StudioConfig {
    let mut c = config(1);
    c.queries.push(QuerySettings {
        id: None,
        name: "Body".into(),
        prompt: "body".into(),
        presets: Some(vec![PresetSpec::Size(ShotSize::Full)]),
        literal: false,
    });
    c
}

/// Ids of the snapshot's shots.
fn shot_ids(snap: &EngineSnapshot) -> Vec<String> {
    snap.shots.iter().map(|s| s.id.0.clone()).collect()
}

#[test]
fn an_on_air_shot_survives_its_query_being_replaced_by_an_alias() {
    let mut rig = Rig::new(two_queries());
    // Two passes (applied on ticks 1 and 31): an alias frames shots once
    // two observing passes have seen it.
    rig.run(32);
    let snap = rig.snapshot();
    // Admission is fair across prompts: "body" sorts first, so it is the
    // primary query and "person" an alias. Both queries' presets make
    // shots, the primary's first.
    let subject = &snap.subjects[0];
    assert_eq!(subject.query, QueryId("body".into()));
    assert_eq!(subject.aliases, [QueryId("person".into())]);
    assert_eq!(
        shot_ids(&snap),
        ["cam/wide", "cam/1/full", "cam/1/medium", "cam/1/close"]
    );
    preview_and_cut(&mut rig, "cam/1/full");
    rig.run(2);
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId("body".into()),
    })
    .unwrap();
    rig.run(2);
    let snap = rig.snapshot();
    assert_eq!(snap.subjects[0].query, QueryId("person".into()));
    // The alias's shots stay and the one on air stays too.
    assert_eq!(
        shot_ids(&snap),
        ["cam/wide", "cam/1/medium", "cam/1/close", "cam/1/full"]
    );
    assert_eq!(snap.switcher.program, Some(shot("cam/1/full")));
    assert!(snap.render_issues.is_empty(), "{:?}", snap.render_issues);
    assert!(snap.shot(&shot("cam/1/full")).unwrap().tally.program);
    assert!(snap.subjects[0].protected);
    let out = rig.step();
    assert!(
        !out.program.data.chunks(4).all(|p| p == [0, 0, 0, 255]),
        "the program went black"
    );
    // Cutting to another of its shots keeps it on air, and the shot too.
    preview_and_cut(&mut rig, "cam/1/close");
    rig.run(2);
    assert!(rig.snapshot().shot(&shot("cam/1/full")).is_some());
    // Once it leaves program and preview, only the new query's framings
    // remain.
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/wide"),
    })
    .unwrap();
    rig.run(2);
    let snap = rig.snapshot();
    assert!(!snap.subjects[0].protected);
    assert_eq!(shot_ids(&snap), ["cam/wide", "cam/1/medium", "cam/1/close"]);
}

/// [`config`] whose "person" query frames one close-up with 0.03 headroom
/// and a 6x ceiling, plus a query `id`, named `name`, with the default
/// close-up (4x) on the same boxes. Its id sorts before "person", so it is
/// the primary query and its close-up is the one made. That close-up is cut
/// to program and the query is removed; returns the on-air close-up's
/// (headroom, max zoom) and name afterwards.
fn close_after_removing_the_primary(id: &str, name: &str) -> ((f64, f64), String) {
    let mut c = config(1);
    c.queries[0].presets = Some(vec![PresetSpec::Full(Preset {
        id: "close".into(),
        name: "Close".into(),
        size: ShotSize::Close,
        headroom: 0.03,
        max_zoom: 6.0,
    })]);
    c.queries.push(QuerySettings {
        id: Some(id.into()),
        name: name.into(),
        prompt: "body".into(),
        presets: Some(vec![PresetSpec::Size(ShotSize::Close)]),
        literal: false,
    });
    let mut rig = Rig::new(c);
    rig.run(31);
    assert_eq!(rig.snapshot().subjects[0].query, QueryId(id.into()));
    let e = rig.core.catalog().get(&shot("cam/1/close")).unwrap();
    assert_eq!(e.preset.as_ref().unwrap().max_zoom, 4.0, "the primary's");
    preview_and_cut(&mut rig, "cam/1/close");
    rig.run(2);
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId(id.into()),
    })
    .unwrap();
    rig.run(2);
    let snap = rig.snapshot();
    assert_eq!(snap.subjects[0].query, QueryId("person".into()));
    assert_eq!(snap.switcher.program, Some(shot("cam/1/close")));
    let e = rig.core.catalog().get(&shot("cam/1/close")).unwrap();
    let preset = e.preset.as_ref().unwrap();
    ((preset.headroom, preset.max_zoom), e.shot.name.clone())
}

#[test]
fn a_kept_shot_frames_with_the_new_query_s_preset() {
    let (preset, name) = close_after_removing_the_primary("body", "Body");
    assert_eq!(preset, (0.03, 6.0));
    assert_eq!(name, "Person 1 Close");
}

/// The removed query shares its display name with the one promoted: the
/// shot is still named "Person", but takes the promoted query's preset,
/// since a held shot finds the query it is named after by id.
#[test]
fn a_kept_shot_takes_the_new_preset_when_both_queries_share_a_name() {
    let (preset, name) = close_after_removing_the_primary("body", "Person");
    assert_eq!(
        preset,
        (0.03, 6.0),
        "the removed query's preset stayed on air"
    );
    assert_eq!(name, "Person 1 Close");
}
