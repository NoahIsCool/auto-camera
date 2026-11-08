//! Framing through the core: the detector's extent reaches the shots, whole
//! people are cut at body lines when the query asks for a person, shots the
//! zoom ceiling holds back are flagged in the snapshot, a prompt's shot
//! words reach the catalog while only its parsed phrase reaches the
//! detector, every query a subject answers makes its shots, an on-air
//! shot keeps its basis whatever happens to its query, and a person cut
//! off by the bottom of the picture is framed down from the crown.

use ac_core::Extent;

use super::*;

/// A whole person half the frame high, standing still mid-stage.
fn standing() -> Rect {
    Rect::new(0.45, 0.3, 0.1, 0.5)
}

/// A rig on an empty (static) stage whose detector always sees `standing()`
/// as `extent`, with full and close shots at the standard 4x ceiling.
fn rig(extent: Extent) -> Rig {
    let mut c = config(0);
    c.queries[0].presets = Some(vec![
        PresetSpec::Size(ShotSize::Full),
        PresetSpec::Size(ShotSize::Close),
    ]);
    let detector = ScriptedDetector::constant(
        "one-person",
        vec![Detection {
            query: QueryId("person".into()),
            bbox: standing(),
            score: 0.9,
            extent,
        }],
    );
    let mut rig = Rig::new(c).with_detector(detector);
    rig.run_secs(6.0);
    rig
}

#[test]
fn whole_people_are_cut_at_body_lines_and_limited_shots_say_so() {
    let rig = rig(Extent::Person);
    let snap = rig.snapshot();
    assert_eq!(snap.subjects.len(), 1);
    let full = snap.shot(&shot("cam/1/full")).unwrap();
    assert!(!full.limited_by_zoom);
    // Crown to feet with 10% headroom: 0.5 / 0.9 of the frame high.
    assert!((full.crop.h - 0.5 / 0.9).abs() < 0.04, "{:?}", full.crop);
    assert!(full.crop.y < standing().y && full.crop.bottom() >= 0.78);
    // A close-up needs about 9x; it is drawn at the 4x ceiling, and says so.
    let close = snap.shot(&shot("cam/1/close")).unwrap();
    assert!(close.limited_by_zoom);
    assert!((close.crop.w - 0.25).abs() < 0.02, "{:?}", close.crop);
    assert!(close.crop.y < standing().y, "the crown stays in frame");
    let wide = snap.shot(&shot("cam/wide")).unwrap();
    assert!(!wide.limited_by_zoom);
}

#[test]
fn object_boxes_keep_the_fill_rule() {
    let rig = rig(Extent::Object);
    let snap = rig.snapshot();
    // The fill rule makes the full shot wider than the source: the wide.
    let full = snap.shot(&shot("cam/1/full")).unwrap();
    assert!((full.crop.w - 1.0).abs() < 0.04, "{:?}", full.crop);
    assert!(!full.limited_by_zoom);
    let close = snap.shot(&shot("cam/1/close")).unwrap();
    assert!((close.crop.h - 0.5 / 0.92).abs() < 0.04, "{:?}", close.crop);
    assert!(!close.limited_by_zoom);
}

#[test]
fn object_prompts_keep_the_fill_rule_on_person_boxes() {
    // People-first matching answers "guitar" with the whole player; the
    // prompt asks for an object, so the box is not cut at body lines.
    let mut c = config(0);
    c.queries[0].prompt = "guitar".into();
    c.queries[0].presets = Some(vec![PresetSpec::Size(ShotSize::Full)]);
    let detector = ScriptedDetector::constant(
        "player",
        vec![Detection {
            query: QueryId("person".into()),
            bbox: standing(),
            score: 0.9,
            extent: Extent::Person,
        }],
    );
    let mut rig = Rig::new(c).with_detector(detector);
    rig.run_secs(6.0);
    let snap = rig.snapshot();
    assert_eq!(snap.subjects.len(), 1);
    let full = snap.shot(&shot("cam/1/full")).unwrap();
    assert!((full.crop.w - 1.0).abs() < 0.04, "{:?}", full.crop);
}

