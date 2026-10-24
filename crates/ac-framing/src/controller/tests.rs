//! Tests for the per-shot framing controller: dead zone, springs, loss,
//! head memory and the zoom-limit verdict.

use ac_core::ShotSize;

use super::*;

/// 16:9, in and out.
const A: f64 = 16.0 / 9.0;

/// A reachable crop at `(x, y)`, `width` wide.
fn at(x: f64, y: f64, width: f64) -> Composition {
    Composition {
        center: Point { x, y },
        width,
        limited_by_zoom: false,
        zoom_fit: f64::INFINITY,
    }
}

/// Steps `framer` toward `target` for `seconds` at 60 Hz.
fn run(framer: &mut ShotFramer, target: Composition, seconds: f64) -> Composition {
    let dt = 1.0 / 60.0;
    let mut out = framer.current();
    for _ in 0..(seconds / dt) as usize {
        out = framer.step(Some(target), dt);
    }
    out
}

#[test]
fn small_sway_inside_dead_zone_keeps_shot_static() {
    let start = at(0.5, 0.5, 0.5);
    let mut f = ShotFramer::new(FramerConfig::default(), start, A, A);
    let out = run(&mut f, at(0.5 + 0.03 * 0.5, 0.5, 0.5), 2.0);
    assert_eq!(out, start);
}

#[test]
fn large_move_is_followed_and_settles() {
    let mut f = ShotFramer::new(FramerConfig::default(), at(0.3, 0.5, 0.5), A, A);
    let out = run(&mut f, at(0.7, 0.5, 0.5), 6.0);
    assert!((out.center.x - 0.7).abs() < 0.5 * 0.08 * RELEASE_RATIO + 1e-9);
}

#[test]
fn pan_speed_never_exceeds_cap() {
    let config = FramerConfig::default();
    let mut f = ShotFramer::new(config, at(0.2, 0.5, 0.5), A, A);
    let dt = 1.0 / 60.0;
    let mut previous = f.current().center.x;
    for _ in 0..240 {
        let x = f.step(Some(at(0.8, 0.5, 0.5)), dt).center.x;
        assert!((x - previous).abs() / dt <= config.max_pan_speed * 0.5 + 1e-9);
        previous = x;
    }
}

#[test]
fn zoom_follows_in_log_space() {
    let mut f = ShotFramer::new(FramerConfig::default(), at(0.5, 0.5, 0.8), A, A);
    let out = run(&mut f, at(0.5, 0.5, 0.4), 10.0);
    assert!((out.width / 0.4 - 1.0).abs() < 0.03);
}

#[test]
fn missing_observations_hold_the_framing() {
    let start = at(0.4, 0.5, 0.5);
    let mut f = ShotFramer::new(FramerConfig::default(), start, A, A);
    for _ in 0..120 {
        f.step(None, 1.0 / 60.0);
    }
    assert_eq!(f.current(), start);
}

/// A whole person half the frame high, legs in view.
fn standing() -> Rect {
    Rect::new(0.45, 0.3, 0.1, 0.5)
}

/// The same person behind a lectern that hides them from the waist down:
/// the box ends at the lectern top, too squat for a standing body.
fn behind_lectern() -> Rect {
    Rect::new(0.45, 0.3, 0.1, 0.22)
}

/// A close preset that can reach its size on `standing()`.
fn close() -> Preset {
    Preset {
        max_zoom: 20.0,
        ..Preset::new("c", "Close", ShotSize::Close)
    }
}

/// A framer for `close()` at rest on its first target.
fn framer() -> ShotFramer {
    ShotFramer::new(FramerConfig::default(), at(0.5, 0.5, 1.0), A, A)
}

#[test]
fn person_boxes_seed_then_smooth_the_head_memory() {
    let mut f = framer();
    assert_eq!(f.head_height(), None);
    f.aim(&standing(), Extent::Person, &close(), 0.0);
    assert!((f.head_height().unwrap() - 0.5 / 7.5).abs() < 1e-12);
    // A taller box (walking toward the camera) moves the memory half
    // way in one half-life, in any number of steps.
    let taller = Rect::new(0.45, 0.2, 0.12, 0.7);
    let half_life = FramerConfig::default().head_half_life;
    for _ in 0..50 {
        f.aim(&taller, Extent::Person, &close(), half_life / 50.0);
    }
    let halfway = (0.5 / 7.5 + 0.7 / 7.5) / 2.0;
    assert!((f.head_height().unwrap() - halfway).abs() < 1e-9);
}

