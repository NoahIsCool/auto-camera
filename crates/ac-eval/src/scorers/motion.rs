//! Crop motion: how smoothly every subject shot's crop moves (spec V9
//! jerk, C.4 acceleration and jerk caps, peak pan speed, zoom rate cap and
//! minimum static hold).
//!
//! # Paths
//!
//! A shot's *path* is its crop on consecutive trace records, timed by each
//! record's master-clock PTS ([`shot_paths`]). Every subject shot of the
//! scoped source has one, on air or not: the path is the framer's work,
//! and a jerk off air is on air the moment the shot is taken. A path ends
//! where the shot is missing from a record or its time fails to advance; a
//! crop with no size or a non-finite number is left out. Wides do not move
//! and are not scored.
//!
//! # Derivatives
//!
//! [`differentiate`] takes difference quotients between consecutive
//! samples, placed at the midpoint of their times: for samples `dt` apart
//! it gives exactly the first, second and third finite differences over
//! `dt`, `dt²` and `dt³` (as [`crate::jerk`] does), and it stays right
//! across a skipped tick, where the gap is longer. From each path:
//!
//! * acceleration and jerk of the crop centre, in pixels of the source
//!   picture (the clip's size in the label file), the unit of C.4's PoC
//!   baseline; peak and RMS, pooled over every path;
//! * pan speed: horizontal centre speed in crop widths per second, the
//!   width taken as the geometric mean of the two crops;
//! * zoom rate: the rate of change of the natural log of the crop width;
//! * static holds: a crop is *still* between two samples when its centre
//!   moves less than `rest_speed` crop widths (across) and crop heights
//!   (down) per second and its log width and log height change slower
//!   than `rest_speed` per second (C.4's dead-zone release speed). A hold
//!   is a run of still intervals with motion on both sides; holds that
//!   start or end a path are cut short by the trace and not counted
//!   ([`holds`]).
//!
//! Peaks and RMS values over no samples are not measurable: a path needs
//! three samples for an acceleration, four for a jerk.

use std::collections::{BTreeMap, BTreeSet};

use ac_core::{Rect, ShotId, SourceId};

use super::join::Join;
use super::seconds_between;
use crate::report::{Limit, Measure, Metric};
use crate::thresholds::MotionLimits;
use crate::trace::TraceRecord;

/// One subject shot's crop over consecutive records: (seconds from the
/// trace's first record, crop).
#[derive(Debug, Clone, PartialEq)]
pub struct ShotPath {
    /// The shot.
    pub shot: ShotId,
    /// Its crops, times strictly increasing.
    pub samples: Vec<(f64, Rect)>,
}

/// What a path did; see the module docs. Each list is in time order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PathMotion {
    /// Crop-centre acceleration magnitudes, source pixels per second
    /// squared.
    pub acceleration: Vec<f64>,
    /// Crop-centre jerk magnitudes, source pixels per second cubed.
    pub jerk: Vec<f64>,
    /// Horizontal speeds, crop widths per second.
    pub pan_speed: Vec<f64>,
    /// Zoom rates, natural-log crop widths per second.
    pub zoom_rate: Vec<f64>,
    /// Interior static holds, seconds.
    pub holds: Vec<f64>,
}

/// Difference quotients of `samples` (time, value) at the midpoints of
/// consecutive times; a pair whose time does not advance is skipped.
pub fn differentiate<const N: usize>(samples: &[(f64, [f64; N])]) -> Vec<(f64, [f64; N])> {
    samples
        .windows(2)
        .filter_map(|w| {
            let dt = w[1].0 - w[0].0;
            (dt > 0.0).then(|| {
                (
                    (w[0].0 + w[1].0) / 2.0,
                    std::array::from_fn(|i| (w[1].1[i] - w[0].1[i]) / dt),
                )
            })
        })
        .collect()
}

