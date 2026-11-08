//! Tests for shots of a person cut off by the bottom of the source, run
//! through the framer for a minute at 30 Hz: the smoothed assumed head
//! count keeps the crop and the "(limited)" verdict steady through
//! gestures, a walk to a side edge, a crown bobbing at the top edge and a
//! single box that ends just above the bottom edge, also when that box
//! comes as the speaker steps out from behind a lectern (the lectern
//! itself is framed on the fill rule; only the state after it is checked).

use super::*;

/// One frame at 30 Hz.
const DT: f64 = 1.0 / 30.0;

/// The sizes a box cut by the bottom is framed at on an assumed head.
const TIGHT: [ShotSize; 3] = [ShotSize::Close, ShotSize::MediumClose, ShotSize::Medium];

/// The sermon speaker of the Mac run, cut off at about the knees, at
/// horizontal position `x`, `w` wide.
fn speaker(x: f64, w: f64) -> Rect {
    Rect::new(x, 0.65, w, 0.35)
}

/// What a shot did over a run.
struct Trace {
    /// Crop height per frame.
    heights: Vec<f64>,
    /// The shot's "(limited)" verdict per frame.
    limited: Vec<bool>,
    /// The framer at the end of the run.
    framer: ShotFramer,
}

impl Trace {
    /// Smallest and largest crop height from `from` seconds on.
    fn height_range(&self, from: f64) -> (f64, f64) {
        let tail = &self.heights[(from / DT).round() as usize..];
        let lo = tail.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = tail.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (lo, hi)
    }

    /// How many times the verdict changed from `from` seconds on.
    fn flips(&self, from: f64) -> usize {
        let tail = &self.limited[(from / DT).round() as usize..];
        tail.windows(2).filter(|w| w[0] != w[1]).count()
    }

    /// The last crop height.
    fn last_height(&self) -> f64 {
        *self.heights.last().unwrap()
    }
}

/// Runs a new standard `size` shot on the person boxes `boxes(t)` for
/// `seconds`, starting framed on the first box as the catalog does.
fn run_shot(size: ShotSize, seconds: f64, boxes: impl Fn(f64) -> Rect) -> Trace {
    let p = standard(size);
    let first = SubjectGeometry::new(boxes(0.0), Extent::Person);
    let initial = compose_subject(&first, &p, A, A);
    let mut framer = ShotFramer::new(FramerConfig::default(), initial, A, A);
    let mut heights = vec![];
    let mut limited = vec![];
    for i in 0..(seconds / DT).round() as usize {
        let target = framer.aim(&boxes(i as f64 * DT), Extent::Person, &p, DT);
        let c = framer.step(Some(target), DT);
        heights.push(c.rect(A, A).h);
        limited.push(c.limited_by_zoom);
    }
    Trace {
        heights,
        limited,
        framer,
    }
}

#[test]
fn gestures_neither_pump_the_zoom_nor_flip_the_limited_label() {
    // Arms out for 1 s every 4 s doubles the box width. Read frame by
    // frame, that is 1.85 heads instead of 3 and a reachable Close every
    // gesture; smoothed, the Close stays limited at the ceiling.
    let gesturing = |t: f64| {
        if t % 4.0 >= 3.0 {
            speaker(0.40, 0.20)
        } else {
            speaker(0.45, 0.10)
        }
    };
    for size in TIGHT {
        let trace = run_shot(size, 60.0, gesturing);
        let (lo, hi) = trace.height_range(30.0);
        assert!(hi / lo - 1.0 < 0.02, "{size:?}: {lo}..{hi}");
        assert_eq!(trace.flips(0.0), 0, "{size:?}");
        assert_eq!(trace.limited[0], size == ShotSize::Close, "{size:?}");
        let (low, high) = match size {
            ShotSize::Close => (0.25, 0.25),
            ShotSize::MediumClose => (0.28, 0.34),
            _ => (0.375, 0.385),
        };
        assert!(
            (low - 1e-9..=high + 1e-9).contains(&lo) && hi <= high + 1e-9,
            "{size:?}: {lo}..{hi}"
        );
    }
}

