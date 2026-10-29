//! Replaying recorded detector passes: the detection log and
//! [`ReplayDetector`].
//!
//! Scoring framing every night (spec V9) must not run a model in CI, and
//! must give the same numbers every night. So the passes a run's detector
//! made are recorded once, as a *detection log*, and played back: a
//! [`ReplayDetector`] answers each request from the log instead of from a
//! model, through the same [`Detector`] trait, so everything after the
//! detector (subject store, tracker seeding, framing, switching) runs
//! unchanged on the recorded boxes.
//!
//! # The log
//!
//! One [`PassRecord`] per pass that the engine *applied* (stale and failed
//! passes changed nothing, so they are not part of what a run saw), as
//! JSON Lines ([`PassRecord::to_json_line`], [`parse_pass_log`]). A record
//! holds the source, the sequence number and PTS of the frame the pass
//! looked at, the ids of the queries it looked for, and its detections
//! (query, box, score, extent):
//!
//! ```json
//! {"version":1,"source":"stage","seq":41,"pts_ns":1366666666,"queries":["person"],
//!  "detections":[{"query":"person","bbox":{"x":0.2,"y":0.3,"w":0.1,"h":0.4},"score":0.9,"extent":"object"}]}
//! ```
//!
//! (one line in the file; wrapped here). Times are whole nanoseconds, and
//! boxes and scores are written in the shortest form that reads back to the
//! same number, so a replayed box is bit for bit the recorded one. Every
//! line carries `version`, [`PASS_LOG_VERSION`]; a line of another version
//! is refused rather than misread. Readers ignore fields they do not know.
//!
//! # Answering by PTS
//!
//! A request on a frame is answered with the pass of the frame's source
//! whose PTS is nearest the frame's, when it is at most the replay's
//! `tolerance` away; of two equally near passes the earlier one wins, and
//! of passes with the same PTS the one listed first. Sequence numbers are
//! not compared: they count the frames one run happened to deliver, while
//! the PTS is the frame's position in the media, the same in every run
//! over the same input (a file segment counts PTS from its own start, so a
//! log replays over the segment it was recorded on). The eval runner's
//! tolerance is half the source's frame interval unless it is told
//! otherwise, so that a request only ever matches a pass on the same frame;
//! a log recorded live, whose passes looked at whichever frames real time
//! handed the detector, is replayed with a wider one.
//!
//! A request without such a pass is a *miss*, [`VisionError::NotRecorded`].
//! The engine takes it as a failed pass, which changes no subject, so a
//! pass missing from the log skips exactly that update and nothing else.
//! A hit answers with the recorded detections of the queries asked for,
//! through [`sanitize_detections`], so the answer honours the detector
//! contract like any model's; a query the recorded pass did not look for
//! gets no detections.
//!
//! # Invariants
//!
//! * The answer is a function of the log, the tolerance, the frame's source
//!   and PTS and the queries asked for. Nothing changes between calls, so a
//!   replay is deterministic and can be repeated.
//! * A replay detector only ever answers with passes of its own source: a
//!   log recorded on another source, or a frame of another source, misses.
//! * Nothing here reads a file or a clock; the caller reads the log's text.

use std::fmt;
use std::time::Duration;

use ac_core::{QueryId, SourceId};
use ac_media::VideoFrame;
use serde::{Deserialize, Serialize};

use crate::api::{Detection, Detector, PromptQuery, sanitize_detections};
use crate::error::VisionError;

/// Version of the detection log format that this crate writes and reads.
pub const PASS_LOG_VERSION: u32 = 1;

/// One applied detector pass, a line of the detection log; see the module
/// docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PassRecord {
    /// Format version, [`PASS_LOG_VERSION`].
    pub version: u32,
    /// Source of the frame the pass looked at.
    pub source: SourceId,
    /// Sequence number of that frame.
    pub seq: u64,
    /// PTS of that frame, on the source's media clock.
    #[serde(rename = "pts_ns", with = "nanos")]
    pub pts: Duration,
    /// Ids of the queries the pass looked for.
    pub queries: Vec<QueryId>,
    /// What it found, in the detector's order.
    pub detections: Vec<Detection>,
}

impl PassRecord {
    /// A record of the current version for the pass on frame `seq` at `pts`
    /// of `source`, which looked for `queries` and found `detections`.
    pub fn new(
        source: SourceId,
        seq: u64,
        pts: Duration,
        queries: Vec<QueryId>,
        detections: Vec<Detection>,
    ) -> Self {
        Self {
            version: PASS_LOG_VERSION,
            source,
            seq,
            pts,
            queries,
            detections,
        }
    }

    /// The record as one line of JSON Lines: compact JSON and a final `\n`.
    pub fn to_json_line(&self) -> String {
        // Ids, numbers and derived structs only, with no maps, so
        // serializing cannot fail.
        let mut line = serde_json::to_string(self).expect("a pass record always serializes");
        line.push('\n');
        line
    }