/// A tall whole person, crown at 0.05, 0.9 of the frame high.
fn tall() -> Rect {
    Rect::new(0.4, 0.05, 0.2, 0.9)
}

#[test]
fn shot_words_make_the_shots_and_the_detector_sees_the_parsed_phrase() {
    let mut c = config(0);
    c.queries[0] = QuerySettings::new("Speaker", "speaker upper body");
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = asked.clone();
    let detector = ScriptedDetector::from_fn("speaker", move |_, queries| {
        let mut log = log.lock().unwrap();
        log.extend(queries.iter().map(|q| q.prompt.clone()));
        Ok(queries
            .iter()
            .filter(|q| q.prompt == "a person speaking")
            .map(|q| Detection {
                query: q.id.clone(),
                bbox: tall(),
                score: 0.9,
                extent: Extent::Person,
            })
            .collect())
    });
    let mut rig = Rig::new(c).with_detector(detector);
    rig.run_secs(6.0);
    let asked = asked.lock().unwrap().clone();
    assert!(!asked.is_empty());
    assert!(asked.iter().all(|p| p == "a person speaking"), "{asked:?}");
    let snap = rig.snapshot();
    let q = &snap.queries[0];
    assert_eq!(q.prompt, "speaker upper body");
    assert_eq!(q.parse.model_phrase, "a person speaking");
    let ids: Vec<&str> = snap.shots.iter().map(|s| s.id.0.as_str()).collect();
    assert_eq!(ids, ["cam/wide", "cam/1/medium", "cam/1/medium_close"]);
    // Waist-up: three heads (0.12 each) below the crown, 8% headroom.
    let medium = snap.shot(&shot("cam/1/medium")).unwrap();
    assert!(!medium.limited_by_zoom);
    assert!(
        (medium.crop.h - 0.36 / 0.92).abs() < 0.02,
        "{:?}",
        medium.crop
    );
    assert!(
        (medium.crop.y - (0.05 - 0.08 * 0.36 / 0.92)).abs() < 0.02,
        "{:?}",
        medium.crop
    );
    // Chest-up: 2.3 heads, 6% headroom.
    let chest = snap.shot(&shot("cam/1/medium_close")).unwrap();
    assert!(
        (chest.crop.h - 0.276 / 0.94).abs() < 0.02,
        "{:?}",
        chest.crop
    );
}

/// A rig whose detector answers every query with `standing()`, a whole
/// person, as people-first OWL does for prompts that share an object or a
/// described person.
fn everyone_sees_one_person(c: StudioConfig) -> Rig {
    everyone_sees(standing(), c)
}

/// A rig whose detector answers every query with `bbox`, a whole person.
fn everyone_sees(bbox: Rect, c: StudioConfig) -> Rig {
    let detector = ScriptedDetector::from_fn("one-person", move |_, queries| {
        Ok(queries
            .iter()
            .map(|q| Detection {
                query: q.id.clone(),
                bbox,
                score: 0.9,
                extent: Extent::Person,
            })
            .collect())
    });
    Rig::new(c).with_detector(detector)
}

#[test]
fn each_query_s_shot_words_make_shots_on_a_shared_subject() {
    // Both prompts ask the detector for "a person speaking", so they find
    // the same box and one subject answers both.
    let mut c = config(0);
    c.queries = vec![
        QuerySettings::new("Speaker wide", "speaker upper body"),
        QuerySettings::new("Speaker tight", "speaker close-up"),
    ];
    let mut rig = everyone_sees_one_person(c);
    rig.run_secs(6.0);
    let snap = rig.snapshot();
    assert_eq!(snap.subjects.len(), 1, "{:?}", snap.subjects);
    assert_eq!(snap.subjects[0].aliases.len(), 1);
    let shots: Vec<(&str, &str)> = snap
        .shots
        .iter()
        .map(|s| (s.id.0.as_str(), s.name.as_str()))
        .collect();
    assert_eq!(
        shots,
        [
            ("cam/wide", "Wide"),
            ("cam/1/close", "Speaker tight 1 Close"),
            ("cam/1/medium", "Speaker wide 1 Medium"),
            ("cam/1/medium_close", "Speaker wide 1 Medium close"),
        ]
    );
}

