//! Scorers: a trace of one run over a labelled clip, measured against the
//! clip's labels and judged against the thresholds.
//!
//! [`score`] is the entry point. It takes the clip's [`LabelFile`], the
//! run's trace ([`TraceRecord`]s, in tick order), a [`Scope`] saying how
//! the two relate, and the [`Thresholds`], and returns a [`Report`] with a
//! verdict per metric (spec V9, C.3, C.4). It is a pure function: the
//! runner reads the files and writes the report.
//!
//! # Joining labels and trace
//!
//! The label file describes one camera's picture; the trace, every source
//! of a studio. [`Scope::source`] names the trace source the clip played
//! on. Labels are on keyframes, by frame index and PTS from the clip's
//! first frame; a trace record names the newest frame of each source by its
//! PTS on the source's clock, which for a clip is the same clock. A record
//! *shows* a keyframe when the clip frame nearest its frame's PTS is the
//! keyframe's ([`FrameRate::nearest_frame`](crate::labels::FrameRate)), and
//! each keyframe is scored on the first record that shows it, the tick that
//! first rendered that frame. A keyframe no record shows (the output ran
//! slower than the clip, or the trace is shorter) is left out and counted
//! in the report's coverage.
//!
//! Everything a subject or a shot did on that tick is then compared with
//! the keyframe's people. Boxes are matched by IoU at or above
//! `tracking.match_iou` (0.5, the MOT Challenge's threshold), in two ways
//! for two questions:
//!
//! * **Who a query found** ([`tracking`]): its tracking subjects against
//!   the labelled people it should find, one to one, the way MOT metrics
//!   match (CLEAR MOT: last keyframe's pairs kept while they still overlap,
//!   the rest assigned for the most matches and then the most overlap).
//!   [`QueryBinding`] says whom a query should find: everyone, or the
//!   people holding a role ([`crate::labels`]). A query the scope does not
//!   bind is scored as looking for everyone.
//! * **Who a subject is**: the labelled person its box overlaps most,
//!   whatever the query, since the picture shows that person. This
//!   *identity* is what [`rest`] follows and what an on-air swap
//!   ([`switching`]) changes.
//! * **Whom a shot frames** ([`framing`]): its subject's identity; when
//!   the box matches nobody (the tracker drifted, or the subject is held
//!   after a loss), the person its subject last matched, if they are on
//!   the keyframe: a held crop that lost them is scored as losing them,
//!   not skipped.
//!
//! # On air
//!
//! The *on-air* shots of a tick are the ones program draws
//! ([`on_air`]): the program shot between transitions; during one, the
//! base shot and the incoming preview shot whenever each shows through
//! ([`crate::trace::TransitionTrace`]). Framing is scored on on-air subject
//! shots of the scoped source; motion on every subject shot of it, on air
//! or not, because the crop path is the framer's, whoever switches.
//! Switching is scored on the whole trace, every source.
//!
//! # Shot sizes
//!
//! Headroom bands and cut lines depend on the shot size, which the trace
//! does not carry. A shot with no subject is a wide. The engine names a
//! subject shot `source/subject/preset` (`ac_engine::catalog`), and the
//! built-in presets are named by their sizes (`medium`, `close`, ...);
//! [`Scope::preset_sizes`] gives the size of any other preset. A shot whose
//! size is unknown is left out of the size-dependent metrics.
//!
//! # Defined results
//!
//! No metric is ever NaN. Counts of events (jump cuts, switches, swaps) are
//! always measured, as 0 over no events; so are waits for events that never
//! had to happen (no loss on program waits 0 s for a fallback). Shares,
//! rates and peaks over samples that do not exist (a fraction of on-air
//! frames with none on air, an acceleration over a path of two ticks) are
//! [`NotMeasurable`](crate::report::Measure::NotMeasurable), with the reason.
//! An empty trace therefore scores no failure, and its report says it
//! measured nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use ac_core::{QueryId, ShotSize, SourceId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::labels::{FieldErrors, LabelFile, RoleTag};
use crate::report::{Coverage, Report};
use crate::thresholds::Thresholds;
use crate::trace::{ShotTrace, TraceRecord};

mod assign;
pub mod framing;
mod join;
pub mod motion;
pub mod rest;
pub mod switching;
pub mod tracking;

#[cfg(test)]
mod testkit;

pub use join::on_air;

/// How a trace relates to a label file; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    /// The trace source the labelled clip played on.
    pub source: SourceId,
    /// Whom each query should find; each query at most once.
    #[serde(default)]
    pub queries: Vec<QueryBinding>,
    /// The size of each preset whose id is not a size's name.
    #[serde(default)]
    pub preset_sizes: BTreeMap<String, ShotSize>,
    /// What the clip shows, which sets the minimum dwell.
    #[serde(default)]
    pub content: Content,
}

/// Whom one query should find.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryBinding {
    /// The query, as its subjects name it in the trace.
    pub query: QueryId,
    /// The role its people hold at the frame; `None` for everyone
    /// labelled.
    #[serde(default)]
    pub role: Option<RoleTag>,
}