    /// Reads one record from `line` (a trailing `\n` is allowed), refusing
    /// any other format version than [`PASS_LOG_VERSION`].
    pub fn from_json_line(line: &str) -> Result<Self, PassLogError> {
        let error = |message: String| PassLogError {
            line: None,
            message,
        };
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| error(e.to_string()))?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v == u64::from(PASS_LOG_VERSION) => {}
            Some(v) => {
                return Err(error(format!(
                    "format version {v} is not supported (this build reads {PASS_LOG_VERSION})"
                )));
            }
            None => return Err(error("no format version".into())),
        }
        serde_json::from_value(value).map_err(|e| error(e.to_string()))
    }
}

/// Reads every record of a detection log's `text`, one per line, in order.
///
/// Blank lines are skipped, so a log edited by hand (a pass deleted) still
/// reads; every other line must hold a record. The first line that does not
/// read is the error, with its 1-based line number.
pub fn parse_pass_log(text: &str) -> Result<Vec<PassRecord>, PassLogError> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            PassRecord::from_json_line(line).map_err(|e| PassLogError {
                line: Some(i + 1),
                ..e
            })
        })
        .collect()
}

/// Why detection log text could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassLogError {
    /// The 1-based line of the text that failed, for [`parse_pass_log`].
    pub line: Option<usize>,
    /// What is wrong with it.
    pub message: String,
}

impl fmt::Display for PassLogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "detection log line {line}: {}", self.message),
            None => write!(f, "detection log record: {}", self.message),
        }
    }
}

impl std::error::Error for PassLogError {}

/// A [`Detector`] that answers from a detection log; see the module docs.
#[derive(Debug, Clone)]
pub struct ReplayDetector {
    /// The source whose passes it replays.
    source: SourceId,
    /// That source's passes, by PTS; passes with equal PTS in log order.
    passes: Vec<PassRecord>,
    /// Farthest a pass's PTS may be from a request's.
    tolerance: Duration,
}

impl ReplayDetector {
    /// A replay of the passes of `source` in `log` (passes of other sources
    /// are left out), answering a request with a pass at most `tolerance`
    /// from its PTS.
    pub fn new(
        source: SourceId,
        log: impl IntoIterator<Item = PassRecord>,
        tolerance: Duration,
    ) -> Self {
        let mut passes: Vec<PassRecord> = log.into_iter().filter(|p| p.source == source).collect();
        // Stable, so passes with the same PTS keep their order in the log.
        passes.sort_by_key(|p| p.pts);
        Self {
            source,
            passes,
            tolerance,
        }
    }

    /// The source whose passes it replays.
    pub fn source(&self) -> &SourceId {
        &self.source
    }

    /// Its passes, by PTS.
    pub fn passes(&self) -> &[PassRecord] {
        &self.passes
    }

    /// Farthest a pass's PTS may be from a request's.
    pub fn tolerance(&self) -> Duration {
        self.tolerance
    }

    /// The pass that answers a request at `pts`: the nearest, within the
    /// tolerance, the earlier of two equally near ones and the first listed
    /// of passes with the same PTS. `None` is a miss.
    pub fn find(&self, pts: Duration) -> Option<&PassRecord> {
        // The first pass at or after `pts`, and the first listed of the
        // passes sharing the PTS of the last one before it.
        let after = self.passes.partition_point(|p| p.pts < pts);
        let before = after.checked_sub(1).map(|last| {
            let at = self.passes[last].pts;
            self.passes.partition_point(|p| p.pts < at)
        });
        let distance = |i: usize| self.passes[i].pts.abs_diff(pts);
        let best = match (before, (after < self.passes.len()).then_some(after)) {
            (Some(b), Some(a)) if distance(a) < distance(b) => a,
            (Some(b), _) => b,
            (None, Some(a)) => a,
            (None, None) => return None,
        };
        (distance(best) <= self.tolerance).then(|| &self.passes[best])
    }
}

impl Detector for ReplayDetector {
    fn name(&self) -> &str {
        "replay"
    }

    fn detect(
        &mut self,
        frame: &VideoFrame,
        queries: &[PromptQuery],
    ) -> Result<Vec<Detection>, VisionError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let pass = (frame.source == self.source)
            .then(|| self.find(frame.pts))
            .flatten()
            .ok_or_else(|| VisionError::NotRecorded {
                id: frame.source.clone(),
                pts: frame.pts,
            })?;
        Ok(sanitize_detections(pass.detections.clone(), queries))
    }
}

/// Serde for a [`Duration`] as whole nanoseconds, which read back exactly.
/// A duration longer than `u64::MAX` nanoseconds (584 years) is written as
/// `u64::MAX`.
mod nanos {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    /// Writes `d` as a `u64` of nanoseconds.
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
    }

    /// Reads a `u64` of nanoseconds.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_nanos)
    }
}

#[cfg(test)]
mod tests;
