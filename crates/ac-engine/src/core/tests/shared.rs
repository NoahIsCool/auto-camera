//! Sources taking turns on a shared detector (the sidecar): one request in
//! flight for the whole studio, the source that waited longest goes next,
//! each with its newest frame, so answers are one pass old however many
//! sources share the process; and none of them until the process is ready.
//! The sidecar also sees actions, so its queries carry no action-role
//! notice.

use super::*;
use crate::config::{SidecarSettings, SourceEntry};

/// Measured pass time of SAM 3.1 for 9 prompts at 672 px.
const PASS: Duration = Duration::from_millis(1050);

/// Plays `n` synthetic sources into a core whose detector is `detector`,
/// answering every request [`PASS`] after it was sent.
struct Studio {
    core: EngineCore,
    pattern: SyntheticPattern,
    ids: Vec<SourceId>,
    /// Requests being "detected": when each is answered, and the request.
    passes: Vec<(Duration, usize, DiscoveryRequest)>,
    /// Most requests in flight at once.
    most_in_flight: usize,
    /// Every request sent: (time, source).
    sent: Vec<(Duration, usize)>,
    /// What each source's feed says about its detector loading.
    loading: Vec<bool>,
}

impl Studio {
    /// A studio of `n` sources on `detector`, with default discovery.
    fn new(n: usize, detector: DetectorSettings) -> Self {
        let mut c = config(1);
        let first = c.sources[0].clone();
        c.sources = (0..n)
            .map(|i| SourceEntry {
                id: format!("cam-{i}"),
                ..first.clone()
            })
            .collect();
        c.detector = detector;
        let pattern = SyntheticPattern::new(W, H, FPS, 1).unwrap();
        let ids: Vec<SourceId> = c.sources.iter().map(|s| s.source_id()).collect();
        let infos = ids
            .iter()
            .map(|id| SourceInfo {
                id: id.clone(),
                width: W,
                height: H,
                fps: FPS,
                audio: None,
            })
            .collect();
        let core = EngineCore::new(&c, infos, &vec![true; n]).unwrap();
        Self {
            core,
            pattern,
            ids,
            passes: Vec::new(),
            most_in_flight: 0,
            sent: Vec::new(),
            loading: vec![false; n],
        }
    }

    /// Runs `secs` seconds of ticks.
    fn run_secs(&mut self, secs: f64) {
        let mut detector = synthetic_detector(self.pattern.clone(), 0.9);
        for _ in 0..(secs * FPS).round() as usize {
            let tick = self.core.ticks();
            let now = Duration::from_secs_f64(tick as f64 / FPS);
            let feeds: Vec<SourceFeed> = self
                .ids
                .iter()
                .zip(&self.loading)
                .map(|(id, &loading)| SourceFeed {
                    state: SourceState::Running,
                    frame: Some(self.pattern.frame(id, tick)),
                    detector_loading: loading,
                    error: None,
                })
                .collect();
            let (done, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.passes)
                .into_iter()
                .partition(|(at, _, _)| *at <= now);
            self.passes = running;
            let results = done
                .into_iter()
                .map(|(_, source, req)| DiscoveryResult {
                    source,
                    outcome: detector
                        .detect(&req.frame, &req.queries)
                        .map_err(|e| e.to_string()),
                    queries: req.queries.iter().map(|q| q.id.clone()).collect(),
                    frame: req.frame,
                    latency: PASS,
                })
                .collect();
            let out = self.core.tick(now, &feeds, results);
            for (source, req) in out.requests {
                self.sent.push((now, source));
                self.passes.push((now + PASS, source, req));
            }
            self.most_in_flight = self.most_in_flight.max(self.passes.len());
        }
    }

    /// Discovery counters of source `i`.
    fn stats(&self, i: usize) -> crate::snapshot::DiscoveryStats {
        self.core
            .snapshot(OutputView::default(), TimingView::default(), true)
            .sources[i]
            .discovery
            .clone()
    }
}

