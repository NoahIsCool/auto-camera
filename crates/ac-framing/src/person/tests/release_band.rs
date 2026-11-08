//! Tests for the release band as a hysteresis on a shot's last reading of
//! the bottom edge (the cut state of [`HeadMemory`]): with a count stored,
//! a box at the edge puts the shot in the cut state, a box the canon
//! measures ending above the band takes it out, and any other box (in the
//! band, or measuring nothing) keeps whichever came last. Also the limits
//! that remain, as the module docs list them.
//!
//! Most tests here use a half-life of zero, so each box moves the head all
//! the way at once and the gating shows in a single step.

use super::*;

/// One frame at 30 Hz.
const DT: f64 = 1.0 / 30.0;

/// A half-life with which the memory follows each box at once.
const INSTANT: f64 = 0.0;

/// The standard head half-life, in seconds.
const HALF_LIFE: f64 = 5.0;

/// The sermon speaker: cut off by the bottom at about the knees, crown in
/// the picture, clear of both sides, so it seeds an assumed count of 3.
fn cut() -> Rect {
    Rect::new(0.45, 0.65, 0.10, 0.35)
}

/// A person close to the camera, cut off by the bottom, 0.75 visible: its
/// visible head bound is 0.1 and it seeds an assumed count of 3.
fn cut_close() -> Rect {
    Rect::new(0.42, 0.25, 0.15, 0.75)
}

/// A whole person `h` high with the feet at `bottom`.
fn whole(h: f64, bottom: f64) -> Rect {
    Rect::new(0.44, bottom - h, 0.10, h)
}

/// True when the remembered head `head` is `expected`, up to rounding.
fn is_head(head: Option<f64>, expected: f64) -> bool {
    head.is_some_and(|h| (h - expected).abs() < 1e-12)
}

/// `bbox` as the person of the shot whose memory is `m`, as the framer
/// hands it to [`compose_subject`].
fn seen_by(bbox: Rect, m: &HeadMemory) -> SubjectGeometry {
    SubjectGeometry {
        remembered_head: m.get(),
        assumed_heads: m.assumed_heads_in_force(),
        ..SubjectGeometry::new(bbox, Extent::Person)
    }
}

#[test]
fn the_cut_state_follows_the_last_reading_of_the_bottom_edge() {
    let (tall, short) = (whole(0.75, 0.95), whole(0.6, 0.98));
    assert!(!in_release_band(&tall) && in_release_band(&short));
    assert!(is_head(head_height(&short, HD), 0.08));
    let mut m = HeadMemory::new();
    assert!(is_head(m.observe(&tall, HD, INSTANT, DT), 0.1));
    // At the edge: the count is stored and in force.
    m.observe(&cut(), HD, INSTANT, DT);
    assert_eq!(m.assumed_heads_in_force(), Some(3.0));
    // In the band: still cut, so not measured and the head stays.
    assert!(is_head(m.observe(&short, HD, INSTANT, DT), 0.1));
    assert_eq!(m.assumed_heads_in_force(), Some(3.0));
    // Above the band: out of the cut state, the count kept but not in
    // force, and the box measured.
    let clear = whole(0.75, 0.96);
    assert!(is_head(m.observe(&clear, HD, INSTANT, DT), 0.1));
    assert_eq!(m.assumed_heads(), Some(3.0));
    assert_eq!(m.assumed_heads_in_force(), None);
    // In the band again: measured now, so the head comes down.
    assert!(is_head(m.observe(&short, HD, INSTANT, DT), 0.08));
    assert_eq!(m.assumed_heads_in_force(), None);
    // Back at the edge, also at a side edge with no count of its own: in
    // the cut state again, and the band holds.
    m.observe(&tall, HD, INSTANT, DT);
    m.observe(&Rect::new(0.0, 0.65, 0.1, 0.35), HD, INSTANT, DT);
    assert_eq!(m.assumed_heads_in_force(), Some(3.0));
    assert!(is_head(m.observe(&short, HD, INSTANT, DT), 0.1));
}

