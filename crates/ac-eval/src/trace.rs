//! The per-tick trace: one [`TraceRecord`] for every engine tick, written as
//! JSON Lines.
//!
//! The metrics of spec V9 (crop containment, framing error, jerk, latency)
//! are functions of what the engine did on every tick. The framing core is
//! a pure function, observations → target → path, and a trace records each
//! stage of it per tick: where every subject's box was (the observations),
//! where every shot aimed (its *target*, the ideal crop for that box) and
//! where its crop actually was (the *path* the smoothing produced), plus
//! what the switcher put on program. Scorers then run on a file, offline,
//! with no engine.
//!
//! A record holds:
//!
//! * `tick` and `pts_ns`: the tick's number (1 for the first tick run, as in
//!   the engine's snapshots) and its master-clock media time, which is the
//!   PTS of the program frame it rendered;
//! * `late` and `skipped`: whether the tick started late and how many ticks
//!   the clock skipped just before it. These come from the runtime's clock,
//!   not from the deterministic core; a record built by the core alone
//!   says `false` and `0`;
//! * `sources`, in configuration order: each source's newest frame (its
//!   sequence number and PTS, so a box can be told apart from the tick that
//!   used it) and its subjects in ascending id, with box and track state;
//! * `shots`, in display order: each shot's source, subject, target and
//!   crop. The target is `null` while the subject is lost, because the
//!   framer then holds with nothing to aim at; a wide's target is its crop;
//! * `switcher`: the program and preview shots and, while a transition
//!   runs, how far it has got, as program draws it.
//!
//! Boxes, targets and crops are in normalized source coordinates (`x`, `y`,
//! `w`, `h`), as everywhere in the engine.
//!
//! # Format
//!
//! One record per line, as compact JSON ending in `\n`
//! ([`TraceRecord::to_json_line`]); [`parse_trace`] reads a whole file's
//! text back. A record, pretty-printed:
//!
//! ```json
//! {
//!   "version": 1, "tick": 42, "pts_ns": 1366666653, "late": false, "skipped": 0,
//!   "sources": [{
//!     "id": "stage", "frame": { "seq": 41, "pts_ns": 1366666666 },
//!     "subjects": [{ "id": 1, "query": "person", "bbox": { "x": 0.2, "y": 0.3, "w": 0.1, "h": 0.4 }, "state": "tracking" }]
//!   }],
//!   "shots": [
//!     { "id": "stage/wide", "source": "stage", "subject": null, "target": { "x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0 }, "crop": { "x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0 } },
//!     { "id": "stage/1/medium", "source": "stage", "subject": 1, "target": { "x": 0.1, "y": 0.25, "w": 0.4, "h": 0.4 }, "crop": { "x": 0.11, "y": 0.25, "w": 0.41, "h": 0.41 } }
//!   ],
//!   "switcher": { "program": "stage/wide", "preview": "stage/1/medium", "transition": { "base": "stage/wide", "overlay": 0.25, "dip": 0.0 } }
//! }
//! ```
//!
//! # Invariants
//!
//! * A record is a function of the engine core's state: the same core
//!   inputs give byte-identical lines. Fields are written in declaration
//!   order, every sequence has a fixed order, and there are no maps.
//! * Times are whole nanoseconds of media time (`*_ns`), so they read back
//!   exactly. Coordinates are written in the shortest form that reads back
//!   to the same number, so a line read and written again is unchanged.
//! * `version` is [`TRACE_VERSION`], and a line of another version is
//!   refused rather than misread. Readers ignore fields they do not know,
//!   so fields can be added within a version; removing, renaming or
//!   changing the meaning of one needs a new version.
//! * A non-finite number is written as `null` (serde_json's rule) and then
//!   does not read back; the engine's boxes and crops are always finite.

use std::fmt;
use std::time::Duration;

use ac_core::{QueryId, Rect, ShotId, SourceId, SubjectId, TrackState};
use serde::{Deserialize, Serialize};

/// Version of the trace format that this crate writes and reads.
pub const TRACE_VERSION: u32 = 1;

/// What the engine did on one tick; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceRecord {
    /// Format version, [`TRACE_VERSION`].
    pub version: u32,
    /// Number of the tick: 1 for the first tick run. Ticks the clock
    /// skipped have no number, so the numbers of consecutive records
    /// always differ by one; a gap means records were lost on the way.
    pub tick: u64,
    /// Master-clock media time of the tick, the PTS of its program frame.
    #[serde(rename = "pts_ns", with = "nanos")]
    pub pts: Duration,
    /// The tick started more than half a period after it was due (runtime).
    pub late: bool,
    /// Ticks the clock skipped just before this one, because the engine
    /// fell too far behind (runtime). Media time still advanced over them.
    pub skipped: u64,
    /// Every source, in configuration order.
    pub sources: Vec<SourceTrace>,
    /// Every shot, in display order.
    pub shots: Vec<ShotTrace>,
    /// Preview, program and the running transition.
    pub switcher: SwitcherTrace,
}

