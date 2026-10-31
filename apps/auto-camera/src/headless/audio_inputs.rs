//! The audio inputs in the summary, and what drift correction made of each
//! at the end of the run (spec C.3, A/V sync drift-free over a show): the
//! correction it ended on, in ppm to one decimal (below zero for a board
//! whose clock runs fast, zero while its blocks pass as they are), how
//! often a break in its timestamps (a reconnection, a long loss) or a new
//! format restarted the measurement, and how many smaller steps in them
//! (audio lost, timestamps that went back) it made up with silence or a
//! cut. Over a long run with a board as program audio, a correction that
//! settles near minus the board's drift (about −37 ppm for a board 37 ppm
//! fast, which cancels it), with no resets and no steps, and an output that
//! counted no inserted silence, say that the board was kept in sync. An
//! input is named by its id and name, and marked when it is program audio;
//! nothing else of its health is here. A studio without audio inputs has an
//! empty list.

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
    /// It was the program audio's origin.
    pub program: bool,
    /// Drift correction at the end, in ppm to one decimal.
    pub correction_ppm: f64,
    /// Breaks and format changes that restarted the drift measurement.
    pub drift_resets: u64,
    /// Steps in its timestamps made up with silence or a cut.
    pub drift_steps: u64,
}

impl From<&AudioInputView> for AudioInputSummary {
    fn from(v: &AudioInputView) -> Self {
        Self {
            id: v.id.0.clone(),
            name: v.name.clone(),
            program: v.program,
            correction_ppm: round(v.correction_ppm, 1),
            drift_resets: v.drift_resets,
            drift_steps: v.drift_steps,
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
    /// `drift_resets`, and two steps made up.
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
            unheard: false,
            correction_ppm,
            drift_resets,
            drift_steps: 2,
        }
    }

    #[test]
    fn each_input_reports_its_drift_correction() {
        let mut snap = snapshot();
        snap.audio_inputs = vec![board(-37.2649, 1)];
        let summary = audio_input_summaries(Some(&snap));
        assert_eq!(summary.len(), 1);
        let s = &summary[0];
        assert_eq!((s.id.as_str(), s.name.as_str()), ("board", "Soundboard"));
        assert_eq!(
            (s.program, s.correction_ppm, s.drift_resets, s.drift_steps),
            (true, -37.3, 1, 2)
        );
        // Only the drift correction, by the input's id and name.
        let json = serde_json::to_value(s).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "correction_ppm",
                "drift_resets",
                "drift_steps",
                "id",
                "name",
                "program"
            ]
        );
        assert_eq!(json["correction_ppm"], -37.3);
        assert_eq!(json["drift_resets"], 1);
        assert_eq!(json["drift_steps"], 2);
    }

    #[test]
    fn an_input_not_connected_yet_has_no_correction() {
        let mut snap = snapshot();
        snap.audio_inputs = vec![AudioInputView {
            state: SourceState::Offline,
            error: Some("board.wav does not exist".into()),
            format: None,
            program: false,
            drift_steps: 0,
            ..board(0.0, 0)
        }];
        let s = &audio_input_summaries(Some(&snap))[0];
        assert!(!s.program);
        assert_eq!(
            (s.correction_ppm, s.drift_resets, s.drift_steps),
            (0.0, 0, 0)
        );
    }

    #[test]
    fn a_studio_without_audio_inputs_or_snapshots_lists_none() {
        assert!(audio_input_summaries(Some(&snapshot())).is_empty());
        assert!(audio_input_summaries(None).is_empty());
    }
}