/// A sidecar detector (never started: the core only reads its kind).
fn sidecar() -> DetectorSettings {
    DetectorSettings::Sidecar(SidecarSettings::new(["sidecar"]))
}

#[test]
fn sources_take_turns_and_every_answer_is_fresh_enough() {
    // Two sources at 1.05 s a pass and the default 2 s age limit: queued
    // behind each other, every answer would come back 2.1 s old.
    let mut studio = Studio::new(2, sidecar());
    studio.run_secs(12.0);
    assert_eq!(studio.most_in_flight, 1, "one pass at a time");
    let order: Vec<usize> = studio.sent.iter().map(|(_, s)| *s).collect();
    assert!(order.len() >= 10, "{order:?}");
    assert!(
        order.windows(2).all(|w| w[0] != w[1]),
        "they alternate: {order:?}"
    );
    for i in 0..2 {
        let d = studio.stats(i);
        assert_eq!(d.stale, 0, "source {i}: {d:?}");
        assert_eq!(d.timeouts + d.errors, 0, "source {i}: {d:?}");
        assert!(d.applied >= 5, "source {i}: {d:?}");
    }
}

#[test]
fn sources_take_turns_only_once_the_shared_detector_is_ready() {
    // The sidecar is still starting: every source's loader waits for its
    // hello. Well past discovery.timeout_s, nothing is sent or timed out.
    let mut studio = Studio::new(2, sidecar());
    studio.loading = vec![true, true];
    studio.run_secs(30.0);
    assert!(studio.sent.is_empty(), "{:?}", studio.sent);
    for i in 0..2 {
        let d = studio.stats(i);
        assert!(d.preparing, "source {i}: {d:?}");
        assert_eq!((d.requests, d.timeouts, d.ready_after), (0, 0, None));
    }
    // One loader has seen the hello a tick before the other: the shared
    // detector is still starting for both.
    studio.loading = vec![false, true];
    studio.run_secs(1.0 / FPS);
    assert!(studio.sent.is_empty(), "{:?}", studio.sent);
    assert!(studio.stats(0).preparing && studio.stats(1).preparing);
    // Ready: both on the same tick, and the turns start at once.
    studio.loading = vec![false, false];
    let ready = Duration::from_secs_f64(studio.core.ticks() as f64 / FPS);
    studio.run_secs(6.0);
    assert_eq!(studio.sent.first(), Some(&(ready, 0)), "{:?}", studio.sent);
    assert_eq!(studio.most_in_flight, 1, "one pass at a time");
    let order: Vec<usize> = studio.sent.iter().map(|(_, s)| *s).collect();
    assert!(order.len() >= 4, "{order:?}");
    assert!(order.windows(2).all(|w| w[0] != w[1]), "{order:?}");
    for i in 0..2 {
        let d = studio.stats(i);
        assert!(!d.preparing, "source {i}: {d:?}");
        assert_eq!(d.ready_after, Some(ready), "source {i}");
        assert_eq!((d.stale, d.timeouts, d.errors), (0, 0, 0), "{d:?}");
        assert!(d.applied >= 2, "source {i}: {d:?}");
    }
}

#[test]
fn three_sources_share_the_rate_the_process_can_serve() {
    let mut studio = Studio::new(3, sidecar());
    studio.run_secs(15.0);
    assert_eq!(studio.most_in_flight, 1);
    let counts: Vec<u64> = (0..3).map(|i| studio.stats(i).requests).collect();
    // 15 s of 1.05 s passes, one tick apart: about 4.6 each.
    assert!(counts.iter().all(|&n| (4..=5).contains(&n)), "{counts:?}");
    assert!((0..3).all(|i| studio.stats(i).stale == 0));
    // Each source's turns are about three passes apart, never closer than
    // its own rate allows.
    let turns: Vec<Duration> = studio
        .sent
        .iter()
        .filter(|(_, s)| *s == 0)
        .map(|(t, _)| *t)
        .collect();
    assert!(
        turns.windows(2).all(|w| w[1] - w[0] >= 3 * PASS),
        "{turns:?}"
    );
}

