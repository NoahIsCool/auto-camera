//! Tests for the shot catalog.

use super::*;
use ac_core::ShotSize;

/// Output and source aspect (16:9).
const A: f64 = 16.0 / 9.0;

/// Medium and close presets.
fn presets() -> Vec<Preset> {
    vec![
        Preset::new("medium", "Medium", ShotSize::Medium),
        Preset::new("close", "Close", ShotSize::Close),
    ]
}

/// A person-sized box at horizontal position `x`.
fn person(x: f64) -> Rect {
    Rect::new(x, 0.3, 0.08, 0.3)
}

/// Shot specs for subjects `ids` with `presets` on one source.
fn specs(ids: &[u32], presets: &[Preset]) -> Vec<ShotSpec> {
    let source = SourceId("cam".into());
    let subjects = ids
        .iter()
        .map(|&i| SubjectShots {
            id: SubjectId(i),
            bbox: person(0.2 + 0.1 * i as f64),
            query_name: "Person",
            presets,
        })
        .collect();
    desired_shots(&[SourceShots {
        id: &source,
        name: "Stage",
        subjects,
    }])
}

/// Facts of a live 16:9 source.
fn live(_: usize) -> SourceFacts {
    SourceFacts {
        aspect: A,
        available: true,
    }
}

#[test]
fn ids_and_names_derive_from_what_is_framed() {
    let p = presets();
    let s = specs(&[1, 2], &p);
    let ids: Vec<&str> = s.iter().map(|s| s.shot.id.0.as_str()).collect();
    assert_eq!(
        ids,
        [
            "cam/wide",
            "cam/1/medium",
            "cam/1/close",
            "cam/2/medium",
            "cam/2/close"
        ]
    );
    assert_eq!(s[0].shot.name, "Wide");
    assert_eq!(s[1].shot.name, "Person 1 Medium");
    assert_eq!(s[1].preset.as_ref().unwrap().id, "medium");
}

#[test]
fn several_sources_prefix_names_with_the_source() {
    let (a, b) = (SourceId("a".into()), SourceId("b".into()));
    let s = desired_shots(&[
        SourceShots {
            id: &a,
            name: "Left",
            subjects: vec![],
        },
        SourceShots {
            id: &b,
            name: "Right",
            subjects: vec![],
        },
    ]);
    assert_eq!(s[0].shot.name, "Left Wide");
    assert_eq!(s[1].shot.name, "Right Wide");
    assert_eq!(s[1].source, 1);
}

#[test]
fn sync_keeps_surviving_entries_and_reports_changes() {
    let p = presets();
    let mut c = Catalog::new();
    let (added, removed) = c.sync(specs(&[1], &p), &[A], A, &FramerConfig::default());
    assert_eq!(added.len(), 3);
    assert!(removed.is_empty());
    c.mark_on_program(&ShotId("cam/1/medium".into()), Duration::from_secs(3));
    let (added, removed) = c.sync(specs(&[1, 2], &p), &[A], A, &FramerConfig::default());
    assert_eq!(added.len(), 2);
    assert!(removed.is_empty());
    let kept = c.get(&ShotId("cam/1/medium".into())).unwrap();
    assert_eq!(kept.last_on_program, Some(Duration::from_secs(3)));
    let (added, removed) = c.sync(specs(&[2], &p), &[A], A, &FramerConfig::default());
    assert!(added.is_empty());
    assert_eq!(
        removed,
        [ShotId("cam/1/close".into()), ShotId("cam/1/medium".into())]
    );
    assert_eq!(c.len(), 3);
    assert!(c.get(&ShotId("cam/1/medium".into())).is_none());
}

#[test]
fn tracking_shots_follow_their_subject_and_settle() {
    let p = presets();
    let mut c = Catalog::new();
    c.sync(specs(&[1], &p), &[A], A, &FramerConfig::default());
    let moved = person(0.6);
    let facts = |_: usize, _: SubjectId| {
        Some(SubjectFacts {
            bbox: moved,
            tracking: true,
            confidence: 0.9,
        })
    };
    c.step(1.0 / 30.0, &[live(0)], facts, A);
    let early = c.get(&ShotId("cam/1/medium".into())).unwrap().clone();
    assert_eq!(early.status, ShotStatus::Tracking);
    assert!(early.settled < 0.5, "{}", early.settled);
    for _ in 0..300 {
        c.step(1.0 / 30.0, &[live(0)], facts, A);
    }
    let late = c.get(&ShotId("cam/1/medium".into())).unwrap();
    assert!(late.settled > 0.8, "{}", late.settled);
    assert!(late.quality > 0.7 && late.quality <= 0.9);
    assert!(late.crop.contains_rect(&moved));
    assert!(late.available);
}

#[test]
fn lost_subjects_hold_and_score_zero() {
    let p = presets();
    let mut c = Catalog::new();
    c.sync(specs(&[1], &p), &[A], A, &FramerConfig::default());
    let before = c.get(&ShotId("cam/1/close".into())).unwrap().crop;
    let lost = |_: usize, _: SubjectId| {
        Some(SubjectFacts {
            bbox: person(0.9),
            tracking: false,
            confidence: 0.9,
        })
    };
    for _ in 0..60 {
        c.step(1.0 / 30.0, &[live(0)], lost, A);
    }
    let e = c.get(&ShotId("cam/1/close".into())).unwrap();
    assert_eq!(e.status, ShotStatus::Lost);
    assert_eq!(e.quality, 0.0);
    assert_eq!(e.crop, before);
    assert!(e.available);
}

#[test]
fn unavailable_sources_make_every_shot_unavailable() {
    let p = presets();
    let mut c = Catalog::new();
    c.sync(specs(&[1], &p), &[A], A, &FramerConfig::default());
    let down = [SourceFacts {
        aspect: A,
        available: false,
    }];
    c.step(1.0 / 30.0, &down, |_, _| None, A);
    assert!(c.entries().iter().all(|e| !e.available && e.quality == 0.0));
    assert!(!c.is_available(&ShotId("cam/wide".into())));
    assert!(!c.is_available(&ShotId("nope".into())));
    c.step(1.0 / 30.0, &[live(0)], |_, _| None, A);
    let wide = c.get(&ShotId("cam/wide".into())).unwrap();
    assert_eq!((wide.status, wide.quality), (ShotStatus::Wide, 1.0));
    assert_eq!(wide.crop, Rect::FULL);
}

#[test]
fn wide_crop_fits_the_output_aspect() {
    let r = wide_crop(A, 1.0);
    assert!((r.h - 1.0).abs() < 1e-12);
    assert!((r.w - 9.0 / 16.0).abs() < 1e-12);
    assert!(Rect::FULL.contains_rect(&r));
    let r = wide_crop(4.0 / 3.0, A);
    assert!((r.w - 1.0).abs() < 1e-12 && r.h < 1.0);
}

#[test]
fn settledness_is_one_on_target_and_falls_with_error() {
    let at = |x: f64, w: f64| Composition {
        center: Point { x, y: 0.5 },
        width: w,
    };
    assert_eq!(settledness(at(0.5, 0.4), at(0.5, 0.4), A, A), 1.0);
    let half = settledness(at(0.5, 0.4), at(0.55, 0.4), A, A);
    assert!((half - 0.5).abs() < 1e-9, "{half}");
    assert_eq!(settledness(at(0.5, 0.4), at(0.9, 0.4), A, A), 0.0);
    assert_eq!(settledness(at(0.5, 0.4), at(0.5, f64::NAN), A, A), 0.0);
}
