//! The detector sidecar in the summary: what its host was doing, which
//! process and model answered, and its counters over the run, restarts
//! among them.
//!
//! The parts come from different snapshots: the host's state, process and
//! hello from the last running one (the final snapshot always shows the
//! host stopped, with no process), the counters from the final one, which
//! also count what happened after the last sample. A studio without a
//! sidecar has none in its summary.

use ac_engine::EngineSnapshot;
use ac_sidecar::HostState;
use serde::Serialize;

use super::report::round;

/// The sidecar in the summary; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SidecarSummary {
    /// `idle`, `starting`, `ready`, `unavailable` or `stopped`, in the last
    /// running snapshot.
    pub state: String,
    /// Why it was unavailable, then.
    pub reason: Option<String>,
    /// The process id, then.
    pub pid: Option<u32>,
    /// The model its hello named.
    pub model: Option<String>,
    /// The model's input side, from its hello.
    pub input_side: Option<u32>,
    /// SHA-256 of the weights, when the sidecar checked them.
    pub weights_sha256: Option<String>,
    /// Processes started.
    pub spawns: u64,
    /// Processes started after the first: each replaced one that was lost
    /// or a start that failed.
    pub restarts: u64,
    /// Processes lost to a failure, failed starts included.
    pub failures: u64,
    /// Startups and passes that missed their deadline.
    pub timeouts: u64,
    /// Protocol errors.
    pub protocol_errors: u64,
    /// Requests handed to a process.
    pub requests: u64,
    /// Results accepted.
    pub results: u64,
    /// Requests the sidecar answered with an error.
    pub rejected: u64,
    /// Replies dropped as not matching the request.
    pub stale: u64,
    /// Requests not sent because their frame was too old.
    pub expired: u64,
    /// Requests refused while waiting to restart.
    pub refused: u64,
    /// Pipe threads detached after their child died.
    pub detached_threads: u64,
    /// Pass time of the newest result, in milliseconds to one decimal.
    pub last_latency_ms: Option<f64>,
    /// Round trip of the newest result, in milliseconds to one decimal.
    pub last_round_trip_ms: Option<f64>,
}

/// The sidecar of a run whose last running snapshot is `last` and whose
/// final one is `end`; `None` without a sidecar.
pub fn sidecar_summary(
    last: Option<&EngineSnapshot>,
    end: Option<&EngineSnapshot>,
) -> Option<SidecarSummary> {
    let fin = end.or(last)?.sidecar.as_ref()?;
    let live = last.and_then(|s| s.sidecar.as_ref()).unwrap_or(fin);
    let (state, reason) = match &live.state {
        HostState::Idle => ("idle", None),
        HostState::Starting => ("starting", None),
        HostState::Ready => ("ready", None),
        HostState::Unavailable { reason, .. } => ("unavailable", Some(reason.clone())),
        HostState::Stopped => ("stopped", None),
    };
    let hello = live.hello.as_ref().or(fin.hello.as_ref());
    let ms = |d: Option<std::time::Duration>| d.map(|d| round(d.as_secs_f64() * 1e3, 1));
    let st = &fin.stats;
    Some(SidecarSummary {
        state: state.to_owned(),
        reason,
        pid: live.pid,
        model: hello.map(|h| h.model.clone()),
        input_side: hello.map(|h| h.input_side),
        weights_sha256: hello.and_then(|h| h.weights_sha256.clone()),
        spawns: st.spawns,
        restarts: st.restarts(),
        failures: st.failures,
        timeouts: st.timeouts,
        protocol_errors: st.protocol_errors,
        requests: st.requests,
        results: st.results,
        rejected: st.rejected,
        stale: st.stale,
        expired: st.expired,
        refused: st.refused,
        detached_threads: st.detached_threads,
        last_latency_ms: ms(fin.last_latency.or(live.last_latency)),
        last_round_trip_ms: ms(fin.last_round_trip.or(live.last_round_trip)),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ac_engine::{EngineCore, OutputView, StudioConfig, TimingView};
    use ac_media::SourceInfo;
    use ac_sidecar::{Capabilities, Hello, HostStats, HostStatus};
    use ac_sources::SourceConfig;

    use super::*;

    /// A one-source studio's first snapshot, with `sidecar` as its host.
    fn snapshot(sidecar: Option<HostStatus>) -> EngineSnapshot {
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
            id: ac_core::SourceId("stage".into()),
            width: 64,
            height: 36,
            fps: 30.0,
            audio: None,
        };
        let core = EngineCore::new(&config, vec![info], &[true]).unwrap();
        let mut snap = core.snapshot(OutputView::default(), TimingView::default(), true);
        snap.sidecar = sidecar;
        snap
    }

    /// A host that is ready after one restart.
    fn ready() -> HostStatus {
        HostStatus {
            state: HostState::Ready,
            pid: Some(4242),
            hello: Some(Hello {
                protocol: 1,
                model: "sam3.1-mlx".into(),
                weights_sha256: Some("ab".repeat(32)),
                input_side: 1008,
                capabilities: Capabilities {
                    masks: true,
                    parts: false,
                },
            }),
            stats: HostStats {
                spawns: 2,
                failures: 1,
                requests: 10,
                results: 9,
                ..HostStats::default()
            },
            consecutive_failures: 0,
            last_latency: Some(Duration::from_micros(120_340)),
            last_round_trip: Some(Duration::from_micros(131_060)),
        }
    }

    #[test]
    fn the_state_comes_from_the_run_and_the_counts_from_the_end() {
        let last = snapshot(Some(ready()));
        let mut stopped = ready();
        stopped.state = HostState::Stopped;
        stopped.pid = None;
        stopped.hello = None;
        stopped.stats.requests = 12;
        stopped.stats.results = 11;
        let end = snapshot(Some(stopped));
        let s = sidecar_summary(Some(&last), Some(&end)).unwrap();
        assert_eq!(s.state, "ready");
        assert_eq!(s.reason, None);
        assert_eq!(s.pid, Some(4242));
        assert_eq!(s.model.as_deref(), Some("sam3.1-mlx"));
        assert_eq!(s.input_side, Some(1008));
        assert_eq!(s.weights_sha256, Some("ab".repeat(32)));
        assert_eq!((s.spawns, s.restarts, s.failures), (2, 1, 1));
        assert_eq!((s.requests, s.results), (12, 11));
        assert_eq!(s.last_latency_ms, Some(120.3));
        assert_eq!(s.last_round_trip_ms, Some(131.1));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["restarts"], 1);
        assert_eq!(json["state"], "ready");
    }

    #[test]
    fn a_host_that_was_down_says_why() {
        let mut down = ready();
        down.state = HostState::Unavailable {
            reason: "exit status 1: no model".into(),
            retry_at: None,
        };
        down.pid = None;
        down.hello = None;
        down.last_latency = None;
        down.last_round_trip = None;
        let snap = snapshot(Some(down));
        let s = sidecar_summary(Some(&snap), None).unwrap();
        assert_eq!(s.state, "unavailable");
        assert_eq!(s.reason.as_deref(), Some("exit status 1: no model"));
        assert_eq!((s.pid, s.model, s.last_latency_ms), (None, None, None));
    }

    #[test]
    fn no_sidecar_and_no_snapshot_give_none() {
        let snap = snapshot(None);
        assert_eq!(sidecar_summary(Some(&snap), Some(&snap)), None);
        assert_eq!(sidecar_summary(None, None), None);
        // A final snapshot alone still gives the counts.
        let end = snapshot(Some(ready()));
        assert_eq!(sidecar_summary(None, Some(&end)).unwrap().restarts, 1);
    }
}
