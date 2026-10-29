//! The discovery side of a tick: applying the detector's answers (with the
//! prompt feedback they carry) and issuing the next requests.
//!
//! # Applying
//!
//! An answer frees its source's request slot only when it answers the
//! newest request: a late answer to a request that timed out must not let
//! a second request go out while the newer one is still being worked on.
//! It is then applied unless it is stale ([`staleness`]), which includes an
//! answer about a frame of an older epoch once a frame of the source's new
//! connection has come in: the old answer frees the slot, so a request on
//! the new picture can follow, but its boxes are dropped. An applied answer
//! *replaces* the feedback of every live query it looked for on that
//! source (a later pass that found nothing reads "no matches", it does not
//! add to the earlier count), and it is counted before the subject store
//! sees the boxes, so aliasing, admission and the store's capacity cannot
//! change what the operator is told the detector found. Stale and failed
//! answers change no feedback; they only become the source's newest
//! [`PassOutcome`].
//!
//! # Scheduling
//!
//! A source is asked at most `discovery.rate_hz` times a second, never
//! while its request is in flight, and never while its detector is still
//! preparing (which also counts no timeouts). A request in flight longer
//! than `discovery.timeout_s` is given up and counted as timed out. When
//! one detector serves every source, the studio keeps one request in
//! flight in all and the due source that waited longest since its last
//! turn goes next (see the parent module's docs for why). Every request
//! asks for each query's parsed detector phrase, never its prompt.

use ac_core::QueryId;
use ac_vision::PromptQuery;

use super::EngineCore;
use crate::discovery::{DiscoveryRequest, DiscoveryResult, staleness};
use crate::snapshot::{MatchView, PassOutcome};

impl EngineCore {
    /// Applies one discovery result unless it is stale.
    pub(super) fn apply_result(&mut self, r: DiscoveryResult) {
        let live: Vec<QueryId> = self.queries.iter().map(|q| q.id.clone()).collect();
        let max_age = self.discovery.max_result_age();
        let Some(s) = self.sources.get_mut(r.source) else {
            return;
        };
        let d = &mut s.discovery;
        // A late answer to a request that timed out does not free the slot
        // of the newer request now in flight.
        if d.last_requested == Some(r.frame.seq) {
            d.in_flight_since = None;
        }
        let detections = match r.outcome {
            Ok(dets) if r.frame.source == s.id => {
                d.stats.last_latency = Some(r.latency);
                dets
            }
            Ok(_) => {
                d.stats.errors += 1;
                d.stats.last_error = Some("result for another source".into());
                d.stats.last_outcome = Some(PassOutcome::Failed);
                return;
            }
            Err(e) => {
                d.stats.errors += 1;
                d.stats.last_error = Some(e);
                d.stats.last_outcome = Some(PassOutcome::Failed);
                return;
            }
        };
        let newest = s.frame.as_ref().map(|f| (f.epoch, f.pts));
        let (epoch, seq, pts) = (r.frame.epoch, r.frame.seq, r.frame.pts);
        if staleness(epoch, seq, pts, d.last_applied, newest, max_age).is_some() {
            d.stats.stale += 1;
            d.stats.last_outcome = Some(PassOutcome::Stale);
            return;
        }
        d.last_applied = Some(r.frame.seq);
        d.stats.applied += 1;
        d.stats.last_outcome = Some(PassOutcome::Applied);
        let floor = s.vision.store().config().min_score;
        for id in r.queries.iter().filter(|id| live.contains(id)) {
            let mut feedback = MatchView {
                source: s.id.clone(),
                seq: r.frame.seq,
                pts: r.frame.pts,
                count: 0,
                best: None,
                kept: 0,
                floor,
            };
            for det in detections.iter().filter(|det| &det.query == id) {
                feedback.count += 1;
                feedback.kept += usize::from(det.score >= floor);
                feedback.best = Some(feedback.best.map_or(det.score, |b| b.max(det.score)));
            }
            d.matches.insert(id.clone(), feedback);
        }
        s.vision
            .on_detections(&r.frame, s.frame.as_ref(), &detections, &live);
    }

    /// Issues discovery requests for sources that are due and not
    /// preparing; see the module docs for how sources take turns on a
    /// shared detector.
    pub(super) fn schedule_discovery(&mut self) -> Vec<(usize, DiscoveryRequest)> {
        let now = self.now;
        let timeout = self.discovery.timeout();
        let interval = self.discovery.interval();
        // The detector gets each query's parsed phrase, never the operator's
        // prompt: "speaker upper body" asks for "a person speaking".
        let queries: Vec<PromptQuery> = self
            .queries
            .iter()
            .map(|q| PromptQuery {
                id: q.id.clone(),
                prompt: q.detector_phrase().to_string(),
            })
            .collect();
        for s in &mut self.sources {
            let d = &mut s.discovery;
            if !d.enabled || d.stats.preparing {
                continue;
            }
            if let Some(since) = d.in_flight_since
                && now.saturating_sub(since) >= timeout
            {
                d.in_flight_since = None;
                d.stats.timeouts += 1;
                d.stats.last_outcome = Some(PassOutcome::TimedOut);
            }
        }
        let due = self.sources.iter().enumerate().filter(|(_, s)| {
            let d = &s.discovery;
            d.enabled
                && !d.stats.preparing
                && d.in_flight_since.is_none()
                && now >= d.next_due
                && !queries.is_empty()
                && s.frame
                    .as_ref()
                    .is_some_and(|f| d.last_requested != Some(f.seq))
        });
        let chosen: Vec<usize> = if !self.shared_detector {
            due.map(|(i, _)| i).collect()
        } else if self
            .sources
            .iter()
            .any(|s| s.discovery.in_flight_since.is_some())
        {
            Vec::new()
        } else {
            // The source that waited longest since its last turn (never
            // first), so every source gets a turn.
            due.min_by_key(|(i, s)| (s.discovery.last_sent, *i))
                .map(|(i, _)| i)
                .into_iter()
                .collect()
        };
        let mut out = Vec::new();
        for i in chosen {
            let s = &mut self.sources[i];
            let Some(frame) = &s.frame else { continue };
            let d = &mut s.discovery;
            d.in_flight_since = Some(now);
            d.last_sent = Some(now);
            d.next_due = now + interval;
            d.last_requested = Some(frame.seq);
            d.stats.requests += 1;
            out.push((
                i,
                DiscoveryRequest {
                    frame: frame.clone(),
                    queries: queries.clone(),
                },
            ));
        }
        out
    }
}