/// One source on one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceTrace {
    /// Source id.
    pub id: SourceId,
    /// The newest frame the engine has seen from the source (`None` before
    /// the first); the subjects' boxes are as of that frame or older.
    pub frame: Option<FrameTag>,
    /// Its subjects, in ascending id, hidden ones included.
    pub subjects: Vec<SubjectTrace>,
}

/// Identity of a source frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameTag {
    /// Sequence number within the source.
    pub seq: u64,
    /// Media time on the source's own clock.
    #[serde(rename = "pts_ns", with = "nanos")]
    pub pts: Duration,
}

/// One subject on one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubjectTrace {
    /// Id, unique within its source and never reused there.
    pub id: SubjectId,
    /// The query that created it.
    pub query: QueryId,
    /// Its newest box.
    pub bbox: Rect,
    /// Tracking or lost.
    pub state: TrackState,
}

/// One shot on one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShotTrace {
    /// Shot id.
    pub id: ShotId,
    /// The source it crops from.
    pub source: SourceId,
    /// The subject it frames; `None` for a wide.
    pub subject: Option<SubjectId>,
    /// The ideal crop this tick, for the subject's box: what the framer
    /// aimed at. `None` while the subject is lost; a wide's crop.
    pub target: Option<Rect>,
    /// The crop actually drawn, after smoothing.
    pub crop: Rect,
}

/// The switcher on one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwitcherTrace {
    /// The shot on program.
    pub program: Option<ShotId>,
    /// The shot on preview, which a running transition brings in.
    pub preview: Option<ShotId>,
    /// The running transition; `None` between transitions.
    pub transition: Option<TransitionTrace>,
}

/// How far a running transition has got, as program draws it.
///
/// Program draws `base` fully opaque, the preview shot over it with opacity
/// `overlay`, and black over both with opacity `dip`. A mix raises
/// `overlay` from 0 to 1 over the old program shot. A dip raises `dip` to 1
/// over the old program shot, then switches `base` to the preview shot and
/// lowers `dip` back to 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionTrace {
    /// The shot drawn first.
    pub base: Option<ShotId>,
    /// Opacity of the preview shot over `base`, in `0..=1`; 0 in a dip.
    pub overlay: f64,
    /// Opacity of black over the picture, in `0..=1`; 0 in a mix.
    pub dip: f64,
}

/// Why trace text could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceError {
    /// The 1-based line of the text that failed, for [`parse_trace`].
    pub line: Option<usize>,
    /// What is wrong with it.
    pub message: String,
}

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "trace line {line}: {}", self.message),
            None => write!(f, "trace record: {}", self.message),
        }
    }
}

impl std::error::Error for TraceError {}

impl TraceRecord {
    /// The record as one line of JSON Lines: compact JSON and a final `\n`.
    pub fn to_json_line(&self) -> String {
        // Every field is a derived struct, a newtype, a number, a bool or a
        // string, with no maps, so serializing cannot fail.
        let mut line = serde_json::to_string(self).expect("a trace record always serializes");
        line.push('\n');
        line
    }

    /// Reads one record from `line` (a trailing `\n` is allowed), refusing
    /// any other format version than [`TRACE_VERSION`].
    pub fn from_json_line(line: &str) -> Result<Self, TraceError> {
        let error = |message: String| TraceError {
            line: None,
            message,
        };
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| error(e.to_string()))?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v == u64::from(TRACE_VERSION) => {}
            Some(v) => {
                return Err(error(format!(
                    "format version {v} is not supported (this build reads {TRACE_VERSION})"
                )));
            }
            None => return Err(error("no format version".into())),
        }
        serde_json::from_value(value).map_err(|e| error(e.to_string()))
    }
}

/// Reads every record of a trace file's `text`, one per line, in order.
///
/// Every line must hold a record; the final `\n` may be missing. The first
/// line that does not read is the error, with its line number.
pub fn parse_trace(text: &str) -> Result<Vec<TraceRecord>, TraceError> {
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            TraceRecord::from_json_line(line).map_err(|e| TraceError {
                line: Some(i + 1),
                ..e
            })
        })
        .collect()
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