/// The program shot's crop width and zoom verdict.
fn on_air(rig: &Rig) -> (f64, bool) {
    let snap = rig.snapshot();
    let id = snap.switcher.program.clone().expect("a program shot");
    let s = snap.shot(&id).expect("the program shot exists");
    (s.crop.w, s.limited_by_zoom)
}

#[test]
fn removing_a_query_never_reframes_its_shot_on_air() {
    // "guitar" is an object prompt; people-first matching answers it with
    // the player. On air, the shot keeps the fill rule after its query is
    // removed and the subject, an orphan, goes on tracking.
    let mut c = config(1);
    c.queries[0] = QuerySettings {
        presets: Some(vec![PresetSpec::Size(ShotSize::Full)]),
        ..QuerySettings::new("Guitar", "guitar")
    };
    let pattern = SyntheticPattern::new(W, H, FPS, 1).unwrap();
    let mut truth = synthetic_detector(pattern, 0.9);
    let detector = ScriptedDetector::from_fn("players", move |frame, queries| {
        let found = truth.detect(frame, queries)?;
        Ok(found
            .into_iter()
            .map(|d| Detection {
                extent: Extent::Person,
                ..d
            })
            .collect())
    });
    let mut rig = Rig::new(c).with_detector(detector);
    rig.run(31);
    preview_and_cut(&mut rig, "cam/1/full");
    rig.run_secs(1.0);
    let before = on_air(&rig);
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId("guitar".into()),
    })
    .unwrap();
    for _ in 0..5 {
        rig.run_secs(0.5);
        let snap = rig.snapshot();
        assert_eq!(snap.subjects[0].state, TrackState::Tracking);
        let after = on_air(&rig);
        assert!((after.0 - before.0).abs() < 1e-6, "{before:?} -> {after:?}");
        assert_eq!(after.1, before.1);
    }
}

#[test]
fn a_promoted_alias_reframes_a_shot_only_once_it_is_off_air() {
    // "guitar" (an object) and "guitarist" (a person) find the same player;
    // "guitar" sorts first, so its basis frames the shared "close" preset.
    let mut c = config(0);
    let close = || Some(vec![PresetSpec::Size(ShotSize::Close)]);
    c.queries = vec![
        QuerySettings {
            presets: close(),
            ..QuerySettings::new("Guitar", "guitar")
        },
        QuerySettings {
            presets: close(),
            ..QuerySettings::new("Guitarist", "guitarist")
        },
    ];
    let mut rig = everyone_sees_one_person(c);
    rig.run_secs(6.0);
    assert_eq!(rig.snapshot().subjects[0].query, QueryId("guitar".into()));
    preview_and_cut(&mut rig, "cam/1/close");
    rig.run_secs(1.0);
    let before = on_air(&rig);
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId("guitar".into()),
    })
    .unwrap();
    rig.run_secs(4.0);
    assert_eq!(
        rig.snapshot().subjects[0].query,
        QueryId("guitarist".into())
    );
    let after = on_air(&rig);
    assert!((after.0 - before.0).abs() < 1e-6, "{before:?} -> {after:?}");
    // Off air, the shot follows its new query: body lines, at the ceiling.
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/wide"),
    })
    .unwrap();
    rig.run_secs(6.0);
    let off = rig.snapshot();
    let s = off.shot(&shot("cam/1/close")).unwrap();
    assert!(
        s.limited_by_zoom && (s.crop.w - 0.25).abs() < 0.02,
        "{:?}",
        s.crop
    );
}

/// The sermon speaker of the Mac run: a whole-person box cut off by the
/// bottom of the picture at about the knees, crown in the picture.
fn cut_off_by_the_bottom() -> Rect {
    Rect::new(0.45, 0.65, 0.10, 0.35)
}

