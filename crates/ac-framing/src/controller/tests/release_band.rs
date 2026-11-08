//! Tests for the release band as a hysteresis on the shot's last reading
//! of the bottom edge, run through the framer at 30 Hz. A person seen
//! whole, then cut off by the bottom, then clear of the band for a second
//! and back with the feet just above the edge is framed as a whole body
//! again; straight back from the cut into the band, the head raised while
//! cut stays raised (a known limit, see [`crate::person`]).

use super::*;
use crate::person::{compose_person, head_height};

/// One frame at 30 Hz.
const DT: f64 = 1.0 / 30.0;

/// The person seen whole with the feet at `bottom`: 0.6 high, so a head of
/// 0.08 on the canon.
fn whole(bottom: f64) -> Rect {
    Rect::new(0.44, bottom - 0.60, 0.10, 0.60)
}

/// The person close to the camera and cut off by the bottom, 0.75
/// visible: the memory rises toward a head of 0.1.
fn cut() -> Rect {
    Rect::new(0.42, 0.25, 0.15, 0.75)
}

/// Whole with the feet at 0.95 for 5 s, cut by the bottom for 20 s, whole
/// at 0.96 (above the band) for `clear` seconds, then whole at 0.98 (in the
/// band).
fn steps_back(clear: f64) -> impl Fn(f64) -> Rect {
    move |t| {
        if t < 5.0 {
            whole(0.95)
        } else if t < 25.0 {
            cut()
        } else if t < 25.0 + clear {
            whole(0.96)
        } else {
            whole(0.98)
        }
    }
}

/// Runs a new standard `size` shot on the person boxes `boxes(t)` for
/// `seconds`, starting framed on the first box as the catalog does, and
/// returns the framer and its last ideal crop.
fn aim_through(
    size: ShotSize,
    seconds: f64,
    boxes: impl Fn(f64) -> Rect,
) -> (ShotFramer, Composition) {
    let p = standard(size);
    let first = SubjectGeometry::new(boxes(0.0), Extent::Person);
    let initial = compose_subject(&first, &p, A, A);
    let mut framer = ShotFramer::new(FramerConfig::default(), initial, A, A);
    let mut target = initial;
    for i in 0..(seconds / DT).round() as usize {
        target = framer.aim(&boxes(i as f64 * DT), Extent::Person, &p, DT);
        framer.step(Some(target), DT);
    }
    (framer, target)
}

#[test]
fn a_person_who_steps_clear_of_the_band_and_back_is_framed_whole_again() {
    // 3 and 7.5 heads of 0.08 below the crown, with 8% and 10% headroom.
    for (size, h) in [(ShotSize::Medium, 0.2609), (ShotSize::Full, 0.6667)] {
        let p = standard(size);
        let back = whole(0.98);
        let canon = compose_person(&back, head_height(&back, A).unwrap(), &p, A, A);
        assert!((canon.rect(A, A).h - h).abs() < 1e-3, "{size:?}");
        // A second after stepping back the box is framed on its own
        // proportions, while the memory is still coming down.
        let (framer, target) = aim_through(size, 27.0, steps_back(1.0));
        let head = framer.head_height().unwrap();
        assert!(head > 0.09, "{size:?}: {head}");
        assert_eq!(target, canon, "{size:?}");
        // A minute later the memory is back on the canon too.
        let (framer, target) = aim_through(size, 86.0, steps_back(1.0));
        let head = framer.head_height().unwrap();
        assert!((head - 0.08).abs() < 1e-4, "{size:?}: {head}");
        assert_eq!(target, canon, "{size:?}");
        // The same picture as a shot that only ever saw the person back.
        assert_eq!(aim_through(size, 1.0, |_| back).1, canon, "{size:?}");
    }
}

#[test]
fn straight_back_into_the_band_the_raised_head_stays_raised() {
    // Known limit: no box ends above the band after the cut, so the shot
    // still reads the person as cut by the bottom and never measures them.
    // 0.08 + 0.02 * (1 - 2^(-20 / 5)), framed about 23% looser.
    for size in [ShotSize::Medium, ShotSize::Full] {
        let (framer, target) = aim_through(size, 85.0, steps_back(0.0));
        let head = framer.head_height().unwrap();
        assert!((head - 0.09875).abs() < 1e-4, "{size:?}: {head}");
        let p = standard(size);
        assert_eq!(
            target,
            compose_person(&whole(0.98), head, &p, A, A),
            "{size:?}"
        );
    }
}
