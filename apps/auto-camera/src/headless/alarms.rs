//! The alarms of a headless run: every alarm the run raised, for the
//! summary's `alarms`.
//!
//! Nobody watches a headless run, so nothing is acknowledged and a list of
//! the alarms active at the end would miss everything that came and went
//! (a stream that reconnected, a burst of encoder drops). [`AlarmLog`]
//! therefore runs the engine's alarm model ([`ac_engine::alarms`]) over
//! the snapshots the run samples and keeps one entry per alarm raised, in
//! raise order, with when it was raised and, once its cause cleared, when
//! that was; an alarm still raised when the run ended has no clear time.
//! The model's rules are written for a reader that samples, so the run's
//! 20 ms sampling raises and clears what a window would.

use ac_engine::{AlarmCause, AlarmChange, Alarms, EngineSnapshot};
use serde::Serialize;

use super::report::round;

/// One alarm in the summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlarmSummary {
    /// What it was about: `program_black`, `program_subject_lost`,
    /// `program_source_stale`, `encoder_drops`, `output_failed`,
    /// `stream_reconnecting`, `stream_down` or `recording_without_audio`.
    pub kind: String,
    /// What was wrong, as the window would have said it (its newest
    /// wording, such as the last reconnection attempt).
    pub message: String,
    /// Master-clock seconds at which it was raised.
    pub raised_s: f64,
    /// Master-clock seconds at which its cause cleared; `null` when it was
    /// still raised at the end of the run.
    pub cleared_s: Option<f64>,
}

/// The alarm model over a run's samples, and every alarm it raised; see
/// the module docs.
#[derive(Debug, Default)]
pub(super) struct AlarmLog {
    /// The model.
    alarms: Alarms,
    /// Every alarm raised, in raise order, with its cause while it is
    /// still raised (`None` once cleared).
    entries: Vec<(Option<AlarmCause>, AlarmSummary)>,
}

impl AlarmLog {
    /// Takes in one sampled snapshot.
    pub(super) fn observe(&mut self, snap: &EngineSnapshot) {
        for change in self.alarms.observe(snap) {
            match change {
                AlarmChange::Raised(alarm) => self.entries.push((
                    Some(alarm.cause.clone()),
                    AlarmSummary {
                        kind: alarm.cause.kind().to_owned(),
                        message: alarm.message,
                        raised_s: round(alarm.raised_at.as_secs_f64(), 3),
                        cleared_s: None,
                    },
                )),
                AlarmChange::Cleared { alarm, at } => {
                    let open = self
                        .entries
                        .iter_mut()
                        .rev()
                        .find(|(cause, _)| cause.as_ref() == Some(&alarm.cause));
                    if let Some((cause, entry)) = open {
                        *cause = None;
                        entry.message = alarm.message;
                        entry.cleared_s = Some(round(at.as_secs_f64(), 3));
                    }
                }
            }
        }
    }

    /// The summary's `alarms`: every alarm raised, in raise order; those
    /// still raised carry their newest message.
    pub(super) fn summary(&self) -> Vec<AlarmSummary> {
        self.entries
            .iter()
            .map(|(cause, entry)| {
                let mut entry = entry.clone();
                if let Some(alarm) = cause.as_ref().and_then(|c| self.alarms.get(c)) {
                    entry.message.clone_from(&alarm.message);
                }
                entry
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
