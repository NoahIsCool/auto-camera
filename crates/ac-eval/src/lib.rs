//! Framing-quality metrics.
//!
//! The rebuild gates releases on measured framing quality, not just plumbing
//! tests. These metrics score a crop path against ground-truth subject boxes
//! sampled at a fixed frame interval. Annotated real footage plugs into the
//! same functions once it exists.

pub mod trace;

pub use trace::{TRACE_VERSION, TraceError, TraceRecord, parse_trace};

use ac_core::Rect;

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
/// second cubed, using third finite differences at interval `dt`.
///
/// Returns `None` with fewer than four samples or a non-positive `dt`.
pub fn jerk(crops: &[Rect], dt: f64) -> Option<(f64, f64)> {
    if crops.len() < 4 || dt <= 0.0 {
        return None;
    }
    let xs: Vec<(f64, f64)> = crops.iter().map(|r| (r.center().x, r.center().y)).collect();
    let values: Vec<f64> = xs
        .windows(4)
        .map(|w| {
            let jx = w[3].0 - 3.0 * w[2].0 + 3.0 * w[1].0 - w[0].0;
            let jy = w[3].1 - 3.0 * w[2].1 + 3.0 * w[1].1 - w[0].1;
            jx.hypot(jy) / dt.powi(3)
        })
        .collect();
    let peak = values.iter().copied().fold(0.0, f64::max);
    let rms = (values.iter().map(|v| v * v).sum::<f64>() / values.len() as f64).sqrt();
    Some((peak, rms))
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
    }
}