#[test]
fn per_source_detectors_still_run_side_by_side() {
    let mut studio = Studio::new(2, DetectorSettings::Synthetic { score: 0.9 });
    studio.run_secs(4.0);
    assert_eq!(studio.most_in_flight, 2, "each source has its own detector");
    let shared = |studio: &Studio| {
        studio
            .core
            .snapshot(OutputView::default(), TimingView::default(), true)
            .shared_detector
    };
    assert!(!shared(&studio));
    assert!(shared(&Studio::new(2, sidecar())));
    assert!(sidecar().is_shared());
    assert!(!DetectorSettings::Synthetic { score: 0.9 }.is_shared());
    assert!(!DetectorSettings::None.is_shared());
}

#[test]
fn queries_carry_the_action_role_notice_only_when_the_detector_cannot_see_actions() {
    let synthetic = DetectorSettings::Synthetic { score: 0.9 };
    for (detector, noted) in [(sidecar(), false), (synthetic, true)] {
        let mut c = config(1);
        c.detector = detector;
        // One from the config, one added by command: both read the same way.
        c.queries
            .push(QuerySettings::new("Speaker", "speaker upper body"));
        let info = SourceInfo {
            id: cam(),
            width: W,
            height: H,
            fps: FPS,
            audio: None,
        };
        let mut core = EngineCore::new(&c, vec![info], &[true]).unwrap();
        core.apply(&EngineCommand::AddQuery {
            name: "Singer".into(),
            prompt: "the singer".into(),
            presets: None,
            literal: false,
        })
        .unwrap();
        let snap = core.snapshot(OutputView::default(), TimingView::default(), true);
        let notes: Vec<(&str, bool)> = snap
            .queries
            .iter()
            .map(|q| {
                let action = q
                    .parse
                    .warnings
                    .iter()
                    .any(|w| matches!(w, ac_core::PromptWarning::ActionRole { .. }));
                (q.name.as_str(), action)
            })
            .collect();
        assert_eq!(
            notes,
            [("Person", false), ("Speaker", noted), ("Singer", noted)],
            "{:?}",
            c.detector
        );
        // The detector phrase is the same either way.
        assert_eq!(snap.queries[1].parse.model_phrase, "a person speaking");
    }
}

#[test]
fn a_late_answer_does_not_free_the_slot_of_a_newer_request() {
    let mut c = config(1);
    c.detector = sidecar();
    c.discovery.timeout_s = 6.0;
    let info = SourceInfo {
        id: cam(),
        width: W,
        height: H,
        fps: FPS,
        audio: None,
    };
    let mut core = EngineCore::new(&c, vec![info], &[true]).unwrap();
    let pattern = SyntheticPattern::new(W, H, FPS, 1).unwrap();
    let mut first = None;
    let mut second = None;
    let mut late = Vec::new();
    for tick in 0..(11.0 * FPS) as u64 {
        let now = Duration::from_secs_f64(tick as f64 / FPS);
        let feed = SourceFeed {
            state: SourceState::Running,
            frame: Some(pattern.frame(&cam(), tick)),
            detector_loading: false,
            error: None,
        };
        let out = core.tick(now, &[feed], std::mem::take(&mut late));
        for (_, req) in out.requests {
            if first.is_none() {
                first = Some(req);
            } else if second.is_none() {
                // The first request timed out; answer it now, late.
                let old: DiscoveryRequest = first.clone().unwrap();
                late.push(DiscoveryResult {
                    source: 0,
                    queries: vec![QueryId("person".into())],
                    frame: old.frame,
                    outcome: Err("hung".into()),
                    latency: Duration::ZERO,
                });
                second = Some((now, req));
            } else {
                panic!("a third request while the second is in flight");
            }
        }
    }
    let (sent, _) = second.expect("a second request after the timeout");
    assert!(sent >= Duration::from_secs(6), "{sent:?}");
    let d = &core
        .snapshot(OutputView::default(), TimingView::default(), true)
        .sources[0]
        .discovery;
    assert_eq!((d.requests, d.timeouts, d.errors), (2, 1, 1), "{d:?}");
}
