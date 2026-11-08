//! Tests for person boxes cut by the bottom of the source: the assumed
//! visible head count, its bounds and continuity, its place after measured
//! and remembered heads, the stored count that frames every box cut by the
//! bottom, the release band, and the memory slot that smooths the count
//! without ever feeding the measured head.
//!
//! The reference subject is the sermon speaker of the Mac run: a box
//! `(0.45, 0.65, 0.10, 0.35)` on a 16:9 source, cut off at about the knees,
//! for which the fill rule framed the whole visible body at every size.

use super::*;

/// The sermon speaker: a person box cut by the bottom edge, crown in the
/// picture, clear of both sides, pixel aspect about 1.97.
fn sermon() -> Rect {
    Rect::new(0.45, 0.65, 0.10, 0.35)
}

/// `bbox` as a person whose shot holds the assumed head count `heads`.
fn assuming(bbox: Rect, heads: f64) -> SubjectGeometry {
    SubjectGeometry {
        assumed_heads: Some(heads),
        ..SubjectGeometry::new(bbox, Extent::Person)
    }
}

/// Where the crown sits in `r`, as a fraction of its height from the top.
fn crown_headroom(bbox: &Rect, r: &Rect) -> f64 {
    (bbox.y - r.y) / r.h
}

#[test]
fn the_sermon_speaker_gets_a_limited_close_and_distinct_tighter_sizes() {
    let b = sermon();
    assert!(looks_truncated(&b, HD));
    assert_eq!(head_height(&b, HD), None);
    assert_eq!(assumed_visible_heads(&b, HD), Some(3.0));
    // Close needs 5.2x: drawn at the 4x ceiling, and said so.
    let close = person_crop(b, &preset(ShotSize::Close), HD, HD);
    assert!(close.limited_by_zoom);
    assert!((close.width - 0.25).abs() < 1e-12, "{}", close.width);
    assert!((close.zoom_fit - 0.77).abs() < 0.01, "{}", close.zoom_fit);
    // (size, frame height): 2.3 and 3 heads of b.h / 3 below the crown.
    for (size, h) in [
        (ShotSize::Close, 0.25),
        (ShotSize::MediumClose, 0.2855),
        (ShotSize::Medium, 0.3804),
    ] {
        let p = preset(size);
        let c = person_crop(b, &p, HD, HD);
        let r = c.rect(HD, HD);
        assert!((r.h - h).abs() < 1e-3, "{size:?}: {r:?}");
        assert!(
            (crown_headroom(&b, &r) - p.headroom).abs() < 1e-9,
            "{size:?}: crown at {}",
            crown_headroom(&b, &r)
        );
        assert!(Rect::FULL.contains_rect(&r), "{size:?}: {r:?}");
        if size != ShotSize::Close {
            assert!(!c.limited_by_zoom, "{size:?}");
        }
    }
    // Full is the fill rule, unchanged: 0.875 high, pushed up into the
    // source.
    let p = preset(ShotSize::Full);
    let full = person_crop(b, &p, HD, HD);
    assert_eq!(full, compose(&b, &p, HD, HD));
    assert!((full.rect(HD, HD).h - 0.875).abs() < 1e-9);
}

#[test]
fn the_assumed_count_is_bounded_and_never_shows_less_than_the_box() {
    assert!((SHOULDER_HEADS - 1.875).abs() < 1e-12);
    assert_eq!(Some(MIN_ASSUMED_HEADS), cut_heads(ShotSize::Close));
    assert_eq!(Some(MAX_ASSUMED_HEADS), cut_heads(ShotSize::Medium));
    for src in [HD, 4.0 / 3.0, 1.0, 9.0 / 16.0] {
        for i in 1..=19 {
            let h = 0.05 * i as f64;
            let mut previous = f64::INFINITY;
            for j in 1..=90 {
                let w = 0.01 * j as f64;
                let b = Rect::new(0.5 - w / 2.0, 1.0 - h, w, h);
                let v = assumed_visible_heads(&b, src).unwrap();
                assert!(
                    (MIN_ASSUMED_HEADS..=MAX_ASSUMED_HEADS).contains(&v),
                    "{b:?} at {src}: {v}"
                );
                assert!(v <= previous, "wider never reads as more heads");
                previous = v;
                // The assumed head is never below what the box shows.
                assert!(b.h / v >= visible_head(&b).unwrap(), "{b:?}");
            }
        }
    }
    // Tall in pixels: the cap binds, crown to waist.
    assert_eq!(assumed_visible_heads(&sermon(), HD), Some(3.0));
    // Squat: head and shoulders at the floor.
    let squat = Rect::new(0.2, 0.8, 0.6, 0.2);
    assert_eq!(assumed_visible_heads(&squat, HD), Some(1.6));
}

