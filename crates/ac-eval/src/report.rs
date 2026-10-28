//! The scorers' report: every metric with its value, its limit and a
//! verdict, written as JSON.
//!
//! # Verdicts
//!
//! Every [`Metric`] gets one [`Verdict`]:
//!
//! * `pass` or `fail`: the value against its [`Limit`] from the thresholds
//!   file ([`crate::thresholds`]), bounds inclusive, with a tolerance of
//!   [`TOLERANCE`] times the limit's size so that a value equal to the
//!   limit but for rounding passes;
//! * `unchecked`: the thresholds file sets no limit, so the value is
//!   reported and judged by nobody;
//! * `not_measurable`: the clip or the trace does not have what the metric
//!   needs (no on-air close-up, no labelled crown, a label the schema does
//!   not have yet), and the report says why in words instead of a value.
//!
//! The report [`passes`](Report::pass) when no metric fails: an unchecked
//! or unmeasurable metric does not fail a run, and the summary counts them
//! so that a run that measured nothing cannot pass unnoticed.
//!
//! # Format
//!
//! [`Report::to_json`] writes pretty-printed JSON. A metric reads
//!
//! ```json
//! { "name": "framing.head_torso_inside", "unit": "fraction of frames",
//!   "value": 1.0, "samples": 12, "limit": { "min": 0.99 }, "verdict": "pass" }
//! ```
//!
//! with `query` for a per-query metric, `not_measurable` (a reason) in
//! place of `value`, `limit` left out when there is none, and `details`
//! (named numbers that explain the value, such as a band's ends or a count
//! of unresolved events) when there are any.
//!
//! # Invariants
//!
//! * No number in a report is NaN or infinite: a metric built from a
//!   non-finite value is not measurable instead ([`Metric::new`]), and
//!   non-finite details are dropped ([`Metric::detail`]). JSON has no NaN,
//!   so the report always writes and reads back. Zero is always written
//!   as `0.0`, never `-0.0`.
//! * The report is a function of its metrics, which are in the scorers'
//!   fixed order; details are a sorted map. Equal inputs give equal bytes.

use std::collections::BTreeMap;

use ac_core::{QueryId, SourceId};
use serde::{Deserialize, Serialize};

use crate::labels::ClipId;

/// The report-format version this build writes.
pub const REPORT_VERSION: u32 = 1;

/// Relative slack in a limit comparison: a value within this fraction of
/// the limit's size (at least of 1) past the limit still keeps it.
pub const TOLERANCE: f64 = 1e-9;

/// A metric's value, or why it has none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Measure {
    /// The value, in the metric's unit; always finite.
    Value(f64),
    /// The metric could not be computed, and why.
    NotMeasurable(String),
}

/// The bound a metric must keep, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Limit {
    /// The value must be at least this.
    Min(f64),
    /// The value must be at most this.
    Max(f64),
}

impl Limit {
    /// A lower bound, when given.
    pub fn min(bound: Option<f64>) -> Option<Self> {
        bound.map(Self::Min)
    }

    /// An upper bound, when given.
    pub fn max(bound: Option<f64>) -> Option<Self> {
        bound.map(Self::Max)
    }

    /// An upper bound on a count, when given.
    pub fn max_count(bound: Option<u64>) -> Option<Self> {
        bound.map(|b| Self::Max(b as f64))
    }

    /// Whether `value` keeps the bound, within [`TOLERANCE`].
    pub fn admits(&self, value: f64) -> bool {
        match *self {
            Limit::Min(bound) => value >= bound - slack(bound),
            Limit::Max(bound) => value <= bound + slack(bound),
        }
    }
}

/// The tolerance on a comparison with `bound`.
fn slack(bound: f64) -> f64 {
    TOLERANCE * bound.abs().max(1.0)
}

/// `value`, with a negative zero (an empty sum, a product with a negative
/// factor) made positive, so the report never writes `-0.0`.
fn positive_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

/// What became of a metric; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The value keeps its limit.
    Pass,
    /// The value breaks its limit.
    Fail,
    /// No limit is set.
    Unchecked,
    /// There is no value.
    NotMeasurable,
}

/// One scored metric; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    /// Stable dotted name, such as `framing.headroom.close`.
    pub name: String,
    /// The query a per-query metric is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<QueryId>,
    /// The value's unit, in words.
    pub unit: String,
    /// The value, or why there is none.
    #[serde(flatten)]
    pub measure: Measure,
    /// How many observations the value is over (frames, ticks, takes,
    /// people), so a value over few can be told apart.
    pub samples: u64,
    /// The limit from the thresholds file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
    /// The value against the limit.
    pub verdict: Verdict,
    /// Named numbers that explain the value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, f64>,
}