/// The largest of `values` and their root mean square; `None` when there
/// are none.
pub fn peak_and_rms(values: &[f64]) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let peak = values.iter().copied().fold(0.0, f64::max);
    let rms = (values.iter().map(|v| v * v).sum::<f64>() / values.len() as f64).sqrt();
    Some((peak, rms))
}

/// Whether `crop` is usable on a path: finite, with a positive size.
fn usable(crop: &Rect) -> bool {
    [crop.x, crop.y, crop.w, crop.h]
        .iter()
        .all(|v| v.is_finite())
        && crop.w > 0.0
        && crop.h > 0.0
}

/// The path of every subject shot of `source` in `trace` (see the module
/// docs), sorted by shot and then by start.
pub fn shot_paths(trace: &[TraceRecord], source: &SourceId) -> Vec<ShotPath> {
    let Some(first) = trace.first() else {
        return Vec::new();
    };
    let mut open: BTreeMap<ShotId, Vec<(f64, Rect)>> = BTreeMap::new();
    let mut done = Vec::new();
    for record in trace {
        let t = seconds_between(first.pts, record.pts);
        let mut present = BTreeSet::new();
        for shot in &record.shots {
            if &shot.source != source || shot.subject.is_none() || !usable(&shot.crop) {
                continue;
            }
            present.insert(&shot.id);
            let path = open.entry(shot.id.clone()).or_default();
            if path.last().is_some_and(|(last, _)| t <= *last) {
                done.push(ShotPath {
                    shot: shot.id.clone(),
                    samples: std::mem::take(path),
                });
            }
            path.push((t, shot.crop));
        }
        let gone: Vec<ShotId> = open
            .keys()
            .filter(|id| !present.contains(id))
            .cloned()
            .collect();
        for shot in gone {
            let samples = open.remove(&shot).unwrap_or_default();
            done.push(ShotPath { shot, samples });
        }
    }
    done.extend(
        open.into_iter()
            .map(|(shot, samples)| ShotPath { shot, samples }),
    );
    done.retain(|p| !p.samples.is_empty());
    done.sort_by(|a, b| {
        a.shot
            .cmp(&b.shot)
            .then(a.samples[0].0.total_cmp(&b.samples[0].0))
    });
    done
}

/// The durations of the interior still runs of a path whose samples are
/// at `times`, where `still[i]` says whether the crop was still from
/// `times[i]` to `times[i + 1]`; see the module docs.
pub fn holds(times: &[f64], still: &[bool]) -> Vec<f64> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < still.len() {
        if !still[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < still.len() && still[i] {
            i += 1;
        }
        // `start..i` is a maximal still run: interior when motion comes
        // before it and after it.
        if start > 0 && i < still.len() {
            found.push(times[i] - times[start]);
        }
    }
    found
}

/// What the crop did along `samples` in a source of `width_px` by
/// `height_px` pixels, with `rest_speed` the still threshold; see the
/// module docs.
pub fn path_motion(
    samples: &[(f64, Rect)],
    width_px: f64,
    height_px: f64,
    rest_speed: f64,
) -> PathMotion {
    let centres: Vec<(f64, [f64; 2])> = samples
        .iter()
        .map(|(t, c)| (*t, [c.center().x * width_px, c.center().y * height_px]))
        .collect();
    let velocity = differentiate(&centres);
    let acceleration = differentiate(&velocity);
    let jerk = differentiate(&acceleration);
    let magnitude = |(_, [x, y]): &(f64, [f64; 2])| x.hypot(*y);

    // Centre in source spans and log size, whose rates are scale-free.
    let state: Vec<(f64, [f64; 4])> = samples
        .iter()
        .map(|(t, c)| (*t, [c.center().x, c.center().y, c.w.ln(), c.h.ln()]))
        .collect();
    let mut pan_speed = Vec::new();
    let mut zoom_rate = Vec::new();
    let mut still = Vec::new();
    for pair in state.windows(2) {
        let Some(&(_, [vx, vy, vw, vh])) = differentiate(pair).first() else {
            continue;
        };
        // The crop's size over the interval: the geometric mean of its
        // ends.
        let w = ((pair[0].1[2] + pair[1].1[2]) / 2.0).exp();
        let h = ((pair[0].1[3] + pair[1].1[3]) / 2.0).exp();
        pan_speed.push(vx.abs() / w);
        zoom_rate.push(vw.abs());
        still.push(
            vx.abs() / w <= rest_speed
                && vy.abs() / h <= rest_speed
                && vw.abs() <= rest_speed
                && vh.abs() <= rest_speed,
        );
    }
    let times: Vec<f64> = samples.iter().map(|(t, _)| *t).collect();
    PathMotion {
        acceleration: acceleration.iter().map(magnitude).collect(),
        jerk: jerk.iter().map(magnitude).collect(),
        pan_speed,
        zoom_rate,
        holds: holds(&times, &still),
    }
}

