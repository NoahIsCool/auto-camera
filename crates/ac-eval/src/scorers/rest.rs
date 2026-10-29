//! Shots of people at rest: whether their crop comes to a stop and stays
//! there (spec C.3 "no visible jitter at rest", C.4 "static by default").
//!
//! # Rest spans
//!
//! A labelled person is *at rest* between two consecutive keyframes that
//! both label them when their box's centre moves less than `rest_speed`
//! source spans per second on each axis and its height changes by less
//! than `rest_speed` of itself per second (C.4's dead-zone release speed;
//! the width is left out, since a still speaker's gestures change it). A
//! *rest span* is a maximal run of such keyframes lasting at least
//! `min_rest_s` ([`rest_spans`]). Labels are the ground truth here: the
//! engine's own boxes jitter, and that is what is being measured.
//!
//! # Per shot
//!
//! A rest span is scored for every subject shot of the scoped source whose
//! subject's box matches the person (their identity, not just the person
//! the shot last framed) on every scored keyframe of the span: a subject
//! that drifted off a still person is not at rest, and its crop may follow
//! it. The shot's crops are those of every record showing a frame of the
//! span, timed by the clip time of that frame ([`settling`]):
//!
//! * the *final* crop is the span's last; the crop has *settled* from the
//!   first sample after which it stays within `settle_band` of the final
//!   crop's width (across) and height (down and in size) to the span's
//!   end. `motion.settle_time` is the largest time from a span's start to
//!   that sample; it is never more than the span, so it is always defined.
//! * `motion.jitter_at_rest` is the largest crop-height variation, highest
//!   minus lowest over the final height, from the settled sample on, or
//!   from the span's midpoint if the crop settles later: half the span is
//!   the time a crop is given to settle, so a crop that jitters too much
//!   to ever settle is still scored on the second half instead of on its
//!   last sample alone.
//!
//! Both are not measurable when no span has a shot framing its person
//! with two or more crops.
//!
//! # Invariants
//!
//! * A rest span's first and last keyframes both label its person, every
//!   keyframe between them does too, and it lasts at least `min_rest_s`.
//! * A settle time lies between 0 and the span's length, since every crop
//!   sample is of a frame inside the span; jitter is a non-negative
//!   fraction of the final crop's height.

use std::collections::BTreeSet;

use ac_core::{Rect, ShotId};

use super::join::Join;
use super::seconds_between_ns;
use crate::labels::{LabelFile, TrackId, validate::SLACK};
use crate::report::{Limit, Measure, Metric};
use crate::thresholds::MotionLimits;

/// A labelled person at rest over a run of keyframes; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestSpan {
    /// Who is at rest.
    pub track: TrackId,
    /// The span's first keyframe's frame.
    pub first_frame: u64,
    /// The span's last keyframe's frame.
    pub last_frame: u64,
    /// The first keyframe's PTS, nanoseconds of clip time.
    pub start_ns: u64,
    /// The last keyframe's PTS.
    pub end_ns: u64,
}

/// How a crop settled over a rest span; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settling {
    /// Seconds from the span's start to the settled sample.
    pub settle_s: f64,
    /// Crop-height variation from the settled sample (or the midpoint) on,
    /// as a fraction of the final height.
    pub jitter: f64,
}

/// A keyframe's frame and PTS in nanoseconds.
type Mark = (u64, u64);

/// Whether `a` then `b` (boxes `dt` seconds apart) is at rest at
/// `rest_speed`.
fn still(a: &Rect, b: &Rect, dt: f64, rest_speed: f64) -> bool {
    if dt <= 0.0 || a.h <= 0.0 || b.h <= 0.0 {
        return false;
    }
    let (ca, cb) = (a.center(), b.center());
    (cb.x - ca.x).abs() / dt <= rest_speed
        && (cb.y - ca.y).abs() / dt <= rest_speed
        && (b.h / a.h).ln().abs() / dt <= rest_speed
}

/// Every rest span of every labelled person in `labels`, by track and
/// then by time; see the module docs.
pub fn rest_spans(labels: &LabelFile, rest_speed: f64, min_rest_s: f64) -> Vec<RestSpan> {
    let mut tracks: Vec<TrackId> = labels.tracks.iter().map(|t| t.id).collect();
    tracks.sort_unstable();
    let mut spans = Vec::new();
    for track in tracks {
        // The person on each keyframe, `None` where not labelled.
        let seen: Vec<Option<(u64, u64, Rect)>> = labels
            .keyframes
            .iter()
            .map(|k| {
                let p = k.people.iter().find(|p| p.track == track)?;
                Some((k.frame, k.pts_ns, p.bbox))
            })
            .collect();
        // The open run's first and last keyframes.
        let mut run: Option<(Mark, Mark)> = None;
        let mut close = |run: &mut Option<(Mark, Mark)>| {
            if let Some(((first_frame, start_ns), (last_frame, end_ns))) = run.take()
                && seconds_between_ns(start_ns, end_ns) >= min_rest_s - SLACK
            {
                spans.push(RestSpan {
                    track,
                    first_frame,
                    last_frame,
                    start_ns,
                    end_ns,
                });
            }
        };
        for pair in seen.windows(2) {
            match (pair[0], pair[1]) {
                (Some((fa, ta, a)), Some((fb, tb, b)))
                    if still(&a, &b, seconds_between_ns(ta, tb), rest_speed) =>
                {
                    let start = run.map_or((fa, ta), |(start, _)| start);
                    run = Some((start, (fb, tb)));
                }
                _ => close(&mut run),
            }
        }
        close(&mut run);
    }
    spans
}

