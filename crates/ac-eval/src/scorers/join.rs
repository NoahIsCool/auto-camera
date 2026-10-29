//! The join of a label file and a trace: which tick showed each keyframe,
//! and whom each subject's box is of there.
//!
//! [`Join::new`] reads both once: for every record, where the scoped
//! source is among its sources and which clip frame its newest frame is
//! (counted from the scope's first frame, for a segment of the clip);
//! then, for every keyframe some record shows, the first such record and
//! each subject's identity there (see the scorers' module docs for both
//! rules). Every scorer reads this one join, so they all agree on which
//! tick and which person they are talking about.
//!
//! # Invariants
//!
//! * [`Join::keyframes`] follow the label file's order, which is frame
//!   order, and each names a record that has the scoped source with a
//!   frame; the records they name never go backwards.
//! * Identities hold only people labelled on that keyframe, each found at
//!   IoU at or above the match threshold; `framed` holds every identity
//!   plus, for a subject matching nobody, its last identity when that
//!   person is labelled there.

use std::collections::BTreeMap;

use ac_core::{ShotId, SubjectId};

use super::Scope;
use crate::labels::{Keyframe, LabelFile, PersonBox, TrackId};
use crate::trace::{ShotTrace, SourceTrace, TraceRecord};

/// The labels and the trace, joined; see the module docs.
#[derive(Debug)]
pub(crate) struct Join<'a> {
    /// The clip's labels.
    pub labels: &'a LabelFile,
    /// The run's trace, in tick order.
    pub trace: &'a [TraceRecord],
    /// How the two relate.
    pub scope: &'a Scope,
    /// IoU at or above which boxes match.
    pub match_iou: f64,
    /// For each record, the index of the scoped source in its sources.
    pub source_at: Vec<Option<usize>>,
    /// For each record, the clip frame the scoped source's newest frame
    /// is, when it has one: the scope's first frame plus the frame nearest
    /// its PTS.
    pub frame_at: Vec<Option<u64>>,
    /// The keyframes some record shows, in label order.
    pub keyframes: Vec<Scored<'a>>,
}

/// A keyframe and the tick that first showed it.
#[derive(Debug)]
pub(crate) struct Scored<'a> {
    /// The labelled frame.
    pub keyframe: &'a Keyframe,
    /// Index of the first record showing it.
    pub record: usize,
    /// Each subject of the scoped source on that record whose box matches
    /// a labelled person: the person it overlaps most.
    pub identity: BTreeMap<SubjectId, TrackId>,
    /// Each subject's framed person: its identity, or else its last one
    /// when that person is labelled on this keyframe.
    pub framed: BTreeMap<SubjectId, TrackId>,
}

impl<'a> Join<'a> {
    /// Joins `labels` and `trace` for `scope`, matching boxes at
    /// `match_iou`.
    pub fn new(
        labels: &'a LabelFile,
        trace: &'a [TraceRecord],
        scope: &'a Scope,
        match_iou: f64,
    ) -> Self {
        let source_at: Vec<Option<usize>> = trace
            .iter()
            .map(|r| r.sources.iter().position(|s| s.id == scope.source))
            .collect();
        let fps = labels.clip.fps;
        let first = scope.first_frame(fps);
        let frame_at: Vec<Option<u64>> = trace
            .iter()
            .zip(&source_at)
            .map(|(r, at)| {
                let frame = r.sources[(*at)?].frame?;
                let pts_ns = u64::try_from(frame.pts.as_nanos()).unwrap_or(u64::MAX);
                first?.checked_add(fps.nearest_frame(pts_ns)?)
            })
            .collect();
        let mut first_showing = BTreeMap::new();
        for (record, frame) in frame_at.iter().enumerate() {
            if let Some(frame) = frame {
                first_showing.entry(*frame).or_insert(record);
            }
        }
        let mut last: BTreeMap<SubjectId, TrackId> = BTreeMap::new();
        let keyframes = labels
            .keyframes
            .iter()
            .filter_map(|keyframe| {
                let record = *first_showing.get(&keyframe.frame)?;
                let source = &trace[record].sources[source_at[record]?];
                let identity = identities(keyframe, source, match_iou);
                let mut framed = BTreeMap::new();
                for subject in &source.subjects {
                    let person = identity.get(&subject.id).copied().or_else(|| {
                        last.get(&subject.id)
                            .copied()
                            .filter(|t| keyframe.people.iter().any(|p| p.track == *t))
                    });
                    if let Some(person) = person {
                        framed.insert(subject.id, person);
                    }
                }
                last.extend(identity.iter().map(|(s, t)| (*s, *t)));
                Some(Scored {
                    keyframe,
                    record,
                    identity,
                    framed,
                })
            })
            .collect();
        Self {
            labels,
            trace,
            scope,
            match_iou,
            source_at,
            frame_at,
            keyframes,
        }
    }

