//! Shots: on-air protection, the shot list and framing, once per tick.
//!
//! * **Protection.** The subjects of the program and preview shots are
//!   pushed to their stores before anything else can change them, so an
//!   on-air subject is never retired, evicted or demoted.
//! * **Catalog sync.** The shot list is re-derived from the stores and the
//!   queries ([`crate::catalog`]). A subject's shots follow the presets of
//!   its primary query, except that a protected subject also keeps every
//!   shot it already has, whatever happened to its query: removed (the
//!   subject is an orphan) or replaced by an alias (the store promoted it).
//!   The operator must never see the shot on air vanish and the program go
//!   black; the extra shots go once the subject leaves program and preview.
//! * **Framing.** Every shot's framer steps by `dt` and is rescored. A
//!   subject's box is framed as what its primary query asks for
//!   ([`ac_core::Query::framing_extent`]): cut at body lines only when the
//!   query's basis is a person and the detector marked the box a whole
//!   person; an orphan (its query removed) keeps the detector's extent.

use std::collections::{HashMap, HashSet};

use ac_core::{Extent, Preset, Query, SubjectId, TrackState};

use super::EngineCore;
use crate::catalog::{SourceFacts, SourceShots, SubjectFacts, SubjectShots, desired_shots};

/// What a subject's box is framed as: what its primary `query` asks for,
/// or the detector's extent for an orphan; see the module docs.
fn framing_extent(query: Option<&Query>, detected: Extent) -> Extent {
    query.map_or(detected, |q| q.framing_extent(detected))
}

impl EngineCore {
    /// Pushes the program and preview subjects to their stores.
    pub(super) fn update_protection(&mut self) {
        let mut sets = vec![HashSet::new(); self.sources.len()];
        for id in [self.switcher.program(), self.switcher.preview()]
            .into_iter()
            .flatten()
        {
            if let Some(e) = self.catalog.get(id)
                && let Some(subject) = e.subject
            {
                sets[e.source].insert(subject);
            }
        }
        for (s, set) in self.sources.iter_mut().zip(&sets) {
            s.vision.set_protected(set);
        }
    }

    /// Re-derives the shot list from the stores and queries.
    ///
    /// A subject's shots follow the presets of its primary query, except
    /// while it is protected (on program or preview): then it also keeps
    /// every shot it has now. Its query may just have been removed (it is
    /// an orphan) or replaced by an alias (the store promoted it), and in
    /// both cases the shot on air must not disappear under the operator.
    /// The extra shots go once the subject leaves the air.
    pub(super) fn sync_catalog(&mut self) {
        let presets = self.subject_presets();
        let shots: Vec<SourceShots<'_>> = self
            .sources
            .iter()
            .enumerate()
            .map(|(i, s)| SourceShots {
                id: &s.id,
                name: &s.name,
                subjects: s
                    .vision
                    .store()
                    .records()
                    .filter(|r| !r.hidden || r.protected)
                    .filter_map(|r| {
                        let id = r.subject.id;
                        let query = self.queries.iter().find(|q| q.id == r.subject.query);
                        let presets = match presets.get(&(i, id)) {
                            Some(merged) => merged.as_slice(),
                            None => query?.presets.as_slice(),
                        };
                        Some(SubjectShots {
                            id,
                            bbox: r.subject.bbox,
                            extent: framing_extent(query, r.subject.extent),
                            query_name: query.map_or(r.subject.query.0.as_str(), |q| &q.name),
                            presets,
                        })
                    })
                    .collect(),
            })
            .collect();
        let specs = desired_shots(&shots);
        let aspects: Vec<f64> = self.sources.iter().map(|s| s.aspect).collect();
        let output_aspect = self.compositor.config().aspect();
        self.catalog
            .sync(specs, &aspects, output_aspect, &self.framing);
        self.thumbnails
            .retain(|id, _| self.catalog.get(id).is_some());
    }

    /// The preset lists of protected subjects that have shots: the primary
    /// query's presets first (so a preset id both define frames as the query
    /// says now), then the presets of the subject's current shots that the
    /// query does not have. See [`Self::sync_catalog`].
    fn subject_presets(&self) -> HashMap<(usize, SubjectId), Vec<Preset>> {
        let mut held: HashMap<(usize, SubjectId), Vec<&Preset>> = HashMap::new();
        for e in self.catalog.entries() {
            if let (Some(subject), Some(p)) = (e.subject, &e.preset) {
                held.entry((e.source, subject)).or_default().push(p);
            }
        }
        let mut out = HashMap::new();
        for (i, s) in self.sources.iter().enumerate() {
            for r in s.vision.store().records().filter(|r| r.protected) {
                let key = (i, r.subject.id);
                let Some(current) = held.get(&key) else {
                    continue;
                };
                let mut presets: Vec<Preset> = self
                    .queries
                    .iter()
                    .find(|q| q.id == r.subject.query)
                    .map(|q| q.presets.clone())
                    .unwrap_or_default();
                for p in current {
                    if !presets.iter().any(|x| x.id == p.id) {
                        presets.push((*p).clone());
                    }
                }
                out.insert(key, presets);
            }
        }
        out
    }

    /// Steps every shot's framer by `dt` seconds.
    pub(super) fn step_framing(&mut self, dt: f64) {
        let now = self.now;
        let facts: Vec<SourceFacts> = self
            .sources
            .iter()
            .map(|s| SourceFacts {
                aspect: s.aspect,
                available: s.available(now),
            })
            .collect();
        let sources = &self.sources;
        let queries = &self.queries;
        let output_aspect = self.compositor.config().aspect();
        self.catalog.step(
            dt,
            &facts,
            |i, id| {
                let s = sources.get(i)?;
                let subject = s.vision.store().get(id)?;
                let query = queries.iter().find(|q| q.id == subject.query);
                Some(SubjectFacts {
                    bbox: subject.bbox,
                    extent: framing_extent(query, subject.extent),
                    tracking: subject.state == TrackState::Tracking,
                    confidence: s.vision.confidence(id),
                })
            },
            output_aspect,
        );
    }
}