#[test]
fn the_assumed_count_is_continuous_across_the_squat_threshold() {
    // Boxes just either side of MIN_STANDING_ASPECT in pixels.
    let h = 0.35;
    let at = |pixel_aspect: f64| {
        let w = h / (pixel_aspect * HD);
        Rect::new(0.5 - w / 2.0, 1.0 - h, w, h)
    };
    let below = at(MIN_STANDING_ASPECT * (1.0 - 1e-3));
    let above = at(MIN_STANDING_ASPECT * (1.0 + 1e-3));
    let (lo, hi) = (
        assumed_visible_heads(&below, HD).unwrap(),
        assumed_visible_heads(&above, HD).unwrap(),
    );
    assert!(hi / lo - 1.0 < 0.01, "{lo} vs {hi}");
    assert!((hi - MAX_ASSUMED_HEADS).abs() < 1e-12);
    for size in [ShotSize::Medium, ShotSize::MediumClose, ShotSize::Close] {
        let p = unlimited(size);
        let a = person_crop(below, &p, HD, HD).rect(HD, HD).h;
        let b = person_crop(above, &p, HD, HD).rect(HD, HD).h;
        assert!((a / b - 1.0).abs() < 0.01, "{size:?}: {a} vs {b}");
    }
}

#[test]
fn only_a_box_cut_by_the_bottom_showing_its_crown_and_clear_of_the_sides_is_guessed() {
    let none = [
        // Cut by the top as well: the crown is unknown.
        Rect::new(0.45, 0.005, 0.2, 0.995),
        // Not cut by the bottom.
        Rect::new(0.45, 0.3, 0.1, 0.5),
        Rect::new(0.45, 0.65, 0.1, 0.33),
        // Cut by a side edge: the width is not the body's.
        Rect::new(0.0, 0.65, 0.1, 0.35),
        Rect::new(0.9, 0.65, 0.1, 0.35),
        // Empty or not finite.
        Rect::new(0.45, 0.65, 0.0, 0.35),
        Rect::new(0.45, 1.0, 0.1, 0.0),
        Rect::new(0.45, 0.65, -0.1, 0.35),
        Rect::new(f64::NAN, 0.65, 0.1, 0.35),
        Rect::new(0.45, f64::NAN, 0.1, 0.35),
        Rect::new(0.45, 0.65, f64::INFINITY, 0.35),
        Rect::new(0.45, 0.65, 0.1, f64::INFINITY),
    ];
    for b in none {
        assert_eq!(assumed_visible_heads(&b, HD), None, "{b:?}");
    }
    for aspect in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(assumed_visible_heads(&sermon(), aspect), None, "{aspect}");
    }
    // Within the edge margin still counts.
    assert!(assumed_visible_heads(&Rect::new(0.45, 0.65, 0.1, 0.345), HD).is_some());
    assert!(assumed_visible_heads(&Rect::new(0.02, 0.65, 0.1, 0.35), HD).is_some());
}

#[test]
fn measured_and_remembered_heads_beat_the_assumption() {
    // The lectern case is unchanged: a remembered head sizes a squat box,
    // whatever count the shot holds.
    let standing = half_frame_person();
    let hidden = Rect::new(0.45, 0.3, 0.1, 0.22);
    let head = standing.h / HEADS_PER_BODY;
    for size in SIZES {
        let p = preset(size);
        let g = SubjectGeometry {
            remembered_head: Some(head),
            assumed_heads: Some(1.6),
            ..SubjectGeometry::new(hidden, Extent::Person)
        };
        assert_eq!(
            compose_subject(&g, &p, HD, HD),
            person_crop(standing, &p, HD, HD),
            "{size:?}"
        );
    }
    // A remembered head beats the assumption on a box cut by the bottom.
    let b = sermon();
    for size in SIZES {
        let p = unlimited(size);
        let g = SubjectGeometry {
            remembered_head: Some(0.05),
            ..SubjectGeometry::new(b, Extent::Person)
        };
        let both = SubjectGeometry {
            assumed_heads: Some(3.0),
            ..g
        };
        let expected = compose_person(&b, 0.05, &p, HD, HD);
        assert_eq!(compose_subject(&g, &p, HD, HD), expected, "{size:?}");
        assert_eq!(compose_subject(&both, &p, HD, HD), expected, "{size:?}");
    }
    // An untruncated box ignores both.
    for size in SIZES {
        let p = unlimited(size);
        let g = SubjectGeometry {
            remembered_head: Some(0.01),
            assumed_heads: Some(1.6),
            ..SubjectGeometry::new(standing, Extent::Person)
        };
        assert_eq!(
            compose_subject(&g, &p, HD, HD),
            person_crop(standing, &p, HD, HD),
            "{size:?}"
        );
    }
    // A squat box clear of the bottom keeps the fill rule, count or not.
    let p = preset(ShotSize::Close);
    assert!(looks_truncated(&hidden, HD));
    assert_eq!(
        person_crop(hidden, &p, HD, HD),
        compose(&hidden, &p, HD, HD)
    );
    assert_eq!(
        compose_subject(&assuming(hidden, 3.0), &p, HD, HD),
        compose(&hidden, &p, HD, HD)
    );
}

