//! Snapshots for the log module's tests, built by a real engine core so
//! they stay in step with the engine's types.

use std::time::Duration;

use ac_core::SourceId;
use ac_engine::{EngineCore, EngineSnapshot, OutputView, QuerySettings, StudioConfig, TimingView};
use ac_media::SourceInfo;
use ac_sidecar::{Capabilities, Hello, HostState, HostStats, HostStatus};
use ac_sources::SourceConfig;

/// A fresh one-source studio ("stage") with one query and a detector
/// that is still preparing; nothing on air.
pub fn snapshot() -> EngineSnapshot {
    let mut config = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 1,
        },
    );
    config
        .queries
        .push(QuerySettings::new("Speaker", "speaker upper body"));
    let info = SourceInfo {
        id: SourceId("stage".into()),
        width: 320,
        height: 180,
        fps: 30.0,
        audio: None,
    };
    let core = EngineCore::new(&config, vec![info], &[true]).expect("core");
    core.snapshot(OutputView::default(), TimingView::default(), true)
}

/// The status of a sidecar host that is ready after one restart.
pub fn sidecar() -> HostStatus {
    HostStatus {
        state: HostState::Ready,
        pid: Some(4242),
        hello: Some(Hello {
            protocol: 1,
            model: "sam3.1-mlx".into(),
            weights_sha256: None,
            input_side: 1008,
            capabilities: Capabilities {
                masks: true,
                parts: false,
            },
        }),
        stats: HostStats {
            spawns: 2,
            failures: 1,
            requests: 40,
            results: 39,
            ..HostStats::default()
        },
        consecutive_failures: 0,
        last_latency: Some(Duration::from_millis(120)),
        last_round_trip: Some(Duration::from_millis(131)),
    }
}