/// How the crops `samples` (seconds of clip time, crop) settled over a
/// span from `start` to `end` seconds, within `band`; `None` with fewer
/// than two samples. See the module docs.
pub fn settling(samples: &[(f64, Rect)], start: f64, end: f64, band: f64) -> Option<Settling> {
    let (_, last) = *samples.last()?;
    if samples.len() < 2 || last.h <= 0.0 {
        return None;
    }
    let fc = last.center();
    let within = |c: &Rect| {
        let cc = c.center();
        (cc.x - fc.x).abs() <= band * last.w + SLACK
            && (cc.y - fc.y).abs() <= band * last.h + SLACK
            && (c.w - last.w).abs() <= band * last.w + SLACK
            && (c.h - last.h).abs() <= band * last.h + SLACK
    };
    let settled = samples
        .iter()
        .rposition(|(_, c)| !within(c))
        .map_or(0, |i| i + 1);
    let midpoint = start + (end - start) / 2.0;
    let half = samples
        .iter()
        .position(|(t, _)| *t >= midpoint - SLACK)
        .unwrap_or(samples.len() - 1);
    let window = &samples[settled.min(half)..];
    let lowest = window
        .iter()
        .map(|(_, c)| c.h)
        .fold(f64::INFINITY, f64::min);
    let highest = window
        .iter()
        .map(|(_, c)| c.h)
        .fold(f64::NEG_INFINITY, f64::max);
    Some(Settling {
        settle_s: (samples[settled].0 - start).max(0.0),
        jitter: (highest - lowest) / last.h,
    })
}

/// The subject shots whose subject matches `span`'s person on every
/// scored keyframe of the span, and their crops over it (clip seconds,
/// crop).
fn span_shots(join: &Join, span: &RestSpan) -> Vec<(ShotId, Vec<(f64, Rect)>)> {
    let in_span = |frame: u64| (span.first_frame..=span.last_frame).contains(&frame);
    let mut shots: Option<BTreeSet<&ShotId>> = None;
    for k in join.keyframes.iter().filter(|k| in_span(k.keyframe.frame)) {
        let framing: BTreeSet<&ShotId> = join
            .record(k)
            .shots
            .iter()
            .filter(|s| s.source == join.scope.source)
            .filter(|s| s.subject.and_then(|id| k.identity.get(&id)) == Some(&span.track))
            .map(|s| &s.id)
            .collect();
        shots = Some(match shots {
            None => framing,
            Some(before) => before.intersection(&framing).copied().collect(),
        });
    }
    let fps = join.labels.clip.fps;
    shots
        .unwrap_or_default()
        .into_iter()
        .map(|id| {
            let samples = join
                .trace
                .iter()
                .zip(&join.frame_at)
                .filter_map(|(record, frame)| {
                    let frame = frame.filter(|f| in_span(*f))?;
                    let crop = super::join::shot(record, id)?.crop;
                    let t = fps.pts_ns(frame)? as f64 / 1e9;
                    Some((t, crop))
                })
                .collect();
            (id.clone(), samples)
        })
        .collect()
}

/// Scores shots of people at rest over `join` into `out`; see the module
/// docs.
pub(crate) fn score(join: &Join, limits: &MotionLimits, out: &mut Vec<Metric>) {
    let spans = rest_spans(join.labels, limits.rest_speed, limits.min_rest_s);
    let mut results = Vec::new();
    for span in &spans {
        let (start, end) = (span.start_ns as f64 / 1e9, span.end_ns as f64 / 1e9);
        for (_, samples) in span_shots(join, span) {
            results.extend(settling(&samples, start, end, limits.settle_band));
        }
    }
    let none = format!(
        "no labelled person stayed still for {} s while a subject shot framed them",
        limits.min_rest_s
    );
    let largest = |pick: fn(&Settling) -> f64| match results.iter().map(pick).reduce(f64::max) {
        Some(v) => Measure::Value(v),
        None => Measure::NotMeasurable(none.clone()),
    };
    let n = results.len() as u64;
    out.push(
        Metric::new(
            "motion.jitter_at_rest",
            "fraction of crop height",
            largest(|s| s.jitter),
            n,
            Limit::max(limits.max_jitter_at_rest),
        )
        .detail("rest_spans", spans.len() as f64),
    );
    out.push(Metric::new(
        "motion.settle_time",
        "s",
        largest(|s| s.settle_s),
        n,
        Limit::max(limits.max_settle_s),
    ));
}

#[cfg(test)]
mod tests;
