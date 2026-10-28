//! Switching: what goes on program and when (spec C.4 auto-director
//! constraints and loss behaviour, C.3 reliability "no on-air swaps", M9
//! health guard).
//!
//! Scored on every record of the trace, every source, except on-air swaps,
//! which need the labels.
//!
//! # Takes and stints
//!
//! The shot program is *headed for* on a tick is the preview shot while a
//! transition runs (it is coming in, and neither bus can change before the
//! transition ends) and the program shot otherwise ([`head`]). A *take* is
//! a tick whose head differs from the previous tick's and is a shot
//! ([`takes`]): a cut, the start of a mix or dip, a cut made during a
//! transition. It runs *from* the previous head *to* the new one. A
//! *stint* is a maximal run of ticks with one head ([`stints`]); it lasts
//! from its first tick's PTS to the next tick's (the last tick's when the
//! trace ends first, and the stint is then *open*).
//!
//! * `switching.jump_cuts`: takes between two shots of one source whose
//!   crops (the outgoing shot's on the tick before, the incoming one's on
//!   the take's tick, the pictures either side of the cut) overlap with
//!   IoU above `jump_cut_iou` ([`is_jump_cut`]).
//! * `switching.dwell_violations`: stints that began with a take and ended
//!   with one sooner than the content's minimum dwell. A take away from a
//!   shot that was unavailable or lost on the stint's last tick is a
//!   fallback, which the dwell does not bind.
//! * `switching.long_wides`: stints of a wide that lasted longer than
//!   `max_wide_s`, open ones included: a wide held past the limit until
//!   the trace ended was still held past it.
//! * `switching.takes_onto_unavailable`: takes whose incoming shot is
//!   unavailable or lost on the take's tick.
//!
//! # Loss on program
//!
//! A shot is *available* on a tick when it exists, its source has a
//! frame, and (for a subject shot) its subject is listed and tracking
//! ([`availability`]). A *loss* is a maximal run of ticks whose program
//! shot is not available; it lasts until the first tick with an available
//! program shot (or to the last tick, unresolved).
//!
//! * `switching.time_unavailable_on_program`: the losses' total duration.
//! * `switching.time_to_fall_back`: the longest loss, whatever ended it (a
//!   fallback take, or the subject found again): the time viewers watched
//!   a shot that had lost its subject. 0 when program never lost one.
//!
//! # On-air swaps
//!
//! `switching.on_air_swaps` counts, for each subject shot of the scoped
//! source, the times its subject's identity changes between scored
//! keyframes while the shot stays on air ([`super::on_air`]) on all of
//! them: the picture on air switched people under the viewers' eyes. A
//! keyframe where the box matches nobody keeps the last identity; a
//! keyframe off air forgets it.

use std::collections::BTreeMap;

use ac_core::{ShotId, TrackState};

use super::join::{Join, shot};
use super::{Content, seconds_between};
use crate::labels::TrackId;
use crate::report::{Limit, Measure, Metric, TOLERANCE};
use crate::thresholds::SwitchingLimits;
use crate::trace::{ShotTrace, TraceRecord};

/// A change of the shot program is headed for; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Take {
    /// Index of the take's record.
    pub record: usize,
    /// The previous head, if any.
    pub from: Option<ShotId>,
    /// The new head.
    pub to: ShotId,
}

/// A run of ticks with one head; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct Stint {
    /// The shot.
    pub shot: ShotId,
    /// Index of the first record.
    pub first: usize,
    /// Index of the last record.
    pub last: usize,
    /// How long it lasted, in seconds.
    pub seconds: f64,
    /// It began with a take (not on the trace's first tick).
    pub taken: bool,
    /// The trace ended before it did.
    pub open: bool,
}

/// Whether a shot can be shown on a tick; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// It can.
    Available,
    /// Its subject is lost: the crop is held where it last was.
    Lost,
    /// It does not exist, its source has no frame, or its subject is gone.
    Unavailable,
}