#[test]
fn a_lectern_hiding_the_legs_keeps_the_zoom() {
    let mut f = framer();
    let seen = f.aim(&standing(), Extent::Person, &close(), 1.0 / 30.0);
    assert!(crate::person::looks_truncated(&behind_lectern(), A));
    let hidden = f.aim(&behind_lectern(), Extent::Person, &close(), 1.0 / 30.0);
    assert!((hidden.width - seen.width).abs() < 1e-12);
    assert!((hidden.center.y - seen.center.y).abs() < 1e-12);
    assert!((f.head_height().unwrap() - 0.5 / 7.5).abs() < 1e-12);
    // A shot that never saw the legs has nothing to remember: fill rule.
    let mut fresh = framer();
    let fallback = fresh.aim(&behind_lectern(), Extent::Person, &close(), 1.0 / 30.0);
    assert_eq!(fresh.head_height(), None);
    assert_eq!(fallback, crate::compose(&behind_lectern(), &close(), A, A));
}

#[test]
fn objects_never_touch_the_memory() {
    let mut f = framer();
    let target = f.aim(&standing(), Extent::Object, &close(), 1.0 / 30.0);
    assert_eq!(f.head_height(), None);
    assert_eq!(target, crate::compose(&standing(), &close(), A, A));
}

#[test]
fn bad_time_steps_never_poison_the_memory() {
    let mut f = framer();
    f.aim(&standing(), Extent::Person, &close(), 1.0 / 30.0);
    let bigger = Rect::new(0.4, 0.1, 0.2, 0.8);
    for dt in [f64::NAN, -1.0, 0.0] {
        f.aim(&bigger, Extent::Person, &close(), dt);
        assert!((f.head_height().unwrap() - 0.5 / 7.5).abs() < 1e-12, "{dt}");
    }
    let target = f.aim(&bigger, Extent::Person, &close(), f64::INFINITY);
    assert!((f.head_height().unwrap() - 0.8 / 7.5).abs() < 1e-12);
    assert!(target.width.is_finite() && target.center.y.is_finite());
}

#[test]
fn the_zoom_flag_follows_the_target_and_holds_while_unobserved() {
    let mut f = framer();
    let capped = Preset::new("c", "Close", ShotSize::Close);
    let target = f.aim(&standing(), Extent::Person, &capped, 1.0 / 30.0);
    assert!(target.limited_by_zoom, "a close-up of this person needs 9x");
    assert!(f.step(Some(target), 1.0 / 30.0).limited_by_zoom);
    assert!(f.step(None, 1.0 / 30.0).limited_by_zoom, "held");
    let reachable = f.aim(&standing(), Extent::Person, &close(), 1.0 / 30.0);
    assert!(!reachable.limited_by_zoom);
    assert!(!f.step(Some(reachable), 1.0 / 30.0).limited_by_zoom);
}

/// The standard preset (4x ceiling) for `size`.
fn standard(size: ShotSize) -> Preset {
    Preset::new("p", "P", size)
}

/// Aims `f` at `bbox` once per 30 Hz frame for `seconds` under `preset`;
/// returns the last ideal crop.
fn aim_for(f: &mut ShotFramer, bbox: Rect, preset: &Preset, seconds: f64) -> Composition {
    let dt = 1.0 / 30.0;
    let mut out = f.aim(&bbox, Extent::Person, preset, dt);
    for _ in 1..(seconds / dt).round() as usize {
        out = f.aim(&bbox, Extent::Person, preset, dt);
    }
    out
}

#[test]
fn an_untruncated_box_is_framed_on_its_own_proportions() {
    // A step toward the camera, feet still in view: the memory lags, the
    // framing does not.
    let mut f = framer();
    let full = standard(ShotSize::Full);
    aim_for(&mut f, standing(), &full, 1.0);
    let nearer = Rect::new(0.44, 0.25, 0.12, 0.65);
    assert!(!crate::person::looks_truncated(&nearer, A));
    let target = f.aim(&nearer, Extent::Person, &full, 1.0 / 30.0);
    let r = target.rect(A, A);
    assert!(
        (r.bottom() - nearer.bottom()).abs() < 1e-9,
        "Full is the whole box: {r:?}"
    );
    assert!(f.head_height().unwrap() < 0.6 / 7.5, "the memory lags");
    // Every size is cut on the box's own head.
    let tight = f.aim(&nearer, Extent::Person, &close(), 1.0 / 30.0);
    let cut = nearer.y + 1.6 * nearer.h / 7.5;
    assert!((tight.rect(A, A).bottom() - cut).abs() < 1e-9);
}

/// A person walking toward the camera from `standing()` until their feet
/// leave the picture, `t` in `0..=1` of the way: crown 0.3 to 0.05, body
/// 0.5 to 1.2 high, clipped to the frame.
fn approaching(t: f64) -> Rect {
    let (y, h, w) = (0.3 - 0.25 * t, 0.5 + 0.7 * t, 0.1 + 0.14 * t);
    Rect::new(0.5 - w / 2.0, y, w, h.min(1.0 - y))
}