#[test]
fn a_stored_count_frames_every_box_cut_by_the_bottom() {
    let tight = [ShotSize::Medium, ShotSize::MediumClose, ShotSize::Close];
    // At a side edge and with the crown at the top: no count of their
    // own, so the fill rule, unless the shot holds one.
    let at_side = Rect::new(0.0, 0.65, 0.1, 0.35);
    let crown_at_top = Rect::new(0.3, 0.005, 0.4, 0.995);
    for b in [at_side, crown_at_top] {
        for size in tight {
            let p = preset(size);
            assert_eq!(person_crop(b, &p, HD, HD), compose(&b, &p, HD, HD), "{b:?}");
            assert_eq!(
                compose_subject(&assuming(b, 2.6), &p, HD, HD),
                compose_assumed(&b, 2.6, &p, HD, HD),
                "{b:?} {size:?}"
            );
        }
    }
    // The stored count wins over the box's own.
    for size in tight {
        let p = unlimited(size);
        assert_eq!(
            compose_subject(&assuming(sermon(), 2.5), &p, HD, HD),
            compose_assumed(&sermon(), 2.5, &p, HD, HD),
            "{size:?}"
        );
    }
    // A box just above the edge: measured on the canon, unless the shot
    // holds a count; then it is still read as cut by the bottom.
    let lifted = Rect::new(0.45, 0.65, 0.1, 0.335);
    assert!(!looks_truncated(&lifted, HD));
    assert!(in_release_band(&lifted));
    assert_eq!(measured_head(&lifted, HD, None), head_height(&lifted, HD));
    assert_eq!(measured_head(&lifted, HD, Some(3.0)), None);
    assert_eq!(
        measured_head(&lifted, HD, Some(f64::NAN)),
        head_height(&lifted, HD)
    );
    for size in tight {
        let p = preset(size);
        assert_eq!(
            compose_subject(&assuming(lifted, 3.0), &p, HD, HD),
            compose_assumed(&lifted, 3.0, &p, HD, HD),
            "{size:?}"
        );
        // Inside the framer's 7% zoom dead zone of the box at the edge, so
        // the camera does not move.
        let h = |b| {
            compose_subject(&assuming(b, 3.0), &p, HD, HD)
                .rect(HD, HD)
                .h
        };
        assert!(h(lifted) / h(sermon()) > 0.95, "{size:?}");
    }
    // Clear of the band: measured again.
    let clear = Rect::new(0.45, 0.6, 0.1, 0.36);
    assert!(!in_release_band(&clear));
    let p = unlimited(ShotSize::Close);
    assert_eq!(
        compose_subject(&assuming(clear, 3.0), &p, HD, HD),
        person_crop(clear, &p, HD, HD)
    );
    // Full keeps the fill rule with a count too.
    let p = preset(ShotSize::Full);
    assert_eq!(
        compose_subject(&assuming(sermon(), 2.0), &p, HD, HD),
        compose(&sermon(), &p, HD, HD)
    );
}