/// The shot program is headed for on `record`: the preview shot during a
/// transition, else the program shot.
pub fn head(record: &TraceRecord) -> Option<&ShotId> {
    let s = &record.switcher;
    if s.transition.is_some() {
        s.preview.as_ref()
    } else {
        s.program.as_ref()
    }
}

/// Every take of `trace`, in order.
pub fn takes(trace: &[TraceRecord]) -> Vec<Take> {
    trace
        .windows(2)
        .enumerate()
        .filter_map(|(i, w)| {
            let (before, now) = (head(&w[0]), head(&w[1])?);
            (before != Some(now)).then(|| Take {
                record: i + 1,
                from: before.cloned(),
                to: now.clone(),
            })
        })
        .collect()
}

/// Every stint of `trace`, in order.
pub fn stints(trace: &[TraceRecord]) -> Vec<Stint> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < trace.len() {
        let Some(shot) = head(&trace[i]) else {
            i += 1;
            continue;
        };
        let first = i;
        while i + 1 < trace.len() && head(&trace[i + 1]) == Some(shot) {
            i += 1;
        }
        let open = i + 1 == trace.len();
        let end = if open { &trace[i] } else { &trace[i + 1] };
        found.push(Stint {
            shot: shot.clone(),
            first,
            last: i,
            seconds: seconds_between(trace[first].pts, end.pts),
            taken: first > 0,
            open,
        });
        i += 1;
    }
    found
}

/// Whether shot `id` can be shown on `record`; see the module docs.
pub fn availability(record: &TraceRecord, id: &ShotId) -> Availability {
    let Some(shot) = shot(record, id) else {
        return Availability::Unavailable;
    };
    let Some(source) = record.sources.iter().find(|s| s.id == shot.source) else {
        return Availability::Unavailable;
    };
    if source.frame.is_none() {
        return Availability::Unavailable;
    }
    let Some(subject) = shot.subject else {
        return Availability::Available;
    };
    match source.subjects.iter().find(|s| s.id == subject) {
        None => Availability::Unavailable,
        Some(s) if s.state == TrackState::Lost => Availability::Lost,
        Some(_) => Availability::Available,
    }
}

/// Whether a take from `from` to `to` is a jump cut at `max_iou`: one
/// source, crops overlapping with IoU above it.
pub fn is_jump_cut(from: &ShotTrace, to: &ShotTrace, max_iou: f64) -> bool {
    from.source == to.source && from.crop.iou(&to.crop) > max_iou
}

/// The losses on program in `trace`: (seconds, resolved) each.
pub fn losses(trace: &[TraceRecord]) -> Vec<(f64, bool)> {
    let lost = |r: &TraceRecord| {
        r.switcher
            .program
            .as_ref()
            .is_some_and(|p| availability(r, p) != Availability::Available)
    };
    let mut found = Vec::new();
    let mut i = 0;
    while i < trace.len() {
        if !lost(&trace[i]) {
            i += 1;
            continue;
        }
        let first = i;
        while i < trace.len() && lost(&trace[i]) {
            i += 1;
        }
        let resolved = i < trace.len();
        let end = if resolved { &trace[i] } else { &trace[i - 1] };
        found.push((seconds_between(trace[first].pts, end.pts), resolved));
    }
    found
}

/// The on-air swaps over `join`'s scored keyframes; see the module docs.
fn on_air_swaps(join: &Join) -> u64 {
    let mut last: BTreeMap<&ShotId, TrackId> = BTreeMap::new();
    let mut swaps = 0;
    for k in &join.keyframes {
        let shots: Vec<&ShotTrace> = join
            .on_air_subject_shots(k)
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        last.retain(|id, _| shots.iter().any(|s| &s.id == *id));
        for s in shots {
            let Some(now) = s.subject.and_then(|id| k.identity.get(&id)) else {
                continue;
            };
            if last
                .insert(&s.id, *now)
                .is_some_and(|before| before != *now)
            {
                swaps += 1;
            }
        }
    }
    swaps
}