/// The metric `name` with the peak or the RMS of `values` (`rms` picks),
/// or why there are none.
fn peak_metric(
    name: &str,
    unit: &str,
    values: &[f64],
    rms: bool,
    limit: Option<f64>,
    none: &str,
) -> Metric {
    let measure = match peak_and_rms(values) {
        Some((peak, root)) => Measure::Value(if rms { root } else { peak }),
        None => Measure::NotMeasurable(none.to_owned()),
    };
    Metric::new(name, unit, measure, values.len() as u64, Limit::max(limit))
}

/// Scores crop motion over `join` into `out`; see the module docs.
pub(crate) fn score(join: &Join, limits: &MotionLimits, out: &mut Vec<Metric>) {
    let width = f64::from(join.labels.clip.width);
    let height = f64::from(join.labels.clip.height);
    let mut all = PathMotion::default();
    for path in shot_paths(join.trace, &join.scope.source) {
        let m = path_motion(&path.samples, width, height, limits.rest_speed);
        all.acceleration.extend(m.acceleration);
        all.jerk.extend(m.jerk);
        all.pan_speed.extend(m.pan_speed);
        all.zoom_rate.extend(m.zoom_rate);
        all.holds.extend(m.holds);
    }
    let accel = "source px/s^2";
    let jerk = "source px/s^3";
    let few = |n: usize| format!("no subject shot of the source lasted {n} ticks");
    out.push(peak_metric(
        "motion.peak_acceleration",
        accel,
        &all.acceleration,
        false,
        limits.max_peak_acceleration,
        &few(3),
    ));
    out.push(peak_metric(
        "motion.rms_acceleration",
        accel,
        &all.acceleration,
        true,
        limits.max_rms_acceleration,
        &few(3),
    ));
    out.push(peak_metric(
        "motion.peak_jerk",
        jerk,
        &all.jerk,
        false,
        limits.max_peak_jerk,
        &few(4),
    ));
    out.push(peak_metric(
        "motion.rms_jerk",
        jerk,
        &all.jerk,
        true,
        limits.max_rms_jerk,
        &few(4),
    ));
    out.push(peak_metric(
        "motion.peak_pan_speed",
        "crop widths/s",
        &all.pan_speed,
        false,
        limits.max_pan_speed,
        &few(2),
    ));
    out.push(peak_metric(
        "motion.peak_zoom_rate",
        "log crop width/s",
        &all.zoom_rate,
        false,
        limits.max_zoom_rate,
        &few(2),
    ));
    let measure = match all.holds.iter().copied().reduce(f64::min) {
        Some(shortest) => Measure::Value(shortest),
        None => {
            Measure::NotMeasurable("no subject shot's crop stopped and then moved again".into())
        }
    };
    out.push(Metric::new(
        "motion.min_static_hold",
        "s",
        measure,
        all.holds.len() as u64,
        Limit::min(limits.min_static_hold_s),
    ));
}

#[cfg(test)]
mod tests;
