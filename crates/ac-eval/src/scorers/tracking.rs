//! Tracking: whether each query finds its people and keeps them (spec V9
//! ID switches and latency, M3 discovery of every instance).
//!
//! Scored per query, on the scored keyframes: the query's *targets* are
//! the people labelled there that it should find ([`QueryBinding`]: all of
//! them, or those holding its role at that frame, [`holds_role`]), and its
//! *subjects* there are its tracking subjects on the scoped source (lost
//! ones only hold a box and are not counted as found). Keyframes are
//! matched one to one, CLEAR MOT style ([`match_frame`]): a target keeps
//! the subject it last matched while their boxes still overlap at
//! `match_iou`, and the rest are assigned for the most matches and then
//! the most overlap ([`super::assign`]).
//!
//! # Metrics
//!
//! * `tracking.recall`: matched targets over targets (IoU 0.5 by default).
//! * `tracking.false_subjects_per_frame`: subjects matched to no target,
//!   per scored keyframe. A subject whose box centre lies in an exclusion
//!   zone and matches nobody is neither found nor false (the label file's
//!   rule) and is left out of every count here, IDF1's too.
//! * `tracking.id_switches`: CLEAR MOT's count, one each time a target is
//!   matched to another subject than the one it last matched; a target's
//!   first match is no switch. A subject jumping from one person to
//!   another counts once, when the abandoned person is picked up again; a
//!   swap of two subjects' people counts twice, once per person.
//! * `tracking.idf1`: the identity F1 of Ristani et al. (2016): each
//!   person is paired with at most one subject over the whole clip so that
//!   the keyframes on which they match (IoU at the threshold, regardless
//!   of the keyframe's own matching) are as many as possible ([`idf1`]);
//!   IDF1 is twice those over all targets plus all subjects.
//! * `tracking.time_to_acquire`: the longest wait, over people, from a
//!   person's first keyframe as a target to their first match ([`waits`]).
//! * `tracking.time_to_reacquire`: the longest wait from a target going
//!   unmatched after a match (the tracker lost them, or they left and came
//!   back) to their next match; 0 when nobody was lost.
//!
//! A wait still open at a person's last keyframe as a target lasts until
//! then and is counted as unresolved in the details, so a person never
//! found for ten seconds waits ten seconds, while one glimpsed on a single
//! keyframe waits nothing. Times are the keyframes' clip PTS, so their
//! resolution is the keyframe spacing.
//!
//! # Invariants
//!
//! * A keyframe's matching is one to one, so matched targets never exceed
//!   the targets (recall lies in `0..=1`) and switches never exceed the
//!   matches. IDF1's pairing is one to one too, so its matched keyframes
//!   never exceed the targets or the subjects, and IDF1 lies in `0..=1`.
//! * Every query gets the same six metrics in the same order, queries in
//!   sorted order. A rate over no keyframe or no target is not measurable.

use std::collections::{BTreeMap, BTreeSet};

use ac_core::{Point, QueryId, Rect, SubjectId, TrackState};

use super::assign::{FORBIDDEN, minimize};
use super::join::Join;
use super::{QueryBinding, seconds_between_ns};
use crate::labels::{LabelFile, RoleTag, TrackId};
use crate::report::{Limit, Measure, Metric};
use crate::thresholds::TrackingLimits;

/// The targets and the counted subjects of one keyframe, for IDF1.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    /// The labelled people the query should find, with their boxes.
    pub targets: Vec<(TrackId, Rect)>,
    /// The query's subjects counted there, with their boxes.
    pub found: Vec<(SubjectId, Rect)>,
}

/// IDF1's counts over a clip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Idf1 {
    /// Target keyframes matched by the subject paired with the person.
    pub idtp: u64,
    /// Targets over every keyframe.
    pub targets: u64,
    /// Counted subjects over every keyframe.
    pub found: u64,
}

impl Idf1 {
    /// `2 idtp / (targets + found)`; `None` with neither.
    pub fn value(&self) -> Option<f64> {
        let all = self.targets + self.found;
        (all > 0).then(|| 2.0 * self.idtp as f64 / all as f64)
    }
}

