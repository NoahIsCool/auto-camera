//! Tests for person-basis composition, built around the worked example of
//! the model-picker design (section 4.5): a whole person half the frame
//! high, 16:9 in and out.
//! Boxes cut by the bottom of the picture are in [`bottom_cut`].

use ac_core::{Extent, Preset, Rect, ShotSize};

use super::*;
use crate::compose::{SubjectGeometry, compose_subject};

mod bottom_cut;

/// 16:9.
const HD: f64 = 16.0 / 9.0;

/// The sizes a person can be framed at, loosest first.
const SIZES: [ShotSize; 4] = [
    ShotSize::Full,
    ShotSize::Medium,
    ShotSize::MediumClose,
    ShotSize::Close,
];

/// A whole person half the frame high, standing with room above and below.
fn half_frame_person() -> Rect {
    Rect::new(0.45, 0.3, 0.1, 0.5)
}

/// The standard preset for `size` (4x zoom ceiling).
fn preset(size: ShotSize) -> Preset {
    Preset::new("p", "P", size)
}

/// The standard preset for `size` with a ceiling no shot here reaches.
fn unlimited(size: ShotSize) -> Preset {
    Preset {
        max_zoom: 100.0,
        ..preset(size)
    }
}

/// The crop for a person box with no remembered head.
fn person_crop(bbox: Rect, p: &Preset, src: f64, out: f64) -> Composition {
    compose_subject(&SubjectGeometry::new(bbox, Extent::Person), p, src, out)
}

#[test]
fn the_fill_rule_frames_a_whole_person_far_too_wide() {
    // The bug: today's sizes assume upper-body boxes.
    let b = half_frame_person();
    let height = |size| compose(&b, &preset(size), HD, HD).rect(HD, HD).h;
    assert_eq!(height(ShotSize::Full), 1.0, "wants 1.25: the wide shot");
    assert!((height(ShotSize::Medium) - 0.5 / 0.6).abs() < 1e-9);
    assert!(
        (height(ShotSize::Close) - 0.5 / 0.92).abs() < 1e-9,
        "full length"
    );
}

#[test]
fn the_worked_example_needs_these_frame_heights_and_zooms() {
    let b = half_frame_person();
    // (size, frame height, zoom needed), by hand from the canon.
    let expected = [
        (ShotSize::Full, 0.5 / 0.9, 1.8),
        (ShotSize::Medium, 3.0 * 0.5 / 7.5 / 0.92, 4.6),
        (ShotSize::MediumClose, 2.3 * 0.5 / 7.5 / 0.94, 6.13),
        (ShotSize::Close, 1.6 * 0.5 / 7.5 / 0.97, 9.09),
    ];
    for (size, h, zoom) in expected {
        let c = person_crop(b, &unlimited(size), HD, HD);
        let r = c.rect(HD, HD);
        assert!((r.h - h).abs() < 1e-9, "{size:?}: {} vs {h}", r.h);
        assert!((1.0 / r.h - zoom).abs() < 0.01, "{size:?}: {}x", 1.0 / r.h);
        assert!(!c.limited_by_zoom, "{size:?}");
    }
}

#[test]
fn each_size_cuts_at_its_line_with_its_headroom() {
    let b = half_frame_person();
    let head = b.h / HEADS_PER_BODY;
    for out in [HD, 4.0 / 3.0, 1.0] {
        for size in SIZES {
            let p = unlimited(size);
            let r = person_crop(b, &p, HD, out).rect(HD, out);
            let cut = b.y + cut_heads(size).unwrap() * head;
            assert!((r.bottom() - cut).abs() < 1e-9, "{size:?} at {out}");
            assert!(((b.y - r.y) / r.h - p.headroom).abs() < 1e-9, "{size:?}");
            assert!((r.center().x - b.center().x).abs() < 1e-9, "{size:?}");
        }
    }
    // Full is the whole box, crown to feet.
    let full = person_crop(b, &unlimited(ShotSize::Full), HD, HD).rect(HD, HD);
    assert!((full.bottom() - b.bottom()).abs() < 1e-9);
}

