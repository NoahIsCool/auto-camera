//! The audio inputs in the summary: each one's health and counters at the
//! end of the run, and what drift correction made of it (spec C.3, A/V
//! sync drift-free over a show): the correction it ended on, in ppm to one
//! decimal (below zero for a board whose clock runs fast, zero while its
//! blocks pass as they are), and how often a break in its timestamps (a
//! reconnection, lost audio) or a new format restarted the measurement.
//! Over a long run with a board as program audio, a correction that
//! settles near the board's drift with no resets, and an output that
//! counted no inserted silence, say that the board was kept in sync. A
//! studio without audio inputs has an empty list.

use ac_engine::{AudioInputView, EngineSnapshot};
use serde::Serialize;

use super::report::round;

/// One audio input in the summary; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioInputSummary {
    /// Its id.
    pub id: String,
    /// Operator-facing name.
    pub name: String,
    /// `starting`, `running`, `offline`, `reconnecting` or `stopped`.
    pub state: String,
    /// Why it was offline, while it was.
    pub error: Option<String>,
    /// Sample rate of its newest connection, once one opened.
    pub sample_rate: Option<u32>,
    /// Channels of its newest connection, once one opened.
    pub channels: Option<u16>,
    /// It was the program audio's origin.
    pub program: bool,
    /// Blocks drained from it.
    pub blocks: u64,
    /// Blocks its ring dropped because they were not drained in time.
    pub dropped: u64,
    /// Program blocks dropped because they lacked the picked channels.
    pub unusable: u64,
    /// Drift correction at the end, in ppm to one decimal.
    pub correction_ppm: f64,
    /// Breaks and format changes that restarted the drift measurement.
    pub drift_resets: u64,
}

impl From<&AudioInputView> for AudioInputSummary {
    fn from(v: &AudioInputView) -> Self {
        Self {
            id: v.id.0.clone(),
            name: v.name.clone(),
            state: format!("{:?}", v.state).to_lowercase(),
            error: v.error.clone(),
            sample_rate: v.format.map(|(rate, _)| rate),
            channels: v.format.map(|(_, channels)| channels),
            program: v.program,
            blocks: v.blocks,
            dropped: v.dropped,
            unusable: v.unusable,
            correction_ppm: round(v.correction_ppm, 1),
            drift_resets: v.drift_resets,
        }
    }
}

/// The audio inputs as the run's final snapshot `end` shows them.
pub fn audio_input_summaries(end: Option<&EngineSnapshot>) -> Vec<AudioInputSummary> {
    end.map(|s| s.audio_inputs.iter().map(AudioInputSummary::from).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use ac_core::SourceId;
    use ac_engine::{EngineCore, OutputView, StudioConfig, TimingView};
    use ac_media::{SourceInfo, SourceState};
    use ac_sources::SourceConfig;

    use super::*;

    /// A one-source studio's first snapshot.
    fn snapshot() -> EngineSnapshot {
        let config = StudioConfig::with_source(
            "stage",
            SourceConfig::Synthetic {
                width: 64,
                height: 36,
                fps: 30.0,
                subjects: 0,
            },
        );
        let info = SourceInfo {
            id: SourceId("stage".into()),
            width: 64,
            height: 36,
            fps: 30.0,
            audio: None,
        };
        let core = EngineCore::new(&config, vec![info], &[true]).unwrap();
        core.snapshot(OutputView::default(), TimingView::default(), true)
    }

    /// A board as the engine shows it, with `correction_ppm` and
    /// `drift_resets`.
    fn board(correction_ppm: f64, drift_resets: u64) -> AudioInputView {
        AudioInputView {
            id: SourceId("board".into()),
            name: "Soundboard".into(),
            state: SourceState::Running,
            error: None,
            format: Some((48_000, 16)),
            pick: None,
            program: true,
            blocks: 1_080_000,
            dropped: 2,
            unusable: 0,
            correction_ppm,
            drift_resets,
        }
    }

    #[test]
    fn each_input_reports_its_drift_correction() {
        let mut snap = snapshot();
        snap.audio_inputs = vec![board(-37.2649, 1)];
        let summary = audio_input_summaries(Some(&snap));
        assert_eq!(summary.len(), 1);
        let s = &summary[0];
        assert_eq!((s.id.as_str(), s.state.as_str()), ("board", "running"));
        assert_eq!((s.sample_rate, s.channels), (Some(48_000), Some(16)));
        assert_eq!((s.correction_ppm, s.drift_resets), (-37.3, 1));
        assert_eq!((s.blocks, s.dropped, s.program), (1_080_000, 2, true));
        let json = serde_json::to_value(s).unwrap();
        assert_eq!(json["correction_ppm"], -37.3);
        assert_eq!(json["drift_resets"], 1);
    }

    #[test]
    fn an_input_not_connected_yet_has_no_format_and_no_correction() {
        let mut snap = snapshot();
        snap.audio_inputs = vec![AudioInputView {
            state: SourceState::Offline,
            error: Some("board.wav does not exist".into()),
            format: None,
            ..board(0.0, 0)
        }];
        let s = &audio_input_summaries(Some(&snap))[0];
        assert_eq!((s.sample_rate, s.channels), (None, None));
        assert_eq!(s.state, "offline");
        assert_eq!(s.error.as_deref(), Some("board.wav does not exist"));
        assert_eq!((s.correction_ppm, s.drift_resets), (0.0, 0));
    }

    #[test]
    fn a_studio_without_audio_inputs_or_snapshots_lists_none() {
        assert!(audio_input_summaries(Some(&snapshot())).is_empty());
        assert!(audio_input_summaries(None).is_empty());
    }
}
