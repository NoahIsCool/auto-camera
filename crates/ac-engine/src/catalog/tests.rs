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

/// Plans for `presets` from query "Person" on `basis`.
fn plans(presets: &[Preset], basis: Basis) -> Vec<ShotPlan> {
    presets
        .iter()
        .map(|p| ShotPlan {
            preset: p.clone(),
            basis,
            query: QueryId("person".into()),
            query_name: "Person".into(),
        })
        .collect()
}

/// Shot specs for subjects `ids` with `presets` on one source.
fn specs(ids: &[u32], presets: &[Preset]) -> Vec<ShotSpec> {
    let source = SourceId("cam".into());
    let subjects = ids
        .iter()
        .map(|&i| SubjectShots {
            id: SubjectId(i),
            bbox: person(0.2 + 0.1 * i as f64),
            extent: Extent::Object,
            plans: plans(presets, Basis::Object),
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
    assert_eq!((s[0].basis, s[1].basis), (Basis::Object, Basis::Object));
}

#[test]
fn each_shot_is_named_after_and_measured_by_its_own_query() {
    let source = SourceId("cam".into());
    let mut both = plans(&presets()[..1], Basis::Person);
    both.push(ShotPlan {
        preset: Preset::new("full", "Full", ShotSize::Full),
        basis: Basis::Object,
        query: QueryId("guitar".into()),
        query_name: "Guitar".into(),
    });
    let s = desired_shots(&[SourceShots {
        id: &source,
        name: "Stage",
        subjects: vec![SubjectShots {
            id: SubjectId(1),
            bbox: person(0.4),
            extent: Extent::Person,
            plans: both,
        }],
    }]);
    let named: Vec<(&str, Basis)> = s[1..]
        .iter()
        .map(|s| (s.shot.name.as_str(), s.basis))
        .collect();
    assert_eq!(
        named,
        [
            ("Person 1 Medium", Basis::Person),
            ("Guitar 1 Full", Basis::Object)
        ]
    );
    // Each shot remembers its query by id; the wide has none.
    let queries: Vec<Option<&str>> = s
        .iter()
        .map(|s| s.query.as_ref().map(|q| q.0.as_str()))
        .collect();
    assert_eq!(queries, [None, Some("person"), Some("guitar")]);
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
            extent: Extent::Object,
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
fn whole_people_too_small_for_their_size_are_flagged_from_the_start() {
    let p = presets();
    let source = SourceId("cam".into());
    let subject = |extent| SubjectShots {
        id: SubjectId(1),
        bbox: person(0.4),
        extent,
        plans: plans(&p, Basis::Person),
    };
    let sync = |extent| {
        let mut c = Catalog::new();
        let specs = desired_shots(&[SourceShots {
            id: &source,
            name: "Stage",
            subjects: vec![subject(extent)],
        }]);
        c.sync(specs, &[A], A, &FramerConfig::default());
        c
    };
    let close = ShotId("cam/1/close".into());
    // A whole person 0.3 high needs about 16x for a close-up; 4x is the cap.
    let mut c = sync(Extent::Person);
    assert!(c.get(&close).unwrap().limited_by_zoom);
    let facts = |_: usize, _: SubjectId| {
        Some(SubjectFacts {
            bbox: person(0.4),
            extent: Extent::Person,
            tracking: true,
            confidence: 0.9,
        })
    };
    c.step(1.0 / 30.0, &[live(0)], facts, A);
    assert!(c.get(&close).unwrap().limited_by_zoom);
    assert!(!c.get(&ShotId("cam/wide".into())).unwrap().limited_by_zoom);
    // The same box as an object fills the frame by the fill rule.
    assert!(!sync(Extent::Object).get(&close).unwrap().limited_by_zoom);
}

/// A catalog for one subject at `person(0.4)`, a whole person, with
/// `presets` on `basis`, stepped for `secs` while tracking.
fn person_catalog(presets: &[Preset], basis: Basis, secs: f64) -> Catalog {
    let source = SourceId("cam".into());
    let mut c = Catalog::new();
    let specs = desired_shots(&[SourceShots {
        id: &source,
        name: "Stage",
        subjects: vec![SubjectShots {
            id: SubjectId(1),
            bbox: person(0.4),
            extent: Extent::Person,
            plans: plans(presets, basis),
        }],
    }]);
    c.sync(specs, &[A], A, &FramerConfig::default());
    let facts = |_: usize, _: SubjectId| {
        Some(SubjectFacts {
            bbox: person(0.4),
            extent: Extent::Person,
            tracking: true,
            confidence: 0.9,
        })
    };
    for _ in 0..(secs * 30.0) as usize {
        c.step(1.0 / 30.0, &[live(0)], facts, A);
    }
    c
}

#[test]
fn an_object_basis_keeps_the_fill_rule_on_a_person_box() {
    // "guitar close-up" answered with the whole player: the fill rule.
    let p = presets();
    let object = person_catalog(&p, Basis::Object, 2.0);
    let person = person_catalog(&p, Basis::Person, 2.0);
    let close = ShotId("cam/1/close".into());
    let fill = object.get(&close).unwrap();
    assert!(
        (fill.crop.h - 0.3 / ShotSize::Close.fill()).abs() < 0.02,
        "{:?}",
        fill.crop
    );
    assert!(!fill.limited_by_zoom);
    assert!(person.get(&close).unwrap().limited_by_zoom);
}

#[test]
fn zoom_limited_shots_that_look_alike_are_marked_once() {
    // A whole person 0.3 high: Medium needs about 7.7x and Medium close
    // about 10x, so both are drawn at the 4x ceiling.
    let p = vec![
        Preset::new("medium", "Medium", ShotSize::Medium),
        Preset::new("medium_close", "Medium close", ShotSize::MediumClose),
        Preset::new("full", "Full", ShotSize::Full),
    ];
    let c = person_catalog(&p, Basis::Person, 6.0);
    let get = |id: &str| c.get(&ShotId(id.into())).unwrap();
    assert!(get("cam/1/medium").limited_by_zoom && get("cam/1/medium_close").limited_by_zoom);
    assert_eq!(get("cam/1/medium").same_picture_as, None);
    assert_eq!(
        get("cam/1/medium_close").same_picture_as,
        Some(ShotId("cam/1/medium".into()))
    );
    // A reachable shot and the wide are never marked.
    assert!(!get("cam/1/full").limited_by_zoom);
    assert_eq!(get("cam/1/full").same_picture_as, None);
    assert_eq!(get("cam/wide").same_picture_as, None);
    // A different ceiling makes a different picture.
    let wider = vec![
        Preset::new("medium", "Medium", ShotSize::Medium),
        Preset {
            max_zoom: 8.0,
            ..Preset::new("close", "Close", ShotSize::Close)
        },
    ];
    let c = person_catalog(&wider, Basis::Person, 6.0);
    assert_eq!(
        c.get(&ShotId("cam/1/close".into()))
            .unwrap()
            .same_picture_as,
        None
    );
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
            extent: Extent::Object,
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
        limited_by_zoom: false,
        zoom_fit: f64::INFINITY,
    };
    assert_eq!(settledness(at(0.5, 0.4), at(0.5, 0.4), A, A), 1.0);
    let half = settledness(at(0.5, 0.4), at(0.55, 0.4), A, A);
    assert!((half - 0.5).abs() < 1e-9, "{half}");
    assert_eq!(settledness(at(0.5, 0.4), at(0.9, 0.4), A, A), 0.0);
    assert_eq!(settledness(at(0.5, 0.4), at(0.5, f64::NAN), A, A), 0.0);
}