/// What a clip shows, for the auto-director's minimum dwell (spec C.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    /// Speech: sermon, talk, prayer.
    #[default]
    Talk,
    /// Music: band and worship sets.
    Music,
}

/// Why a run could not be scored.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScoreError {
    /// The label file breaks a rule of [`crate::labels::validate`].
    #[error("the label file is invalid:\n{0}")]
    Labels(FieldErrors),
    /// The thresholds break a rule of [`crate::thresholds`].
    #[error("the thresholds are invalid:\n{0}")]
    Thresholds(FieldErrors),
    /// The scope binds one query twice.
    #[error("the scope binds query {0:?} twice")]
    DuplicateQuery(QueryId),
    /// The trace has records but none of them has the scoped source: the
    /// scope names the wrong source, or the trace is of another studio.
    #[error("no trace record has source {0:?}")]
    UnknownSource(SourceId),
}

impl Scope {
    /// A scope for `source`: talk, every query looking for everyone, and
    /// only the built-in preset names.
    pub fn new(source: SourceId) -> Self {
        Self {
            source,
            queries: Vec::new(),
            preset_sizes: BTreeMap::new(),
            content: Content::Talk,
        }
    }

    /// The role `query` looks for; `None` for everyone (also when the
    /// query is not bound).
    pub fn role(&self, query: &QueryId) -> Option<&RoleTag> {
        self.queries
            .iter()
            .find(|b| &b.query == query)
            .and_then(|b| b.role.as_ref())
    }

    /// The size of `shot`: a wide when it has no subject, else its
    /// preset's (the part of its id after the last `/`), from
    /// [`Self::preset_sizes`] or the preset's name; `None` when unknown.
    pub fn shot_size(&self, shot: &ShotTrace) -> Option<ShotSize> {
        if shot.subject.is_none() {
            return Some(ShotSize::Wide);
        }
        let preset = shot.id.0.rsplit('/').next().unwrap_or_default();
        self.preset_sizes
            .get(preset)
            .copied()
            .or_else(|| size_named(preset))
    }

    /// Checks that no query is bound twice.
    fn validate(&self) -> Result<(), ScoreError> {
        let mut seen = BTreeSet::new();
        for b in &self.queries {
            if !seen.insert(&b.query) {
                return Err(ScoreError::DuplicateQuery(b.query.clone()));
            }
        }
        Ok(())
    }
}

/// The subject shot size named `name` in the config's words; never a wide.
fn size_named(name: &str) -> Option<ShotSize> {
    match name {
        "full" => Some(ShotSize::Full),
        "medium" => Some(ShotSize::Medium),
        "medium_close" => Some(ShotSize::MediumClose),
        "close" => Some(ShotSize::Close),
        _ => None,
    }
}

/// Seconds from `from` to `to`, zero when `to` is earlier.
fn seconds_between(from: Duration, to: Duration) -> f64 {
    to.saturating_sub(from).as_secs_f64()
}

/// Seconds from `from_ns` to `to_ns`, zero when `to_ns` is earlier.
fn seconds_between_ns(from_ns: u64, to_ns: u64) -> f64 {
    seconds_between(Duration::from_nanos(from_ns), Duration::from_nanos(to_ns))
}

/// Scores `trace` against `labels` and `thresholds`; see the module docs.
///
/// Fails only on input it cannot score: invalid labels or thresholds, a
/// query bound twice, or a non-empty trace that never shows the scoped
/// source. Every metric of [`framing`], [`motion`], [`rest`],
/// [`tracking`] (one set per query) and [`switching`] is in the report, in
/// that order.
pub fn score(
    labels: &LabelFile,
    trace: &[TraceRecord],
    scope: &Scope,
    thresholds: &Thresholds,
) -> Result<Report, ScoreError> {
    labels.validate().map_err(ScoreError::Labels)?;
    thresholds.validate().map_err(ScoreError::Thresholds)?;
    scope.validate()?;
    let has_source = trace
        .iter()
        .any(|r| r.sources.iter().any(|s| s.id == scope.source));
    if !trace.is_empty() && !has_source {
        return Err(ScoreError::UnknownSource(scope.source.clone()));
    }
    let join = join::Join::new(labels, trace, scope, thresholds.tracking.match_iou);
    let mut metrics = Vec::new();
    framing::score(&join, &thresholds.framing, &mut metrics);
    motion::score(&join, &thresholds.motion, &mut metrics);
    rest::score(&join, &thresholds.motion, &mut metrics);
    tracking::score(&join, &thresholds.tracking, &mut metrics);
    switching::score(&join, &thresholds.switching, &mut metrics);
    let coverage = Coverage {
        ticks: trace.len() as u64,
        keyframes: labels.keyframes.len() as u64,
        scored_keyframes: join.keyframes.len() as u64,
    };
    Ok(Report::new(
        labels.clip.id.clone(),
        scope.source.clone(),
        coverage,
        metrics,
    ))
}

#[cfg(test)]
mod tests;