#[test]
fn the_framer_s_view_follows_the_cut_state() {
    let short = whole(0.6, 0.98);
    let mut m = HeadMemory::new();
    m.observe(&cut(), HD, HALF_LIFE, DT);
    m.observe(&short, HD, HALF_LIFE, DT);
    // In the cut state the band box is framed on the stored count.
    for size in [ShotSize::Medium, ShotSize::MediumClose, ShotSize::Close] {
        let p = unlimited(size);
        assert_eq!(
            compose_subject(&seen_by(short, &m), &p, HD, HD),
            compose_assumed(&short, 3.0, &p, HD, HD),
            "{size:?}"
        );
    }
    // Out of it, the same box is a whole body, Full the whole box: measured
    // on its own proportions (a head of 0.08), not sized by the head of
    // 0.1 remembered from the taller box that ended the cut state.
    m.observe(&whole(0.75, 0.96), HD, HALF_LIFE, DT);
    m.observe(&short, HD, HALF_LIFE, DT);
    for size in SIZES {
        let p = unlimited(size);
        assert_eq!(
            compose_subject(&seen_by(short, &m), &p, HD, HD),
            compose_person(&short, 0.6 / HEADS_PER_BODY, &p, HD, HD),
            "{size:?}"
        );
    }
}

#[test]
fn boxes_that_say_nothing_about_the_edge_keep_the_cut_state() {
    let mut m = HeadMemory::new();
    // Without a stored count a box at the edge puts nothing in force and
    // leaves the memory as it was.
    m.observe(&Rect::new(0.0, 0.65, 0.1, 0.35), HD, HALF_LIFE, DT);
    m.observe(&Rect::new(0.3, 0.005, 0.4, 0.995), HD, HALF_LIFE, DT);
    assert_eq!(m, HeadMemory::new());
    assert_eq!(m.assumed_heads_in_force(), None);
    m.observe(&cut(), HD, HALF_LIFE, DT);
    let held = m;
    // Empty, negative or non-finite boxes, and boxes the canon cannot
    // measure, change nothing, even ones that end above the band: a squat
    // box (behind a lectern) and one cut by the top.
    for b in [
        Rect::new(0.45, 0.3, 0.0, 0.3),
        Rect::new(0.45, 0.3, 0.1, 0.0),
        Rect::new(0.45, 0.3, -0.1, 0.3),
        Rect::new(f64::NAN, 0.3, 0.1, 0.3),
        Rect::new(0.45, f64::NAN, 0.1, 0.3),
        Rect::new(0.45, 0.3, 0.1, f64::INFINITY),
        Rect::new(0.45, 0.3, 0.1, 0.22),
        Rect::new(0.4, 0.0, 0.2, 0.9),
    ] {
        m.observe(&b, HD, HALF_LIFE, DT);
        assert_eq!(m, held, "{b:?}");
    }
    assert_eq!(m.assumed_heads_in_force(), Some(3.0));
    // So a box in the band after a lectern still reads as cut by the
    // bottom and seeds nothing.
    assert_eq!(
        m.observe(&Rect::new(0.45, 0.65, 0.1, 0.335), HD, HALF_LIFE, DT),
        None
    );
    assert_eq!(m.assumed_heads_in_force(), Some(3.0));
}

#[test]
fn the_band_follows_the_edge_not_the_body() {
    // Known limits. After one box at the edge, a whole body whose feet
    // stay in the band reads as cut by the bottom: with no head remembered
    // it is framed crown to waist, and its Full falls to the fill rule.
    let back = whole(0.6, 0.98);
    let mut m = HeadMemory::new();
    m.observe(&cut(), HD, HALF_LIFE, DT);
    for _ in 0..1800 {
        m.observe(&back, HD, HALF_LIFE, DT);
    }
    assert_eq!(m.get(), None);
    let medium = unlimited(ShotSize::Medium);
    assert_eq!(
        compose_subject(&seen_by(back, &m), &medium, HD, HD),
        compose_assumed(&back, 3.0, &medium, HD, HD)
    );
    let full = preset(ShotSize::Full);
    assert_eq!(
        compose_subject(&seen_by(back, &m), &full, HD, HD),
        compose(&back, &full, HD, HD)
    );
    // A head raised while the person was cut stays raised until a box the
    // canon measures ends above the band.
    let mut m = HeadMemory::new();
    m.observe(&whole(0.6, 0.95), HD, INSTANT, DT);
    assert!(is_head(m.observe(&cut_close(), HD, INSTANT, DT), 0.1));
    for _ in 0..100 {
        assert!(is_head(m.observe(&back, HD, INSTANT, DT), 0.1));
    }
    m.observe(&whole(0.6, 0.96), HD, INSTANT, DT);
    assert!(is_head(m.get(), 0.08));
}
