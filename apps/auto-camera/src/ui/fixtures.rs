//! Snapshots for UI tests, built by a real engine core so they stay in step
//! with the engine's types.

use std::path::PathBuf;
use std::time::Duration;

use ac_core::{Rect, ShotId, SourceId, SubjectId};
use ac_director::CandidateKind;
use ac_engine::{
    EngineCore, EngineSnapshot, OutputView, ShotStatus, ShotView, StudioConfig, Tally, TimingView,
};
use ac_media::SourceInfo;
use ac_output::{EncoderChoice, EncoderStats, Progress};
use ac_sources::SourceConfig;

/// A snapshot of a fresh one-source studio: the wide shot, nothing on air.
pub fn snapshot() -> EngineSnapshot {
    fresh(false)
}

/// [`snapshot`] of a studio whose source has a detector, still preparing.
pub fn preparing_snapshot() -> EngineSnapshot {
    fresh(true)
}

/// A fresh one-source studio's snapshot, with a detector when `detector`.
fn fresh(detector: bool) -> EngineSnapshot {
    let config = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 1,
        },
    );
    let info = SourceInfo {
        id: SourceId("stage".into()),
        width: 320,
        height: 180,
        fps: 30.0,
        audio: None,
    };
    let core = EngineCore::new(&config, vec![info], &[detector]).expect("core");
    core.snapshot(OutputView::default(), TimingView::default(), true)
}

/// A subject shot view with the given status and quality.
pub fn subject_shot(id: &str, status: ShotStatus, quality: f32) -> ShotView {
    ShotView {
        id: ShotId(id.into()),
        name: format!("Person {id}"),
        source: SourceId("stage".into()),
        kind: CandidateKind::Subject,
        subject: Some(SubjectId(1)),
        preset: Some("medium".into()),
        crop: Rect::new(0.25, 0.25, 0.5, 0.5),
        quality,
        settled: 1.0,
        status,
        available: status != ShotStatus::Unavailable,
        tally: Tally::default(),
        thumbnail: None,
    }
}

/// Encoder statistics of a healthy recording.
pub fn encoder_stats() -> EncoderStats {
    EncoderStats {
        encoder: EncoderChoice::Libx264,
        frames_in: 300,
        frames_dropped: 2,
        frames_repeated: 2,
        frames_written: 300,
        audio_samples_in: 0,
        audio_samples_dropped: 0,
        audio_silence_samples: 0,
        audio_samples_written: 0,
        audio_connected: false,
        ffmpeg_alive: true,
        exit_code: None,
        last_error: None,
        progress: Progress {
            frame: 298,
            fps: Some(30.0),
            bitrate_kbps: Some(3999.6),
            total_size: None,
            out_time: Some(Duration::from_secs(9)),
            dup_frames: 0,
            drop_frames: 0,
            speed: Some(1.0),
            ended: false,
        },
    }
}

/// An output view recording to `path`.
pub fn recording(path: &str) -> OutputView {
    OutputView {
        record_path: Some(PathBuf::from(path)),
        ..OutputView::default()
    }
}