/// The sermon's queries: one person found by all three.
fn sermon_queries() -> StudioConfig {
    let mut c = config(0);
    c.queries = vec![
        QuerySettings::new("Speaker", "speaker upper body"),
        QuerySettings::new("Guitarist", "guitarist close-up"),
        QuerySettings::new("Drummer", "drummer wide"),
    ];
    c
}

/// The crown's place in `crop`, as a fraction of its height from the top.
fn crown_headroom(bbox: &Rect, crop: &Rect) -> f64 {
    (bbox.y - crop.y) / crop.h
}

#[test]
fn a_speaker_cut_off_by_the_bottom_gets_a_limited_close_and_distinct_tighter_shots() {
    // Nothing untruncated is ever seen, so nothing is remembered: the
    // tight sizes are cut on an assumed head of a third of the box. The
    // fill rule framed the whole visible body (0.38 / 0.45 / 0.58, crown
    // 8% / 22% / 40% down).
    let mut rig = everyone_sees(cut_off_by_the_bottom(), sermon_queries());
    rig.run_secs(30.0);
    let snap = rig.snapshot();
    assert_eq!(snap.subjects.len(), 1, "{:?}", snap.subjects);
    let bbox = snap.subjects[0].bbox;
    assert!((bbox.bottom() - 1.0).abs() < 0.01, "{bbox:?}");
    let close = snap.shot(&shot("cam/1/close")).unwrap();
    assert_eq!(close.name, "Guitarist 1 Close", "the alias's Close");
    // (shot, limited, crop height, headroom)
    for (id, limited, h, headroom) in [
        ("cam/1/close", true, 0.25, 0.03),
        ("cam/1/medium_close", false, 0.285, 0.06),
        ("cam/1/medium", false, 0.38, 0.08),
    ] {
        let s = snap.shot(&shot(id)).unwrap();
        assert_eq!(s.limited_by_zoom, limited, "{id}");
        assert!((s.crop.h - h).abs() < 0.01, "{id}: {:?}", s.crop);
        let crown = crown_headroom(&bbox, &s.crop);
        assert!((crown - headroom).abs() < 0.01, "{id}: crown at {crown}");
        assert_eq!(s.same_picture_as, None, "{id}");
    }
    // Full keeps the fill rule.
    let full = snap.shot(&shot("cam/1/full")).unwrap();
    assert!((full.crop.h - 0.35 / 0.4).abs() < 0.01, "{:?}", full.crop);
    assert!(!full.limited_by_zoom);
}

#[test]
fn a_mid_distance_speaker_cut_off_by_the_bottom_has_one_tight_picture() {
    // A box 0.25 high: Close and Medium close both need more than the 4x
    // ceiling, so both are drawn at it, and the Medium close says it is
    // the Close's picture. The fill rule gave two reachable, looser shots.
    let mut c = sermon_queries();
    c.queries.truncate(2);
    let mut rig = everyone_sees(Rect::new(0.45, 0.75, 0.07, 0.25), c);
    rig.run_secs(30.0);
    let snap = rig.snapshot();
    assert_eq!(snap.subjects.len(), 1, "{:?}", snap.subjects);
    let close = snap.shot(&shot("cam/1/close")).unwrap();
    let chest = snap.shot(&shot("cam/1/medium_close")).unwrap();
    let medium = snap.shot(&shot("cam/1/medium")).unwrap();
    assert!(close.limited_by_zoom && chest.limited_by_zoom);
    assert!((close.crop.w - 0.25).abs() < 0.01, "{:?}", close.crop);
    assert!((chest.crop.w - 0.25).abs() < 0.01, "{:?}", chest.crop);
    assert_eq!(close.same_picture_as, None);
    assert_eq!(chest.same_picture_as, Some(shot("cam/1/close")));
    assert!(!medium.limited_by_zoom, "{:?}", medium.crop);
    assert_eq!(medium.same_picture_as, None);
}