impl Metric {
    /// A metric `name` in `unit`, judged against `limit`. A non-finite
    /// value becomes not measurable, so no report holds NaN.
    pub fn new(
        name: &str,
        unit: &str,
        measure: Measure,
        samples: u64,
        limit: Option<Limit>,
    ) -> Self {
        let measure = match measure {
            Measure::Value(v) if !v.is_finite() => {
                Measure::NotMeasurable(format!("the result, {v}, is not a finite number"))
            }
            Measure::Value(v) => Measure::Value(positive_zero(v)),
            m => m,
        };
        let verdict = match (&measure, limit) {
            (Measure::NotMeasurable(_), _) => Verdict::NotMeasurable,
            (Measure::Value(_), None) => Verdict::Unchecked,
            (Measure::Value(v), Some(l)) if l.admits(*v) => Verdict::Pass,
            (Measure::Value(_), Some(_)) => Verdict::Fail,
        };
        Self {
            name: name.to_owned(),
            query: None,
            unit: unit.to_owned(),
            measure,
            samples,
            limit,
            verdict,
            details: BTreeMap::new(),
        }
    }

    /// A count of events: always measurable.
    pub fn count(name: &str, unit: &str, count: u64, samples: u64, limit: Option<Limit>) -> Self {
        Self::new(name, unit, Measure::Value(count as f64), samples, limit)
    }

    /// The same metric, for `query`.
    pub fn for_query(mut self, query: &QueryId) -> Self {
        self.query = Some(query.clone());
        self
    }

    /// The same metric with the detail `key`; a non-finite value is left
    /// out.
    pub fn detail(mut self, key: &str, value: f64) -> Self {
        if value.is_finite() {
            self.details.insert(key.to_owned(), positive_zero(value));
        }
        self
    }

    /// The value, if measured.
    pub fn value(&self) -> Option<f64> {
        match self.measure {
            Measure::Value(v) => Some(v),
            Measure::NotMeasurable(_) => None,
        }
    }
}

/// How many metrics got each verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Summary {
    /// Metrics that kept their limit.
    pub pass: u64,
    /// Metrics that broke their limit.
    pub fail: u64,
    /// Metrics with no limit.
    pub unchecked: u64,
    /// Metrics with no value.
    pub not_measurable: u64,
}

/// How much of the clip and the trace the report rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Coverage {
    /// Records in the trace.
    pub ticks: u64,
    /// Keyframes in the label file.
    pub keyframes: u64,
    /// Keyframes a trace record showed, which the label-based metrics are
    /// scored on.
    pub scored_keyframes: u64,
}

/// The scored run; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Format version, [`REPORT_VERSION`].
    pub version: u32,
    /// The labelled clip.
    pub clip: ClipId,
    /// The trace source the clip played on.
    pub source: SourceId,
    /// No metric failed.
    pub pass: bool,
    /// Verdicts, counted.
    pub summary: Summary,
    /// What the scores rest on.
    pub coverage: Coverage,
    /// Every metric, in the scorers' order.
    pub metrics: Vec<Metric>,
}

impl Report {
    /// The report of `metrics`, with its summary and overall verdict.
    pub fn new(clip: ClipId, source: SourceId, coverage: Coverage, metrics: Vec<Metric>) -> Self {
        let mut summary = Summary::default();
        for m in &metrics {
            match m.verdict {
                Verdict::Pass => summary.pass += 1,
                Verdict::Fail => summary.fail += 1,
                Verdict::Unchecked => summary.unchecked += 1,
                Verdict::NotMeasurable => summary.not_measurable += 1,
            }
        }
        Self {
            version: REPORT_VERSION,
            clip,
            source,
            pass: summary.fail == 0,
            summary,
            coverage,
            metrics,
        }
    }

    /// The metric `name` (for `query`, when a per-query one).
    pub fn metric(&self, name: &str, query: Option<&str>) -> Option<&Metric> {
        self.metrics
            .iter()
            .find(|m| m.name == name && m.query.as_ref().map(|q| q.0.as_str()) == query)
    }

    /// Every failed metric, in order.
    pub fn failures(&self) -> impl Iterator<Item = &Metric> {
        self.metrics.iter().filter(|m| m.verdict == Verdict::Fail)
    }

    /// The report as pretty-printed JSON ending in a newline.
    pub fn to_json(&self) -> String {
        // Every number is finite (see the invariants) and every map has
        // string keys, so serializing cannot fail.
        let mut text = serde_json::to_string_pretty(self).expect("a report always serializes");
        text.push('\n');
        text
    }

    /// Reads a report back from [`Report::to_json`]'s text.
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests;