    /// The record that showed `k`.
    pub fn record(&self, k: &Scored<'a>) -> &'a TraceRecord {
        &self.trace[k.record]
    }

    /// The scoped source on the record that showed `k`.
    pub fn source(&self, k: &Scored<'a>) -> &'a SourceTrace {
        let at = self.source_at[k.record].expect("a scored keyframe's record has the source");
        &self.trace[k.record].sources[at]
    }

    /// The subject shots of the scoped source that `k`'s record puts on
    /// air, each with the person it frames there, if any.
    pub fn on_air_subject_shots(
        &self,
        k: &Scored<'a>,
    ) -> Vec<(&'a ShotTrace, Option<&'a PersonBox>)> {
        let record = self.record(k);
        on_air(record)
            .into_iter()
            .filter_map(|id| shot(record, id))
            .filter(|s| s.source == self.scope.source)
            .filter_map(|s| {
                let subject = s.subject?;
                let person = k.framed.get(&subject).and_then(|t| person(k.keyframe, *t));
                Some((s, person))
            })
            .collect()
    }
}

/// Each subject of `source` whose box matches a person labelled on
/// `keyframe` at `match_iou`, with the person it overlaps most (the first
/// listed on a tie).
fn identities(
    keyframe: &Keyframe,
    source: &SourceTrace,
    match_iou: f64,
) -> BTreeMap<SubjectId, TrackId> {
    source
        .subjects
        .iter()
        .filter_map(|s| {
            let mut best: Option<(f64, TrackId)> = None;
            for p in &keyframe.people {
                let iou = p.bbox.iou(&s.bbox);
                if iou >= match_iou && best.is_none_or(|(b, _)| iou > b) {
                    best = Some((iou, p.track));
                }
            }
            best.map(|(_, track)| (s.id, track))
        })
        .collect()
}

/// The person `track` on `keyframe`, if labelled there.
pub(crate) fn person(keyframe: &Keyframe, track: TrackId) -> Option<&PersonBox> {
    keyframe.people.iter().find(|p| p.track == track)
}

/// The shot `id` on `record`, if it exists there.
pub(crate) fn shot<'r>(record: &'r TraceRecord, id: &ShotId) -> Option<&'r ShotTrace> {
    record.shots.iter().find(|s| &s.id == id)
}

/// The shots program draws on `record`: the program shot between
/// transitions; during one, the base shot while black does not cover it
/// and the preview shot while it shows over the base (see
/// [`TransitionTrace`](crate::trace::TransitionTrace)). Base first, each
/// shot once.
pub fn on_air(record: &TraceRecord) -> Vec<&ShotId> {
    let switcher = &record.switcher;
    let Some(t) = &switcher.transition else {
        return switcher.program.iter().collect();
    };
    let mut shots = Vec::new();
    if t.dip < 1.0 {
        shots.extend(t.base.as_ref());
        if t.overlay > 0.0
            && let Some(preview) = &switcher.preview
            && !shots.contains(&preview)
        {
            shots.push(preview);
        }
    }
    shots
}