#[test]
fn an_assumed_frame_never_reaches_below_the_box() {
    // A squat box cut by the bottom (arms out): with fewer than three
    // heads assumed, the Medium cut would fall below the picture.
    let b = Rect::new(0.35, 0.65, 0.13, 0.35);
    let v = assumed_visible_heads(&b, HD).unwrap();
    assert!(v < MAX_ASSUMED_HEADS, "{v}");
    for size in [ShotSize::Medium, ShotSize::MediumClose, ShotSize::Close] {
        let p = unlimited(size);
        let r = person_crop(b, &p, HD, HD).rect(HD, HD);
        assert!(r.bottom() <= b.bottom() + 1e-9, "{size:?}: {r:?}");
        assert!(
            (crown_headroom(&b, &r) - p.headroom).abs() < 1e-9,
            "{size:?}: crown at {}",
            crown_headroom(&b, &r)
        );
    }
    let medium = person_crop(b, &unlimited(ShotSize::Medium), HD, HD).rect(HD, HD);
    assert!((medium.h - b.h / 0.92).abs() < 1e-9, "the visible box");
}

#[test]
fn the_assumed_count_seeds_smooths_and_never_touches_the_head() {
    let mut m = HeadMemory::new();
    assert_eq!(m.assumed_heads(), None);
    // Untruncated, side-cut and top-cut boxes do not seed it.
    m.observe(&Rect::new(0.0, 0.65, 0.1, 0.35), HD, 5.0, 0.1);
    m.observe(&Rect::new(0.3, 0.005, 0.4, 0.995), HD, 5.0, 0.1);
    assert_eq!(m, HeadMemory::new());
    // The first box cut by the bottom seeds the count, not the head.
    assert_eq!(m.observe(&sermon(), HD, 5.0, 0.1), None);
    assert_eq!(m.assumed_heads(), Some(3.0));
    // Arms out for one half-life: half way to the wider box's count.
    let wide = Rect::new(0.4, 0.65, 0.2, 0.35);
    let target = assumed_visible_heads(&wide, HD).unwrap();
    assert!(target < 2.0, "{target}");
    let mut half = m;
    half.observe(&wide, HD, 5.0, 5.0);
    let expected = (3.0 + target) / 2.0;
    assert!((half.assumed_heads().unwrap() - expected).abs() < 1e-12);
    assert_eq!(half.get(), None, "never seeds the head");
    // Boxes that cannot update it leave it alone.
    let mut held = half;
    for b in [
        half_frame_person(),
        Rect::new(0.0, 0.65, 0.1, 0.35),
        Rect::new(0.3, 0.005, 0.4, 0.995),
    ] {
        held.observe(&b, HD, 5.0, 5.0);
        assert_eq!(held.assumed_heads(), half.assumed_heads(), "{b:?}");
    }
    // A time step that is not positive moves nothing.
    let mut still = half;
    for dt in [0.0, -1.0, f64::NAN] {
        still.observe(&sermon(), HD, 5.0, dt);
        assert_eq!(still.assumed_heads(), half.assumed_heads(), "{dt}");
    }
    // An untruncated box seeds the head and leaves the count alone.
    let mut seeded = HeadMemory::new();
    seeded.observe(&half_frame_person(), HD, 5.0, 0.1);
    assert_eq!(seeded.assumed_heads(), None);
    let head = seeded.get().unwrap();
    // Boxes cut by the bottom then seed the count and never lower the
    // head, however long they last.
    for _ in 0..100 {
        seeded.observe(&sermon(), HD, 5.0, 1.0);
    }
    assert_eq!(seeded.get(), Some(head));
    assert_eq!(seeded.assumed_heads(), Some(3.0));
}

#[test]
fn a_box_just_above_the_edge_seeds_the_head_only_before_a_count_is_held() {
    let lifted = Rect::new(0.45, 0.65, 0.1, 0.335);
    // With a count held: read as cut by the bottom, nothing measured.
    let mut m = HeadMemory::new();
    m.observe(&sermon(), HD, 5.0, 1.0 / 30.0);
    assert_eq!(m.observe(&lifted, HD, 5.0, 1.0 / 30.0), None);
    assert_eq!(m.assumed_heads(), Some(3.0), "nor does it move the count");
    // Clear of the release band: measured on the canon again.
    let clear = Rect::new(0.45, 0.6, 0.1, 0.36);
    assert_eq!(
        m.observe(&clear, HD, 5.0, 1.0 / 30.0),
        head_height(&clear, HD)
    );
    // The first-box race, a known limit: a shot whose first box is lifted
    // seeds the head before any count exists.
    let mut first = HeadMemory::new();
    assert_eq!(
        first.observe(&lifted, HD, 5.0, 1.0 / 30.0),
        head_height(&lifted, HD)
    );
}
