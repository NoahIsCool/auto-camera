//! Evaluation: ground-truth labels, the per-tick trace, and the scorers
//! that measure one against the other.
//!
//! The rebuild gates releases on measured quality on an annotated
//! evaluation set of real footage, not just plumbing tests (spec V9). This
//! crate holds the parts of that harness that need no I/O:
//!
//! * [`labels`]: the versioned label file for one clip (the video's
//!   manifest, people boxed on keyframes with visibility, truncation and
//!   body lines, roles over time and exclusion zones), its JSON form and
//!   its validator, whose errors name fields by path.
//! * [`mot`]: an importer for MOT Challenge ground truth.
//! * [`trace`]: the public schema of the engine's per-tick trace
//!   ([`TraceRecord`], one JSON line per tick), which `ac-engine` emits and
//!   the app's `--trace` writes: the subject boxes, targets and crops of a
//!   real run, for scoring offline.
//! * [`scorers`]: [`score`] measures a trace against a clip's labels:
//!   framing (head and torso in the crop, headroom per shot size, cut
//!   lines), crop motion (acceleration, jerk, pan and zoom speed, holds,
//!   jitter at rest, settle time), tracking per query (recall, false
//!   subjects, ID switches, IDF1, time to acquire and reacquire) and
//!   switching (jump cuts, dwell, long wides, on-air swaps, loss on
//!   program and the fallback from it).
//! * [`thresholds`]: the limits those metrics are judged against, a TOML
//!   file whose defaults are the specification's numbers with their
//!   sources, and [`report`]: the verdict per metric, as JSON.
//! * [`containment`] and [`jerk`], the first two metrics, over a bare crop
//!   path and subject boxes sampled at a fixed frame interval.
//!
//! Everything here is a function of its arguments: files are parsed from
//! strings, and reading them, hashing videos and running the engine over
//! clips belong to the runner.
//!
//! The trace schema lives here, below the engine, so that scorers read a
//! trace without the runtime; this crate depends only on `ac-core`,
//! `serde`, `serde_json`, `serde_path_to_error`, `thiserror` and `toml` (a
//! test checks it).

use ac_core::Rect;

pub mod labels;
pub mod mot;
pub mod report;
pub mod scorers;
pub mod thresholds;
pub mod trace;

pub use labels::{FieldError, FieldErrors, LabelFile, SCHEMA_VERSION};
pub use mot::{MotError, import_mot};
pub use report::{Measure, Metric, REPORT_VERSION, Report, Verdict};
pub use scorers::{Content, QueryBinding, Scope, ScoreError, score};
pub use thresholds::{DEFAULT_THRESHOLDS, Thresholds, ThresholdsError};
pub use trace::{TRACE_VERSION, TraceError, TraceRecord, parse_trace};

/// Fraction of frames in which the subject box lies fully inside the crop.
///
/// Returns `None` for empty input or mismatched lengths.
pub fn containment(crops: &[Rect], subjects: &[Rect]) -> Option<f64> {
    if crops.is_empty() || crops.len() != subjects.len() {
        return None;
    }
    let inside = crops
        .iter()
        .zip(subjects)
        .filter(|(c, s)| c.contains_rect(s))
        .count();
    Some(inside as f64 / crops.len() as f64)
}

/// Peak and root-mean-square jerk of the crop centre, in source widths per
/// second cubed, using third finite differences at interval `dt`
/// ([`scorers::motion::differentiate`]).
///
/// Returns `None` with fewer than four samples or a `dt` that is not a
/// positive finite number.
pub fn jerk(crops: &[Rect], dt: f64) -> Option<(f64, f64)> {
    if crops.len() < 4 || !(dt > 0.0 && dt.is_finite()) {
        return None;
    }
    let centres: Vec<(f64, [f64; 2])> = crops
        .iter()
        .enumerate()
        .map(|(i, r)| (i as f64 * dt, [r.center().x, r.center().y]))
        .collect();
    let d = scorers::motion::differentiate;
    let values: Vec<f64> = d(&d(&d(&centres)))
        .iter()
        .map(|(_, [x, y])| x.hypot(*y))
        .collect();
    scorers::motion::peak_and_rms(&values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_counts_frames_with_subject_inside() {
        let crop = Rect::new(0.0, 0.0, 0.5, 0.5);
        let inside = Rect::new(0.1, 0.1, 0.1, 0.1);
        let outside = Rect::new(0.6, 0.6, 0.1, 0.1);
        assert_eq!(containment(&[crop, crop], &[inside, outside]), Some(0.5));
        assert_eq!(containment(&[crop], &[]), None);
    }

    #[test]
    fn constant_velocity_has_zero_jerk() {
        let crops: Vec<Rect> = (0..10)
            .map(|i| Rect::new(i as f64 * 0.01, 0.0, 0.2, 0.2))
            .collect();
        let (peak, rms) = jerk(&crops, 1.0 / 30.0).unwrap();
        assert!(peak < 1e-6 && rms < 1e-6);
    }

    #[test]
    fn jerk_needs_four_samples() {
        assert_eq!(jerk(&[Rect::FULL; 3], 0.1), None);
        assert_eq!(jerk(&[Rect::FULL; 4], 0.0), None);
        assert_eq!(jerk(&[Rect::FULL; 4], f64::NAN), None);
    }

    #[test]
    fn a_step_has_the_third_difference_as_its_jerk() {
        // Centre x: 0.1, 0.1, 0.2, 0.2 at dt = 0.5 s. The third difference
        // is 0.2 - 3 * 0.2 + 3 * 0.1 - 0.1 = -0.2; over dt^3 = 0.125 that
        // is a jerk of 1.6 source widths/s^3, the only sample.
        let crops = [0.0, 0.0, 0.1, 0.1].map(|x| Rect::new(x, 0.0, 0.2, 0.2));
        let (peak, rms) = jerk(&crops, 0.5).unwrap();
        assert!((peak - 1.6).abs() < 1e-9 && (rms - 1.6).abs() < 1e-9);
    }
}