/// Scores switching over `join` into `out`; see the module docs.
pub(crate) fn score(join: &Join, limits: &SwitchingLimits, out: &mut Vec<Metric>) {
    let trace = join.trace;
    let all_takes = takes(trace);
    let n_takes = all_takes.len() as u64;
    let jump_cuts = all_takes
        .iter()
        .filter(|t| {
            let from = t.from.as_ref().and_then(|f| shot(&trace[t.record - 1], f));
            let to = shot(&trace[t.record], &t.to);
            from.zip(to)
                .is_some_and(|(f, to)| is_jump_cut(f, to, limits.jump_cut_iou))
        })
        .count() as u64;
    out.push(
        Metric::count(
            "switching.jump_cuts",
            "takes",
            jump_cuts,
            n_takes,
            Limit::max_count(limits.max_jump_cuts),
        )
        .detail("jump_cut_iou", limits.jump_cut_iou),
    );

    let min_dwell = match join.scope.content {
        Content::Talk => limits.min_dwell_talk_s,
        Content::Music => limits.min_dwell_music_s,
    };
    let all_stints = stints(trace);
    let judged: Vec<&Stint> = all_stints
        .iter()
        .filter(|s| s.taken && !s.open && head(&trace[s.last + 1]).is_some())
        .filter(|s| availability(&trace[s.last], &s.shot) == Availability::Available)
        .collect();
    let short = judged
        .iter()
        .filter(|s| s.seconds < min_dwell - TOLERANCE)
        .count() as u64;
    out.push(
        Metric::count(
            "switching.dwell_violations",
            "stints",
            short,
            judged.len() as u64,
            Limit::max_count(limits.max_dwell_violations),
        )
        .detail("min_dwell_s", min_dwell),
    );

    let wides: Vec<&Stint> = all_stints
        .iter()
        .filter(|s| shot(&trace[s.first], &s.shot).is_some_and(|t| t.subject.is_none()))
        .collect();
    let long = wides
        .iter()
        .filter(|s| s.seconds > limits.max_wide_s + TOLERANCE)
        .count() as u64;
    out.push(
        Metric::count(
            "switching.long_wides",
            "stints",
            long,
            wides.len() as u64,
            Limit::max_count(limits.max_long_wides),
        )
        .detail("max_wide_s", limits.max_wide_s),
    );

    out.push(Metric::count(
        "switching.on_air_swaps",
        "swaps",
        on_air_swaps(join),
        join.keyframes.len() as u64,
        Limit::max_count(limits.max_on_air_swaps),
    ));

    let onto_unavailable = all_takes
        .iter()
        .filter(|t| availability(&trace[t.record], &t.to) != Availability::Available)
        .count() as u64;
    out.push(Metric::count(
        "switching.takes_onto_unavailable",
        "takes",
        onto_unavailable,
        n_takes,
        Limit::max_count(limits.max_takes_onto_unavailable),
    ));

    let all_losses = losses(trace);
    let unresolved = all_losses.iter().filter(|(_, r)| !r).count() as f64;
    // Folded from +0: an empty f64 sum is -0.
    let total = all_losses.iter().fold(0.0, |a, (s, _)| a + s);
    let longest = all_losses.iter().map(|(s, _)| *s).fold(0.0, f64::max);
    let n_losses = all_losses.len() as u64;
    out.push(Metric::new(
        "switching.time_unavailable_on_program",
        "s",
        Measure::Value(total),
        n_losses,
        Limit::max(limits.max_time_unavailable_s),
    ));
    out.push(
        Metric::new(
            "switching.time_to_fall_back",
            "s",
            Measure::Value(longest),
            n_losses,
            Limit::max(limits.max_time_to_fall_back_s),
        )
        .detail("unresolved", unresolved),
    );
}

#[cfg(test)]
mod tests;
