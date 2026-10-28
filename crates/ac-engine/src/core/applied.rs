//! The discovery results a tick applied, handed back as [`PassRecord`]s for
//! the detection log.
//!
//! A replay of a run (`ac_vision::replay`) must see what the run's subjects
//! saw, and the subjects only ever see an *applied* pass: one that freed
//! its request and was not stale or failed ([`crate::core::passes`]).
//! Stale and failed passes changed no subject, so the log leaves them out,
//! and a replay that misses on their frames changes nothing either. Each
//! record is the pass as the detector answered it (its frame's source,
//! sequence number and PTS, the queries it looked for and every detection),
//! before the store filters out queries removed in the meantime: the log
//! stands in for the detector, and the core of the replay filters the same
//! way.
//!
//! Applying a result is what counts it in its source's `applied` counter,
//! so a result was applied exactly when applying it raised that counter;
//! the record is made before, since applying takes the result.
//!
//! The records come back in the order the results were applied, which is
//! the order they arrived in, so the log of a run lists its passes in the
//! order they changed the subjects.

use ac_vision::PassRecord;

use super::EngineCore;
use crate::discovery::DiscoveryResult;

impl EngineCore {
    /// Applies `results` in order ([`EngineCore::apply_result`]) and
    /// returns a record of each one applied; see the module docs.
    pub(super) fn apply_results(&mut self, results: Vec<DiscoveryResult>) -> Vec<PassRecord> {
        let mut applied = Vec::new();
        for r in results {
            let index = r.source;
            let record = self.pass_record(&r);
            let counted = self.applied_count(index);
            self.apply_result(r);
            if self.applied_count(index) > counted {
                applied.extend(record);
            }
        }
        applied
    }

    /// The record of `r` if it were applied: `None` for a failed pass or an
    /// unknown source, which are never applied.
    fn pass_record(&self, r: &DiscoveryResult) -> Option<PassRecord> {
        let s = self.sources.get(r.source)?;
        let detections = r.outcome.as_ref().ok()?;
        Some(PassRecord::new(
            s.id.clone(),
            r.frame.seq,
            r.frame.pts,
            r.queries.clone(),
            detections.clone(),
        ))
    }

    /// Passes applied so far on source `index`; 0 for an unknown source.
    fn applied_count(&self, index: usize) -> u64 {
        self.sources
            .get(index)
            .map_or(0, |s| s.discovery.stats.applied)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ac_core::{Extent, QueryId, Rect, SourceId};
    use ac_media::{SourceInfo, VideoFrame};
    use ac_sources::{SourceConfig, SyntheticPattern};
    use ac_vision::Detection;

    use super::*;
    use crate::config::{DetectorSettings, MonitorSettings, QuerySettings, StudioConfig};
    use crate::core::SourceFeed;

    /// A core for one 320x180 synthetic source "cam" with discovery and one
    /// query, and the source's pattern.
    fn studio() -> (EngineCore, SyntheticPattern) {
        let source = SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 1,
        };
        let mut c = StudioConfig::with_source("cam", source);
        c.detector = DetectorSettings::Synthetic { score: 0.9 };
        c.queries.push(QuerySettings {
            id: None,
            name: "Person".into(),
            prompt: "person".into(),
            presets: None,
            literal: false,
        });
        c.output.width = 320;
        c.output.height = 180;
        c.monitors = MonitorSettings::disabled();
        let info = SourceInfo {
            id: SourceId("cam".into()),
            width: 320,
            height: 180,
            fps: 30.0,
            audio: None,
        };
        let core = EngineCore::new(&c, vec![info], &[true]).unwrap();
        (core, SyntheticPattern::new(320, 180, 30.0, 1).unwrap())
    }

    /// The answer `outcome` to a pass on `frame` of source `index` that
    /// looked for `queries`.
    fn result(
        index: usize,
        frame: &VideoFrame,
        queries: &[QueryId],
        outcome: Result<Vec<Detection>, String>,
    ) -> DiscoveryResult {
        DiscoveryResult {
            source: index,
            frame: frame.clone(),
            queries: queries.to_vec(),
            outcome,
            latency: Duration::from_millis(5),
        }
    }