#[test]
fn cut_lines_go_down_the_body_as_shots_loosen() {
    assert_eq!(cut_heads(ShotSize::Wide), None);
    assert_eq!(cut_heads(ShotSize::Full), Some(HEADS_PER_BODY));
    let cuts: Vec<f64> = SIZES.iter().map(|&s| cut_heads(s).unwrap()).collect();
    assert!(cuts.windows(2).all(|w| w[0] > w[1]), "{cuts:?}");
    let c = cut_heads(ShotSize::Close).unwrap();
    assert!((1.5..=1.75).contains(&c), "tops of the shoulders");
    // Every frame keeps the crown inside, below its headroom.
    let b = half_frame_person();
    for size in SIZES {
        let r = person_crop(b, &unlimited(size), HD, HD).rect(HD, HD);
        assert!(r.y < b.y && b.y < r.bottom(), "{size:?}");
    }
}

#[test]
fn the_zoom_ceiling_limits_close_shots_of_a_distant_person() {
    // With the standard 4x ceiling only Full reaches its size; the others
    // are drawn at the ceiling with the crown still at its headroom.
    let b = half_frame_person();
    for size in SIZES {
        let p = preset(size);
        let c = person_crop(b, &p, HD, HD);
        if size == ShotSize::Full {
            assert!(!c.limited_by_zoom);
            continue;
        }
        assert!(c.limited_by_zoom, "{size:?}");
        assert!((c.width - 1.0 / p.max_zoom).abs() < 1e-12, "{size:?}");
        let r = c.rect(HD, HD);
        assert!(((b.y - r.y) / r.h - p.headroom).abs() < 1e-9, "{size:?}");
    }
    // A closer person (twice as tall) gets a reachable Medium.
    let near = Rect::new(0.4, 0.02, 0.2, 0.96);
    assert!(!looks_truncated(&near, HD));
    let medium = person_crop(near, &preset(ShotSize::Medium), HD, HD);
    assert!(!medium.limited_by_zoom, "{}", medium.width);
}

#[test]
fn boxes_touching_an_edge_look_truncated_and_only_a_bottom_cut_assumes_a_head() {
    let cut_by_bottom = Rect::new(0.4, 0.5, 0.15, 0.5);
    let cut_by_top = Rect::new(0.4, 0.0, 0.15, 0.6);
    for b in [cut_by_bottom, cut_by_top] {
        assert!(looks_truncated(&b, HD), "{b:?}");
        assert_eq!(head_height(&b, HD), None);
    }
    // Cut by the bottom, crown in the picture, not squat: crown to waist,
    // so the head is b.h / 3 and each size but Full is cut on it.
    let b = cut_by_bottom;
    assert_eq!(assumed_visible_heads(&b, HD), Some(MAX_ASSUMED_HEADS));
    let head = b.h / 3.0;
    for size in [ShotSize::Medium, ShotSize::MediumClose, ShotSize::Close] {
        let p = unlimited(size);
        let r = person_crop(b, &p, HD, HD).rect(HD, HD);
        let cut = b.y + cut_heads(size).unwrap() * head;
        assert!((r.bottom() - cut).abs() < 1e-9, "{size:?}: {r:?}");
        assert!(((b.y - r.y) / r.h - p.headroom).abs() < 1e-9, "{size:?}");
        assert!((r.center().x - b.center().x).abs() < 1e-9, "{size:?}");
        assert_ne!(person_crop(b, &p, HD, HD), compose(&b, &p, HD, HD));
    }
    // Full's cut, the feet, is outside the box: the fill rule.
    let p = preset(ShotSize::Full);
    assert_eq!(person_crop(b, &p, HD, HD), compose(&b, &p, HD, HD));
    // Cut by the top, the crown is unknown: the fill rule at every size.
    assert_eq!(assumed_visible_heads(&cut_by_top, HD), None);
    for size in SIZES {
        let p = preset(size);
        assert_eq!(
            person_crop(cut_by_top, &p, HD, HD),
            compose(&cut_by_top, &p, HD, HD),
            "{size:?}"
        );
    }
    // Within the margin still counts; just outside it does not.
    assert!(looks_truncated(&Rect::new(0.4, 0.5, 0.1, 0.495), HD));
    assert!(!looks_truncated(&Rect::new(0.4, 0.4, 0.1, 0.5), HD));
}

