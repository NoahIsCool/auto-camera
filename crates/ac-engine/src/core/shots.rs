//! Shots: on-air protection, the shot list and framing, once per tick.
//!
//! * **Protection.** The subjects of the program and preview shots, and
//!   of a shot waiting to land on preview ([`ac_switcher::Queued`]), are
//!   pushed to their stores before anything else can change them, so an
//!   on-air subject is never retired, evicted or demoted.
//! * **Catalog sync.** The shot list is re-derived from the stores and the
//!   queries ([`crate::catalog`]). A subject's shots follow the presets of
//!   every query it answers: its primary query first, then its aliases,
//!   de-duplicated by preset id (the first query to name an id frames it).
//!   Prompts that the parser reads to one detector phrase ("speaker upper
//!   body" and "speaker close-up" both ask for "a person speaking") find the
//!   same boxes and become aliases of one subject, so each query's shot
//!   words still make their shots. A protected subject also keeps every
//!   shot it already has, whatever happened to its queries: removed (the
//!   subject is an orphan) or replaced by an alias (the store promoted it).
//!   The operator must never see the shot on air vanish and the program go
//!   black; the extra shots go once the subject leaves program and preview.
//! * **Basis.** Each shot is measured on the basis of the query that asked
//!   for it ([`ac_core::Basis::framing_extent`]): cut at body lines only
//!   when that query's basis is a person and the detector marked the box a
//!   whole person. A protected subject's existing shots keep the basis they
//!   have, so removing or promoting a query never re-frames a shot on air
//!   (an orphan's "guitar close-up" keeps the fill rule rather than zooming
//!   in on the player's head under the operator).
//! * **Framing.** Every shot's framer steps by `dt` and is rescored.

use std::collections::{HashMap, HashSet};

use ac_core::{SubjectId, TrackState};
use ac_subjects::SubjectRecord;

use super::EngineCore;
use crate::catalog::{
    ShotEntry, ShotPlan, SourceFacts, SourceShots, SubjectFacts, SubjectShots, desired_shots,
};

impl EngineCore {
    /// Pushes the program, preview and queued subjects to their stores.
    pub(super) fn update_protection(&mut self) {
        let mut sets = vec![HashSet::new(); self.sources.len()];
        let queued = self.switcher.queued().map(|q| &q.shot);
        for id in [self.switcher.program(), self.switcher.preview(), queued]
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

    /// The shots subject `r` of source `source` should have; see the module
    /// docs. `held` are the shots it has now (consulted only while it is
    /// protected).
    fn plans(&self, r: &SubjectRecord, held: Option<&Vec<&ShotEntry>>) -> Vec<ShotPlan> {
        let mut plans: Vec<ShotPlan> = Vec::new();
        for q in r
            .queries()
            .filter_map(|id| self.queries.iter().find(|q| q.id == *id))
        {
            for p in &q.presets {
                if !plans.iter().any(|x| x.preset.id == p.id) {
                    plans.push(ShotPlan {
                        preset: p.clone(),
                        basis: q.basis(),
                        query_name: q.name.clone(),
                    });
                }
            }
        }
        let Some(held) = held.filter(|_| r.protected) else {
            return plans;
        };
        let primary = self
            .queries
            .iter()
            .find(|q| q.id == r.subject.query)
            .map_or(r.subject.query.0.as_str(), |q| &q.name);
        for e in held {
            let Some(preset) = &e.preset else { continue };
            match plans.iter_mut().find(|x| x.preset.id == preset.id) {
                // On air: the shot keeps how it is measured.
                Some(plan) => plan.basis = e.basis,
                None => plans.push(ShotPlan {
                    preset: preset.clone(),
                    basis: e.basis,
                    query_name: primary.to_string(),
                }),
            }
        }
        plans
    }

    /// Re-derives the shot list from the stores and queries; see the module
    /// docs.
    pub(super) fn sync_catalog(&mut self) {
        let mut held: HashMap<(usize, SubjectId), Vec<&ShotEntry>> = HashMap::new();
        for e in self.catalog.entries() {
            if let Some(subject) = e.subject {
                held.entry((e.source, subject)).or_default().push(e);
            }
        }
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
                        let plans = self.plans(r, held.get(&(i, r.subject.id)));
                        (!plans.is_empty()).then_some(SubjectShots {
                            id: r.subject.id,
                            bbox: r.subject.bbox,
                            extent: r.subject.extent,
                            plans,
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
        let output_aspect = self.compositor.config().aspect();
        self.catalog.step(
            dt,
            &facts,
            |i, id| {
                let s = sources.get(i)?;
                let subject = s.vision.store().get(id)?;
                Some(SubjectFacts {
                    bbox: subject.bbox,
                    extent: subject.extent,
                    tracking: subject.state == TrackState::Tracking,
                    confidence: s.vision.confidence(id),
                })
            },
            output_aspect,
        );
    }
}