/// A wait for a person to be matched, in seconds of clip time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wait {
    /// How long it lasted.
    pub seconds: f64,
    /// Whether a match ended it (not the person's last keyframe).
    pub resolved: bool,
}

/// One person's waits; see the module docs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Waits {
    /// From first sight to first match (0 when matched at once); `None`
    /// when never a target.
    pub acquire: Option<Wait>,
    /// Every wait after a loss, in order.
    pub reacquire: Vec<Wait>,
}

/// Whether `track` holds `role` at `frame` in `labels`: as one of its
/// tags, or by a role interval covering the frame.
pub fn holds_role(labels: &LabelFile, track: TrackId, role: &RoleTag, frame: u64) -> bool {
    labels
        .tracks
        .iter()
        .any(|t| t.id == track && t.tags.contains(role))
        || labels.roles.iter().any(|r| {
            r.track == track && &r.role == role && (r.start_frame..r.end_frame).contains(&frame)
        })
}

/// Whether `p` lies inside `polygon` (even-odd rule; a point on an edge
/// may fall either side).
pub fn inside_polygon(polygon: &[Point], p: Point) -> bool {
    let mut inside = false;
    let n = polygon.len();
    for i in 0..n {
        let (a, b) = (polygon[i], polygon[(i + n - 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

/// Whether `p` lies in one of `labels`' exclusion zones.
pub fn in_exclusion_zone(labels: &LabelFile, p: Point) -> bool {
    labels
        .exclusions
        .iter()
        .any(|z| inside_polygon(&z.polygon, p))
}

/// Matches one keyframe's `targets` with the subjects `found` there, one
/// to one at `min_iou`, keeping `last` pairs that still overlap; see the
/// module docs. Sorted by target.
pub fn match_frame(
    targets: &[(TrackId, Rect)],
    found: &[(SubjectId, Rect)],
    last: &BTreeMap<TrackId, SubjectId>,
    min_iou: f64,
) -> Vec<(TrackId, SubjectId)> {
    let mut pairs = Vec::new();
    let mut target_free = vec![true; targets.len()];
    let mut found_free = vec![true; found.len()];
    for (ti, (track, tbox)) in targets.iter().enumerate() {
        let Some(subject) = last.get(track) else {
            continue;
        };
        if let Some(fi) = found.iter().position(|(s, _)| s == subject)
            && found_free[fi]
            && tbox.iou(&found[fi].1) >= min_iou
        {
            pairs.push((*track, *subject));
            target_free[ti] = false;
            found_free[fi] = false;
        }
    }
    let rows: Vec<usize> = (0..targets.len()).filter(|&i| target_free[i]).collect();
    let cols: Vec<usize> = (0..found.len()).filter(|&j| found_free[j]).collect();
    let cost: Vec<Vec<f64>> = rows
        .iter()
        .map(|&i| {
            cols.iter()
                .map(|&j| {
                    let iou = targets[i].1.iou(&found[j].1);
                    if iou >= min_iou { 1.0 - iou } else { FORBIDDEN }
                })
                .collect()
        })
        .collect();
    for (r, c) in minimize(&cost) {
        if cost[r][c] < FORBIDDEN {
            pairs.push((targets[rows[r]].0, found[cols[c]].0));
        }
    }
    pairs.sort_unstable();
    pairs
}

/// IDF1's counts over `frames` at `min_iou`; see the module docs.
pub fn idf1(frames: &[Frame], min_iou: f64) -> Idf1 {
    let mut common: BTreeMap<(TrackId, SubjectId), u64> = BTreeMap::new();
    let mut counts = Idf1::default();
    for f in frames {
        counts.targets += f.targets.len() as u64;
        counts.found += f.found.len() as u64;
        for (track, tbox) in &f.targets {
            for (subject, sbox) in &f.found {
                if tbox.iou(sbox) >= min_iou {
                    *common.entry((*track, *subject)).or_default() += 1;
                }
            }
        }
    }
    let tracks: Vec<TrackId> = common
        .keys()
        .map(|k| k.0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let subjects: Vec<SubjectId> = common
        .keys()
        .map(|k| k.1)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let cost: Vec<Vec<f64>> = tracks
        .iter()
        .map(|t| {
            subjects
                .iter()
                .map(|s| -(common.get(&(*t, *s)).copied().unwrap_or(0) as f64))
                .collect()
        })
        .collect();
    counts.idtp = minimize(&cost)
        .into_iter()
        .map(|(r, c)| common.get(&(tracks[r], subjects[c])).copied().unwrap_or(0))
        .sum();
    counts
}

/// One person's waits from `seen`: (clip PTS in nanoseconds, matched) on
/// every scored keyframe where they are a target, in order; see the
/// module docs.
pub fn waits(seen: &[(u64, bool)]) -> Waits {
    let mut waits = Waits::default();
    let mut open: Option<u64> = None;
    for &(t, matched) in seen {
        match (matched, open) {
            (false, None) => open = Some(t),
            (false, Some(_)) => {}
            (true, open_since) => {
                let wait = Wait {
                    seconds: open_since.map_or(0.0, |s| seconds_between_ns(s, t)),
                    resolved: true,
                };
                if waits.acquire.is_none() {
                    waits.acquire = Some(wait);
                } else if open_since.is_some() {
                    waits.reacquire.push(wait);
                }
                open = None;
            }
        }
    }
    if let (Some(start), Some(&(last, _))) = (open, seen.last()) {
        let wait = Wait {
            seconds: seconds_between_ns(start, last),
            resolved: false,
        };
        if waits.acquire.is_none() {
            waits.acquire = Some(wait);
        } else {
            waits.reacquire.push(wait);
        }
    }
    waits
}

/// What one query did over the scored keyframes.
#[derive(Debug, Default)]
struct Tally {
    /// Scored keyframes.
    keyframes: u64,
    /// Targets over them.
    targets: u64,
    /// Matched targets.
    matched: u64,
    /// Subjects matched to no target, outside the exclusion zones.
    false_subjects: u64,
    /// Identity switches.
    switches: u64,
    /// Each keyframe's targets and counted subjects, for IDF1.
    frames: Vec<Frame>,
    /// For each person, (clip PTS, matched) on each keyframe as a target.
    seen: BTreeMap<TrackId, Vec<(u64, bool)>>,
}

/// Tallies `query`, looking for holders of `role` (everyone for `None`),
/// over `join`.
fn tally(join: &Join, query: &QueryId, role: Option<&RoleTag>) -> Tally {
    let mut t = Tally::default();
    let mut last: BTreeMap<TrackId, SubjectId> = BTreeMap::new();
    for k in &join.keyframes {
        let keyframe = k.keyframe;
        let targets: Vec<(TrackId, Rect)> = keyframe
            .people
            .iter()
            .filter(|p| role.is_none_or(|r| holds_role(join.labels, p.track, r, keyframe.frame)))
            .map(|p| (p.track, p.bbox))
            .collect();
        let found: Vec<(SubjectId, Rect)> = join
            .source(k)
            .subjects
            .iter()
            .filter(|s| &s.query == query && s.state == TrackState::Tracking)
            .map(|s| (s.id, s.bbox))
            .collect();
        let pairs = match_frame(&targets, &found, &last, join.match_iou);
        for &(track, subject) in &pairs {
            if last
                .insert(track, subject)
                .is_some_and(|before| before != subject)
            {
                t.switches += 1;
            }
        }
        let matched: BTreeSet<SubjectId> = pairs.iter().map(|p| p.1).collect();
        let mut counted = Vec::new();
        for &(subject, bbox) in &found {
            if matched.contains(&subject) {
                counted.push((subject, bbox));
            } else if !in_exclusion_zone(join.labels, bbox.center()) {
                t.false_subjects += 1;
                counted.push((subject, bbox));
            }
        }
        for (track, _) in &targets {
            let hit = pairs.iter().any(|p| p.0 == *track);
            t.seen
                .entry(*track)
                .or_default()
                .push((keyframe.pts_ns, hit));
        }
        t.keyframes += 1;
        t.targets += targets.len() as u64;
        t.matched += pairs.len() as u64;
        t.frames.push(Frame {
            targets,
            found: counted,
        });
    }
    t
}

/// Every query to score: those the scope binds and those of any subject
/// of the scoped source in the trace, sorted.
fn queries(join: &Join) -> BTreeSet<QueryId> {
    let bound = join
        .scope
        .queries
        .iter()
        .map(|b: &QueryBinding| b.query.clone());
    let seen = join
        .trace
        .iter()
        .zip(&join.source_at)
        .filter_map(|(r, at)| r.sources.get((*at)?))
        .flat_map(|s| s.subjects.iter().map(|s| s.query.clone()));
    bound.chain(seen).collect()
}

/// Scores tracking over `join` into `out`, query by query; see the module
/// docs.
pub(crate) fn score(join: &Join, limits: &TrackingLimits, out: &mut Vec<Metric>) {
    for query in queries(join) {
        let t = tally(join, &query, join.scope.role(&query));
        let ratio = |num: u64, den: u64, none: &str| {
            if den == 0 {
                Measure::NotMeasurable(none.to_owned())
            } else {
                Measure::Value(num as f64 / den as f64)
            }
        };
        let recall = ratio(
            t.matched,
            t.targets,
            "no person the query should find is on a scored keyframe",
        );
        out.push(
            Metric::new(
                "tracking.recall",
                "fraction of people",
                recall,
                t.targets,
                Limit::min(limits.min_recall),
            )
            .for_query(&query),
        );
        let false_rate = ratio(t.false_subjects, t.keyframes, "no keyframe was scored");
        out.push(
            Metric::new(
                "tracking.false_subjects_per_frame",
                "subjects per keyframe",
                false_rate,
                t.keyframes,
                Limit::max(limits.max_false_subjects_per_frame),
            )
            .for_query(&query)
            .detail("false_subjects", t.false_subjects as f64),
        );
        out.push(
            Metric::count(
                "tracking.id_switches",
                "switches",
                t.switches,
                t.matched,
                Limit::max_count(limits.max_id_switches),
            )
            .for_query(&query),
        );
        let counts = idf1(&t.frames, join.match_iou);
        let measure = match counts.value() {
            Some(v) => Measure::Value(v),
            None => Measure::NotMeasurable(
                "no person the query should find and no subject of it on a scored keyframe".into(),
            ),
        };
        out.push(
            Metric::new(
                "tracking.idf1",
                "fraction",
                measure,
                counts.targets + counts.found,
                Limit::min(limits.min_idf1),
            )
            .for_query(&query)
            .detail("idtp", counts.idtp as f64),
        );
        let per_person: Vec<Waits> = t.seen.values().map(|s| waits(s)).collect();
        let nobody = "no person the query should find is on a scored keyframe";
        let acquire: Vec<Wait> = per_person.iter().filter_map(|w| w.acquire).collect();
        let longest = |ws: &[Wait]| ws.iter().map(|w| w.seconds).fold(0.0, f64::max);
        let unresolved = |ws: &[Wait]| ws.iter().filter(|w| !w.resolved).count() as f64;
        let measure = if acquire.is_empty() {
            Measure::NotMeasurable(nobody.into())
        } else {
            Measure::Value(longest(&acquire))
        };
        out.push(
            Metric::new(
                "tracking.time_to_acquire",
                "s",
                measure,
                acquire.len() as u64,
                Limit::max(limits.max_time_to_acquire_s),
            )
            .for_query(&query)
            .detail("never_acquired", unresolved(&acquire)),
        );
        let reacquire: Vec<Wait> = per_person
            .iter()
            .flat_map(|w| w.reacquire.clone())
            .collect();
        let measure = if per_person.is_empty() {
            Measure::NotMeasurable(nobody.into())
        } else {
            Measure::Value(longest(&reacquire))
        };
        out.push(
            Metric::new(
                "tracking.time_to_reacquire",
                "s",
                measure,
                reacquire.len() as u64,
                Limit::max(limits.max_time_to_reacquire_s),
            )
            .for_query(&query)
            .detail("unresolved", unresolved(&reacquire)),
        );
    }
}

#[cfg(test)]
mod tests;