#[test]
fn squat_boxes_look_truncated_judged_in_pixels() {
    // A person behind a drum kit: wider than a standing body allows.
    let seated = Rect::new(0.3, 0.4, 0.3, 0.3);
    assert!(looks_truncated(&seated, HD));
    let p = preset(ShotSize::Close);
    assert_eq!(
        person_crop(seated, &p, HD, HD),
        compose(&seated, &p, HD, HD)
    );
    // The same normalized box is 2:1 in pixels on a square source but only
    // 1.125:1 on a 16:9 one.
    let b = Rect::new(0.45, 0.3, 0.1, 0.2);
    assert!(!looks_truncated(&b, 1.0));
    assert!(looks_truncated(&b, HD));
    // An invalid source aspect is read as square.
    assert!(!looks_truncated(&b, f64::NAN));
    assert!(!looks_truncated(&b, -2.0));
}

/// The person of [`half_frame_person`] behind a lectern that hides them
/// from the waist down: a squat box, crown in the same place.
fn behind_lectern() -> Rect {
    Rect::new(0.45, 0.3, 0.1, 0.22)
}

/// `bbox` as a person whose shot remembers a head of `head`.
fn remembering(bbox: Rect, head: f64) -> SubjectGeometry {
    SubjectGeometry {
        remembered_head: Some(head),
        ..SubjectGeometry::new(bbox, Extent::Person)
    }
}

#[test]
fn a_remembered_head_survives_truncation() {
    let standing = half_frame_person();
    let hidden = behind_lectern();
    assert!(looks_truncated(&hidden, HD));
    let head = standing.h / HEADS_PER_BODY;
    for size in SIZES {
        let p = preset(size);
        assert_eq!(
            compose_subject(&remembering(hidden, head), &p, HD, HD),
            person_crop(standing, &p, HD, HD),
            "{size:?}"
        );
    }
    // An unusable memory is no memory: the fill rule.
    let p = unlimited(ShotSize::Close);
    for bad in [f64::NAN, 0.0, -0.1, f64::INFINITY] {
        assert_eq!(
            compose_subject(&remembering(hidden, bad), &p, HD, HD),
            compose(&hidden, &p, HD, HD),
            "{bad}"
        );
    }
}

#[test]
fn a_truncated_box_is_never_sized_below_what_it_shows() {
    // Walked toward the camera until the feet left the picture: 0.8 of the
    // body is visible, so the head is at least 0.8 / 7.5, whatever the
    // memory says.
    let cut_off = Rect::new(0.4, 0.2, 0.2, 0.8);
    assert!(looks_truncated(&cut_off, HD));
    let bound = visible_head(&cut_off).unwrap();
    assert!((bound - 0.8 / HEADS_PER_BODY).abs() < 1e-12);
    for size in SIZES {
        let p = unlimited(size);
        let stale = compose_subject(&remembering(cut_off, 0.05), &p, HD, HD);
        assert_eq!(
            stale,
            compose_person(&cut_off, bound, &p, HD, HD),
            "{size:?}"
        );
        let larger = compose_subject(&remembering(cut_off, 0.2), &p, HD, HD);
        assert_eq!(
            larger,
            compose_person(&cut_off, 0.2, &p, HD, HD),
            "{size:?}"
        );
    }
    let full = compose_subject(
        &remembering(cut_off, 0.05),
        &unlimited(ShotSize::Full),
        HD,
        HD,
    );
    assert!(full.rect(HD, HD).bottom() >= cut_off.bottom() - 1e-9);
    assert_eq!(visible_head(&Rect::new(0.4, 0.2, 0.2, 0.0)), None);
    assert_eq!(visible_head(&Rect::new(0.4, f64::NAN, 0.2, 0.5)), None);
}

#[test]
fn an_untruncated_box_ignores_the_memory() {
    // Full is the whole box, however stale the memory.
    let standing = half_frame_person();
    for head in [0.01, 0.05, 0.3] {
        for size in SIZES {
            let p = unlimited(size);
            assert_eq!(
                compose_subject(&remembering(standing, head), &p, HD, HD),
                person_crop(standing, &p, HD, HD),
                "{size:?} {head}"
            );
        }
    }
}

#[test]
fn the_wide_preset_ignores_the_basis() {
    let p = preset(ShotSize::Wide);
    let c = person_crop(half_frame_person(), &p, HD, HD);
    assert_eq!(c.rect(HD, HD), Rect::FULL);
    assert!(!c.limited_by_zoom);
    assert_eq!(
        compose_person(&half_frame_person(), 0.05, &p, HD, HD),
        compose(&half_frame_person(), &p, HD, HD)
    );
}

