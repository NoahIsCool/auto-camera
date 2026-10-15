//! Framing through the core: the detector's extent reaches the shots, whole
//! people are cut at body lines, and shots the zoom ceiling holds back are
//! flagged in the snapshot.

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
    c.queries[0].presets = vec![
        PresetSpec::Size(ShotSize::Full),
        PresetSpec::Size(ShotSize::Close),
    ];
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