#[test]
fn a_speaker_pacing_to_a_side_edge_keeps_every_shot_steady() {
    // Cut by the bottom, walking to the left edge (5 s), staying there
    // (5 s), walking back (5 s) and standing (5 s), three times. At the
    // edge the box has no count of its own; the stored one frames it.
    let pacing = |t: f64| {
        let phase = t % 20.0;
        let x = match phase {
            p if p < 5.0 => 0.45 * (1.0 - p / 5.0),
            p if p < 10.0 => 0.0,
            p if p < 15.0 => 0.45 * (p - 10.0) / 5.0,
            _ => 0.45,
        };
        speaker(x, 0.10)
    };
    for (size, h) in [
        (ShotSize::Close, 0.25),
        (ShotSize::MediumClose, 0.2855),
        (ShotSize::Medium, 0.3804),
    ] {
        let trace = run_shot(size, 60.0, pacing);
        let (lo, hi) = trace.height_range(0.0);
        assert!(hi - lo < 1e-9, "{size:?}: {lo}..{hi}");
        assert!((lo - h).abs() < 1e-3, "{size:?}: {lo}");
        assert_eq!(trace.flips(0.0), 0, "{size:?}");
    }
}

#[test]
fn a_crown_bobbing_at_the_top_edge_keeps_a_tight_source_steady() {
    // A chest-up camera: the box fills the picture below the crown, which
    // bobs across the 1% top margin (0.005 to 0.03, a 2 s cycle). When
    // the crown touches the top the box has no count of its own; the
    // stored one frames it.
    let bobbing = |t: f64| {
        let y = 0.0175 + 0.0125 * (t * std::f64::consts::PI).sin();
        Rect::new(0.3, y, 0.40, 1.0 - y)
    };
    let close = run_shot(ShotSize::Close, 60.0, bobbing);
    let (lo, hi) = close.height_range(30.0);
    assert!(hi / lo - 1.0 < 0.02, "{lo}..{hi}");
    assert!(
        (0.6..0.65).contains(&lo),
        "head and shoulders, not 1.0: {lo}"
    );
    let chest = run_shot(ShotSize::MediumClose, 60.0, bobbing);
    let (lo, hi) = chest.height_range(30.0);
    assert!(hi / lo - 1.0 < 0.02, "{lo}..{hi}");
    assert!(hi < 0.95, "{hi}");
}

#[test]
fn one_box_just_above_the_bottom_edge_does_not_collapse_the_shots() {
    // A single box ending at 0.985 (11 px at 720p) at t = 10 s would be
    // measured as a whole body, b.h / 7.5, and Medium close and Medium
    // would collapse onto the Close at the ceiling.
    let lifted_once = |t: f64| {
        if (t - 10.0).abs() < DT / 2.0 {
            Rect::new(0.45, 0.65, 0.10, 0.335)
        } else {
            speaker(0.45, 0.10)
        }
    };
    for size in TIGHT {
        let lifted = run_shot(size, 30.0, lifted_once);
        let steady = run_shot(size, 30.0, |_| speaker(0.45, 0.10));
        assert_eq!(lifted.framer.head_height(), None, "{size:?}");
        assert!(
            (lifted.last_height() - steady.last_height()).abs() < 1e-9,
            "{size:?}: {} vs {}",
            lifted.last_height(),
            steady.last_height()
        );
        assert_eq!(lifted.limited, steady.limited, "{size:?}");
    }
    let medium = run_shot(ShotSize::Medium, 30.0, lifted_once);
    assert!((medium.last_height() - 0.3804).abs() < 1e-3);
}

#[test]
fn a_speaker_stepping_out_from_a_lectern_is_still_read_as_cut() {
    // Cut by the bottom for 10 s, behind a lectern for 5 s (a squat box
    // ending at 0.80, which measures nothing), one box ending at 0.985 as
    // they step out, then cut by the bottom again for 30 s. Had the
    // lectern ended the cut state, the 0.985 box would be measured as a
    // whole body and seed b.h / 7.5, and Medium close and Medium would
    // collapse onto the Close at the ceiling for the rest of the shot.
    let lectern_exit = |t: f64| {
        if t < 10.0 {
            speaker(0.45, 0.10)
        } else if t < 15.0 {
            Rect::new(0.45, 0.65, 0.10, 0.15)
        } else if (t - 15.0).abs() < DT / 2.0 {
            Rect::new(0.45, 0.65, 0.10, 0.335)
        } else {
            speaker(0.45, 0.10)
        }
    };
    for (size, h, limited) in [
        (ShotSize::Close, 0.25, true),
        (ShotSize::MediumClose, 0.2855, false),
        (ShotSize::Medium, 0.3804, false),
    ] {
        let trace = run_shot(size, 45.0, lectern_exit);
        assert_eq!(trace.framer.head_height(), None, "{size:?}");
        let last = trace.last_height();
        assert!((last - h).abs() < 1e-3, "{size:?}: {last}");
        assert_eq!(trace.limited.last(), Some(&limited), "{size:?}");
    }
}
