//! Framing through the core: the detector's extent reaches the shots, whole
//! people are cut at body lines when the query asks for a person, shots the
//! zoom ceiling holds back are flagged in the snapshot, and a prompt's shot
//! words reach the catalog while only its parsed phrase reaches the
//! detector.

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