#[test]
fn degenerate_inputs_give_finite_crops() {
    let boxes = [
        half_frame_person(),
        Rect::new(f64::NAN, 0.3, 0.1, 0.5),
        Rect::new(0.4, f64::INFINITY, 0.1, 0.5),
        Rect::new(0.4, 0.3, f64::NEG_INFINITY, 0.5),
        Rect::new(0.4, 0.3, 0.1, f64::NAN),
        Rect::new(0.4, 0.3, 0.0, 0.0),
        Rect::new(0.4, 0.3, -0.1, -0.5),
        Rect::new(-3.0, 7.0, 0.1, 0.5),
        Rect::FULL,
        // Cut by the bottom: clear of the sides, at a side, below the
        // picture.
        Rect::new(0.45, 0.65, 0.1, 0.35),
        Rect::new(0.0, 0.65, 0.1, 0.35),
        Rect::new(0.4, 7.0, 0.1, 0.5),
    ];
    let aspects = [HD, 9.0 / 16.0, 0.0, -1.0, f64::NAN, f64::INFINITY];
    let mut presets: Vec<Preset> = SIZES.iter().map(|&s| preset(s)).collect();
    for (headroom, max_zoom) in [
        (f64::NAN, f64::NAN),
        (-1.0, 0.0),
        (2.0, f64::INFINITY),
        (1.0, -4.0),
    ] {
        presets.push(Preset {
            headroom,
            max_zoom,
            ..preset(ShotSize::Close)
        });
    }
    let heads = [None, Some(0.05), Some(f64::NAN), Some(0.0), Some(1e300)];
    let assumed = [
        None,
        Some(2.0),
        Some(f64::NAN),
        Some(0.0),
        Some(f64::INFINITY),
        Some(1e300),
    ];
    for b in boxes {
        for p in &presets {
            for src in aspects {
                for out in aspects {
                    for head_height in heads {
                        for assumed_heads in assumed {
                            for extent in [Extent::Person, Extent::Object] {
                                let g = SubjectGeometry {
                                    bbox: b,
                                    extent,
                                    remembered_head: head_height,
                                    assumed_heads,
                                };
                                let c = compose_subject(&g, p, src, out);
                                let r = c.rect(src, out);
                                let ok = [c.center.x, c.center.y, c.width, r.h]
                                    .iter()
                                    .all(|v| v.is_finite())
                                    && c.width > 0.0
                                    && c.width <= 1.0;
                                assert!(
                                    ok,
                                    "{b:?} {p:?} {src} {out} {head_height:?} \
                                     {assumed_heads:?}: {c:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn the_memory_seeds_smooths_and_only_rises_on_truncated_boxes() {
    let mut m = HeadMemory::new();
    assert_eq!(m.get(), None);
    // Truncated boxes cannot seed it.
    let cut_off = Rect::new(0.45, 0.3, 0.1, 0.7);
    assert!(looks_truncated(&cut_off, HD));
    assert_eq!(m.observe(&cut_off, HD, 5.0, 0.1), None);
    let head = 0.5 / HEADS_PER_BODY;
    assert_eq!(m.observe(&half_frame_person(), HD, 5.0, 0.1), Some(head));
    // Nor lower it once seeded.
    assert_eq!(m.observe(&behind_lectern(), HD, 5.0, 10.0), Some(head));
    // A truncated box showing more body raises it, smoothed.
    let mut raised = m;
    let up = raised.observe(&cut_off, HD, 5.0, 5.0).unwrap();
    assert!((up - (head + 0.7 / HEADS_PER_BODY) / 2.0).abs() < 1e-12);
    // One half-life moves it half way; a zero half-life follows at once.
    let taller = Rect::new(0.45, 0.2, 0.1, 0.6);
    let half = m.observe(&taller, HD, 5.0, 5.0).unwrap();
    assert!((half - (head + 0.6 / HEADS_PER_BODY) / 2.0).abs() < 1e-12);
    for instant in [0.0, -1.0, f64::NAN] {
        let mut m = HeadMemory::new();
        m.observe(&half_frame_person(), HD, instant, 0.1);
        let follows = m.observe(&taller, HD, instant, 0.1).unwrap();
        assert!((follows - 0.6 / HEADS_PER_BODY).abs() < 1e-12, "{instant}");
    }
    // An infinite half-life never forgets the first box.
    let mut m = HeadMemory::new();
    m.observe(&half_frame_person(), HD, f64::INFINITY, 0.1);
    assert_eq!(m.observe(&taller, HD, f64::INFINITY, 100.0), Some(head));
}