    /// A detection of `query` at `x`.
    fn det(query: &QueryId, x: f64) -> Detection {
        Detection {
            query: query.clone(),
            bbox: Rect::new(x, 0.2, 0.2, 0.5),
            score: 0.8,
            extent: Extent::Object,
        }
    }

    /// Runs tick `n` (from 0) on frame `n` with `results`.
    fn tick(
        core: &mut EngineCore,
        p: &SyntheticPattern,
        n: u64,
        results: Vec<DiscoveryResult>,
    ) -> crate::core::TickOutput {
        let frame = p.frame(&SourceId("cam".into()), n);
        let now = p.frame_rate().pts(n);
        core.tick(now, &[SourceFeed::running(Some(frame))], results)
    }

    #[test]
    fn an_applied_pass_comes_back_as_the_detector_answered_it() {
        let (mut core, p) = studio();
        let out = tick(&mut core, &p, 0, Vec::new());
        assert!(out.applied.is_empty());
        let (index, request) = out.requests.into_iter().next().expect("a request");
        let queries: Vec<QueryId> = request.queries.iter().map(|q| q.id.clone()).collect();
        // A detection of a query removed since is still part of the pass.
        let gone = QueryId("gone".into());
        let found = vec![det(&queries[0], 0.1), det(&gone, 0.5)];
        let answer = result(index, &request.frame, &queries, Ok(found.clone()));
        let out = tick(&mut core, &p, 1, vec![answer]);
        assert_eq!(
            out.applied,
            vec![PassRecord::new(
                SourceId("cam".into()),
                0,
                Duration::ZERO,
                queries,
                found
            )]
        );
        assert_eq!(core.store(0).unwrap().len(), 1, "and it was applied");
    }

    #[test]
    fn stale_failed_and_misdirected_passes_are_not_recorded() {
        let (mut core, p) = studio();
        let q = vec![QueryId("person".into())];
        let f = |n| p.frame(&SourceId("cam".into()), n);
        tick(&mut core, &p, 0, Vec::new());
        let first = result(0, &f(1), &q, Ok(vec![det(&q[0], 0.1)]));
        assert_eq!(tick(&mut core, &p, 2, vec![first]).applied.len(), 1);
        let cases = [
            // A frame older than the one applied: superseded.
            result(0, &f(0), &q, Ok(vec![det(&q[0], 0.2)])),
            // The detector failed.
            result(0, &f(3), &q, Err("boom".into())),
            // No such source.
            result(7, &f(3), &q, Ok(vec![det(&q[0], 0.3)])),
            // A frame of another source.
            result(0, &p.frame(&SourceId("other".into()), 3), &q, Ok(vec![])),
        ];
        for (n, case) in (3..).zip(cases) {
            let out = tick(&mut core, &p, n, vec![case]);
            assert!(out.applied.is_empty(), "tick {n}: {:?}", out.applied);
        }
    }

    #[test]
    fn passes_applied_on_one_tick_come_back_in_order() {
        let (mut core, p) = studio();
        let q = vec![QueryId("person".into())];
        let f = |n| p.frame(&SourceId("cam".into()), n);
        tick(&mut core, &p, 0, Vec::new());
        let results = vec![
            result(0, &f(1), &q, Ok(vec![det(&q[0], 0.1)])),
            result(0, &f(1), &q, Ok(vec![det(&q[0], 0.4)])),
            result(0, &f(2), &q, Ok(Vec::new())),
        ];
        let out = tick(&mut core, &p, 3, results);
        let seqs: Vec<u64> = out.applied.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, [1, 2], "the second answer for frame 1 was stale");
        assert_eq!(out.applied[0].detections[0].bbox.x, 0.1);
        assert!(
            out.applied[1].detections.is_empty(),
            "nothing found is a pass too"
        );
    }
}