#[test]
fn a_person_who_walks_out_of_the_bottom_of_the_frame_keeps_a_true_scale() {
    let mut f = framer();
    let medium = standard(ShotSize::Medium);
    aim_for(&mut f, standing(), &medium, 3.0);
    let seeded = f.head_height().unwrap();
    assert!((seeded - 0.5 / 7.5).abs() < 1e-12);
    for i in 1..=90 {
        f.aim(
            &approaching(i as f64 / 90.0),
            Extent::Person,
            &medium,
            1.0 / 30.0,
        );
    }
    let close_by = approaching(1.0);
    assert!(crate::person::looks_truncated(&close_by, A));
    aim_for(&mut f, close_by, &medium, 3.0);
    // The visible body (0.95 high) bounds the head from below.
    let bound = close_by.h / 7.5;
    let head = f.head_height().unwrap();
    assert!(head > 0.08 && head <= bound + 1e-12, "{head} vs {bound}");
    let target = |f: &mut ShotFramer, p: &Preset| f.aim(&close_by, Extent::Person, p, 1.0 / 30.0);
    // Full reaches the bottom of the frame, not the waist.
    let full = target(&mut f, &standard(ShotSize::Full)).rect(A, A);
    assert!(full.bottom() >= 1.0 - 1e-9, "{full:?}");
    // Medium and Medium close are reachable and different shots.
    let m = target(&mut f, &medium);
    let mcu = target(&mut f, &standard(ShotSize::MediumClose));
    assert!(!m.limited_by_zoom && !mcu.limited_by_zoom);
    assert!(m.width > 1.2 * mcu.width, "{} vs {}", m.width, mcu.width);
    // An unlimited close-up never cuts above the shoulders of the visible
    // body.
    let tight = target(&mut f, &close()).rect(A, A);
    assert!(
        tight.bottom() >= close_by.y + 1.6 * bound - 1e-9,
        "{tight:?}"
    );
}

#[test]
fn truncated_boxes_raise_the_memory_but_never_lower_it() {
    let mut f = framer();
    let p = standard(ShotSize::Medium);
    aim_for(&mut f, standing(), &p, 1.0);
    let seeded = f.head_height().unwrap();
    // Smaller than the memory: a lectern; nothing moves.
    aim_for(&mut f, behind_lectern(), &p, 10.0);
    assert_eq!(f.head_height(), Some(seeded));
    // Larger: rises half way in a half-life, toward the bound only.
    let cut_off = Rect::new(0.4, 0.2, 0.2, 0.8);
    assert!(crate::person::looks_truncated(&cut_off, A));
    let half_life = FramerConfig::default().head_half_life;
    aim_for(&mut f, cut_off, &p, half_life);
    let halfway = (seeded + 0.8 / 7.5) / 2.0;
    assert!((f.head_height().unwrap() - halfway).abs() < 1e-3);
    aim_for(&mut f, cut_off, &p, 20.0 * half_life);
    assert!((f.head_height().unwrap() - 0.8 / 7.5).abs() < 1e-6);
}

/// An object box `h` high, near the 4x Close ceiling (`h / 0.92 = 0.25`).
fn guitar(h: f64) -> Rect {
    Rect::new(0.5, 0.4, 0.05, h)
}

#[test]
fn the_limited_verdict_does_not_flicker_near_the_ceiling() {
    let p = standard(ShotSize::Close);
    let aim = |f: &mut ShotFramer, h: f64| {
        let t = f.aim(&guitar(h), Extent::Object, &p, 1.0 / 30.0);
        (
            t.limited_by_zoom,
            f.step(Some(t), 1.0 / 30.0).limited_by_zoom,
        )
    };
    let mut f = framer();
    // Tracker scale steps either side of the ceiling: the exact verdict
    // flips every frame, the shot's does not.
    for i in 0..60 {
        let small = i % 2 == 0;
        let (exact, shown) = aim(&mut f, if small { 0.228 } else { 0.232 });
        assert_eq!(exact, small);
        assert!(!shown, "frame {i}");
    }
    // Clearly too small: limited, and it stays so through the same jitter.
    assert_eq!(aim(&mut f, 0.2), (true, true));
    for i in 0..60 {
        let (_, shown) = aim(&mut f, if i % 2 == 0 { 0.228 } else { 0.232 });
        assert!(shown, "frame {i}");
    }
    // Clearly reachable again: cleared.
    assert_eq!(aim(&mut f, 0.25), (false, false));
    assert!(f.current().zoom_fit > 1.05);
}
