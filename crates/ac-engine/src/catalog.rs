//! The shot catalog: every virtual camera the operator can switch to.
//!
//! The catalog is derived, never edited: on every tick the engine lists the
//! shots that *should* exist and [`Catalog::sync`] reconciles the live
//! entries with that list. The list is
//!
//! * one wide shot per source, then
//! * for every subject of that source (ascending id) that is visible or on
//!   air, one shot per [`ShotPlan`] the engine core gives it: the presets of
//!   every query the subject answers (its primary query first, then its
//!   aliases, de-duplicated by preset id), and for an on-air subject also
//!   those of the shots it already has (kept until it leaves the air).
//!
//! Shot ids are derived from what they frame, `"<source>/wide"` and
//! `"<source>/<subject>/<preset>"`, so they are stable for the life of a
//! subject, and an entry that survives a sync keeps its framing state. A
//! subject id is never reused within a source, so an id never silently
//! changes meaning. A subject shot is named after the query whose preset
//! made it: "Speaker 1 Close".
//!
//! Each subject shot owns a [`ShotFramer`] and has a [`Basis`], that of the
//! query its plan came from (the engine core keeps the basis of an on-air
//! shot fixed, so removing or promoting a query never changes how a live
//! shot is measured). Per tick, [`Catalog::step`] asks the framer to aim at
//! the subject's box ([`ShotFramer::aim`]) while the subject is tracking,
//! and feeds it nothing while it is lost (the framer then holds). The box
//! is framed as a whole person, cut at body lines with the shot's
//! remembered head height, only when the shot's basis is a person and the
//! detector marked the box a whole person ([`Basis::framing_extent`]);
//! otherwise it keeps the fill rule. A new shot starts framed on its subject
//! the same way ([`compose_subject`]). Each entry reports whether its shot
//! is held back by its preset's zoom ceiling ([`ShotEntry::limited_by_zoom`]),
//! so the operator sees a close-up that cannot be one labelled as such, and
//! whether that leaves it looking like another of its subject's shots
//! ([`ShotEntry::same_picture_as`]): two sizes that both need more zoom than
//! the ceiling allows are both drawn at the ceiling, and differ only by
//! their headroom. It also scores the shot for the director:
//!
//! ```text
//! settled = clamp(1 - max(|dx| / w, |dy| / h, |ln(w* / w)|) / SETTLE_TOLERANCE, 0, 1)
//! quality = track_confidence * settled      (subject shots that are tracking)
//!         = 0                               (lost subjects, unavailable sources)
//!         = 1                               (wides of available sources)
//! ```
//!
//! where `(dx, dy)` and `w*` compare the current crop with the ideal one, in
//! units of the current crop size. A shot that is still travelling to its
//! subject is therefore a worse choice to cut to than one already framed.

use std::collections::HashMap;
use std::time::Duration;

use ac_core::{Basis, Extent, Point, Preset, Rect, Shot, ShotId, ShotTarget, SourceId, SubjectId};
use ac_framing::{Composition, FramerConfig, ShotFramer, SubjectGeometry, compose_subject};
use serde::{Deserialize, Serialize};

/// Framing error, in crop sizes, at which a shot counts as fully unsettled.
pub const SETTLE_TOLERANCE: f64 = 0.25;

/// Overlap (IoU) of two zoom-limited crops of one subject above which they
/// count as the same picture ([`ShotEntry::same_picture_as`]). Two sizes
/// drawn at the same ceiling overlap by about 0.98: they differ only by the
/// gap between their headrooms.
pub const SAME_PICTURE_IOU: f64 = 0.9;

/// Id of the wide shot of `source`.
pub fn wide_shot_id(source: &SourceId) -> ShotId {
    ShotId(format!("{}/wide", source.0))
}

/// Id of the shot framing `subject` of `source` with preset `preset`.
pub fn subject_shot_id(source: &SourceId, subject: SubjectId, preset: &str) -> ShotId {
    ShotId(format!("{}/{}/{}", source.0, subject.0, preset))
}

/// Health of a shot, for tallies and the director.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotStatus {
    /// A source wide with a live source.
    Wide,
    /// A subject shot whose subject is tracking.
    Tracking,
    /// A subject shot whose subject is lost; the framing holds.
    Lost,
    /// The source has no fresh frames.
    Unavailable,
}

/// One shot the catalog should contain.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotSpec {
    /// Identity, name and target.
    pub shot: Shot,
    /// Index of the source in the engine's source list.
    pub source: usize,
    /// The preset of a subject shot.
    pub preset: Option<Preset>,
    /// How the shot measures sizes; [`Basis::Object`] for wides.
    pub basis: Basis,
    /// The subject's box when the shot is created.
    pub initial: Rect,
    /// What the detector says `initial` covers.
    pub extent: Extent,
}

/// One shot a subject should have: a preset and the query that asks for it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotPlan {
    /// The framing.
    pub preset: Preset,
    /// How the query measures shot sizes: on [`Basis::Person`] a box the
    /// detector marked a whole person is cut at body lines, on
    /// [`Basis::Object`] every box keeps the fill rule.
    pub basis: Basis,
    /// The query's operator-facing name (the query id for the kept shot of
    /// an on-air orphan).
    pub query_name: String,
}

/// A subject as the catalog needs it.
#[derive(Debug, Clone)]
pub struct SubjectShots {
    /// The subject.
    pub id: SubjectId,
    /// Its current box.
    pub bbox: Rect,
    /// What the detector says the box covers.
    pub extent: Extent,
    /// Shots to make, in order, with unique preset ids; see the module docs.
    pub plans: Vec<ShotPlan>,
}

/// A source as the catalog needs it.
#[derive(Debug, Clone)]
pub struct SourceShots<'a> {
    /// Source id.
    pub id: &'a SourceId,
    /// Operator-facing name.
    pub name: &'a str,
    /// Subjects to make shots for, ascending id.
    pub subjects: Vec<SubjectShots>,
}

/// The shots that should exist, in display order; see the module docs.
pub fn desired_shots(sources: &[SourceShots<'_>]) -> Vec<ShotSpec> {
    let prefix = |name: &str| {
        if sources.len() > 1 {
            format!("{name} ")
        } else {
            String::new()
        }
    };
    let mut out = Vec::new();
    for (index, s) in sources.iter().enumerate() {
        out.push(ShotSpec {
            shot: Shot {
                id: wide_shot_id(s.id),
                name: format!("{}Wide", prefix(s.name)),
                target: ShotTarget::Wide {
                    source: s.id.clone(),
                },
            },
            source: index,
            preset: None,
            basis: Basis::Object,
            initial: Rect::FULL,
            extent: Extent::Object,
        });
        for subject in &s.subjects {
            for plan in &subject.plans {
                let preset = &plan.preset;
                out.push(ShotSpec {
                    shot: Shot {
                        id: subject_shot_id(s.id, subject.id, &preset.id),
                        name: format!(
                            "{}{} {} {}",
                            prefix(s.name),
                            plan.query_name,
                            subject.id.0,
                            preset.name
                        ),
                        target: ShotTarget::Subject {
                            source: s.id.clone(),
                            subject: subject.id,
                            preset: preset.id.clone(),
                        },
                    },
                    source: index,
                    preset: Some(preset.clone()),
                    basis: plan.basis,
                    initial: subject.bbox,
                    extent: subject.extent,
                });
            }
        }
    }
    out
}

/// What the catalog needs to know about a source on one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceFacts {
    /// Width divided by height of the source picture.
    pub aspect: f64,
    /// The source has a fresh frame.
    pub available: bool,
}

/// What the catalog needs to know about a subject on one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubjectFacts {
    /// Current box.
    pub bbox: Rect,
    /// What the detector says the box covers; each shot's basis decides
    /// how it is framed ([`Basis::framing_extent`]).
    pub extent: Extent,
    /// The subject is tracking (not lost).
    pub tracking: bool,
    /// Latest tracker (or detector) confidence in `0..=1`.
    pub confidence: f32,
}

/// One live shot.
#[derive(Debug, Clone)]
pub struct ShotEntry {
    /// Identity, name and target.
    pub shot: Shot,
    /// Index of the source.
    pub source: usize,
    /// The framed subject, for subject shots.
    pub subject: Option<SubjectId>,
    /// The preset, for subject shots.
    pub preset: Option<Preset>,
    /// How the shot measures sizes, from the query that asked for it;
    /// [`Basis::Object`] for wides. See the module docs.
    pub basis: Basis,
    /// Smooth framing state, for subject shots.
    framer: Option<ShotFramer>,
    /// Current crop in normalized source coordinates.
    pub crop: Rect,
    /// How close the crop is to its ideal, in `0..=1`.
    pub settled: f64,
    /// The preset's zoom ceiling keeps the shot looser than its size (the
    /// newest ideal crop's verdict, with hysteresis, held while the subject
    /// is lost). Always false for wides.
    pub limited_by_zoom: bool,
    /// For a zoom-limited shot whose crop is nearly that of another shot of
    /// the same subject (overlap above [`SAME_PICTURE_IOU`]): that shot, an
    /// earlier one in display order or one that is not limited itself, so
    /// each look-alike pair is marked once. Cutting between the two changes
    /// almost nothing; see the module docs.
    pub same_picture_as: Option<ShotId>,
    /// Director quality in `0..=1`.
    pub quality: f32,
    /// The shot can be put on air.
    pub available: bool,
    /// Health.
    pub status: ShotStatus,
    /// When the shot was last on program.
    pub last_on_program: Option<Duration>,
}

/// The live shots; see the module docs.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// Entries in display order.
    entries: Vec<ShotEntry>,
    /// Position of each entry by id.
    index: HashMap<ShotId, usize>,
}

/// The crop of a source wide: as much of the source as the output aspect
/// allows, centred.
pub fn wide_crop(source_aspect: f64, output_aspect: f64) -> Rect {
    Composition {
        center: Point { x: 0.5, y: 0.5 },
        width: ac_framing::compose::max_width(source_aspect, output_aspect),
        limited_by_zoom: false,
        zoom_fit: f64::INFINITY,
    }
    .rect(source_aspect, output_aspect)
}

/// How close `current` is to `target`; see the module docs.
pub fn settledness(
    current: Composition,
    target: Composition,
    source_aspect: f64,
    output_aspect: f64,
) -> f64 {
    let w = current.width.max(1e-9);
    let h = w * source_aspect / output_aspect;
    let dx = (target.center.x - current.center.x).abs() / w;
    let dy = (target.center.y - current.center.y).abs() / h.max(1e-9);
    let dz = (target.width.max(1e-9) / w).ln().abs();
    let err = dx.max(dy).max(dz);
    if err.is_finite() {
        (1.0 - err / SETTLE_TOLERANCE).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

impl Catalog {
    /// An empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Entries in display order.
    pub fn entries(&self) -> &[ShotEntry] {
        &self.entries
    }

    /// The entry with `id`.
    pub fn get(&self, id: &ShotId) -> Option<&ShotEntry> {
        self.index.get(id).map(|&i| &self.entries[i])
    }

    /// True when `id` exists and can go on air.
    pub fn is_available(&self, id: &ShotId) -> bool {
        self.get(id).is_some_and(|e| e.available)
    }

    /// Number of shots.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when there are no shots.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records that `id` is on program at `now`.
    pub fn mark_on_program(&mut self, id: &ShotId, now: Duration) {
        if let Some(&i) = self.index.get(id) {
            self.entries[i].last_on_program = Some(now);
        }
    }

    /// Reconciles the entries with `specs`: existing ids keep their framing
    /// state and take the spec's name, preset and basis, new ids start
    /// framed on their subject, missing ids are dropped.
    /// Returns the ids added and removed.
    pub fn sync(
        &mut self,
        specs: Vec<ShotSpec>,
        aspects: &[f64],
        output_aspect: f64,
        framing: &FramerConfig,
    ) -> (Vec<ShotId>, Vec<ShotId>) {
        let mut old: HashMap<ShotId, ShotEntry> = self
            .entries
            .drain(..)
            .map(|e| (e.shot.id.clone(), e))
            .collect();
        let mut added = Vec::new();
        for spec in specs {
            let entry = match old.remove(&spec.shot.id) {
                Some(mut e) => {
                    // The name and the preset's parameters may have changed
                    // (an alias query took over with its own preset of the
                    // same id); the framer keeps its state and eases to the
                    // new framing.
                    e.shot = spec.shot;
                    e.preset = spec.preset;
                    e.basis = spec.basis;
                    e
                }
                None => {
                    added.push(spec.shot.id.clone());
                    new_entry(spec, aspects, output_aspect, framing)
                }
            };
            self.entries.push(entry);
        }
        let mut removed: Vec<ShotId> = old.into_keys().collect();
        removed.sort();
        self.index = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.shot.id.clone(), i))
            .collect();
        (added, removed)
    }

    /// Advances every shot's framing by `dt` seconds and rescores it.
    pub fn step(
        &mut self,
        dt: f64,
        sources: &[SourceFacts],
        subject: impl Fn(usize, SubjectId) -> Option<SubjectFacts>,
        output_aspect: f64,
    ) {
        for e in &mut self.entries {
            let src = sources.get(e.source).copied().unwrap_or(SourceFacts {
                aspect: output_aspect,
                available: false,
            });
            let facts = e.subject.and_then(|id| subject(e.source, id));
            match (&mut e.framer, &e.preset, facts) {
                (Some(framer), Some(preset), Some(f)) if f.tracking => {
                    let extent = e.basis.framing_extent(f.extent);
                    let target = framer.aim(&f.bbox, extent, preset, dt);
                    let current = framer.step(Some(target), dt);
                    e.crop = current.rect(src.aspect, output_aspect);
                    e.limited_by_zoom = current.limited_by_zoom;
                    e.settled = settledness(current, target, src.aspect, output_aspect);
                    e.quality = (f.confidence.clamp(0.0, 1.0) as f64 * e.settled) as f32;
                    e.status = ShotStatus::Tracking;
                }
                (Some(framer), _, _) => {
                    let current = framer.step(None, dt);
                    e.crop = current.rect(src.aspect, output_aspect);
                    e.limited_by_zoom = current.limited_by_zoom;
                    e.settled = 0.0;
                    e.quality = 0.0;
                    e.status = ShotStatus::Lost;
                }
                (None, _, _) => {
                    e.crop = wide_crop(src.aspect, output_aspect);
                    e.limited_by_zoom = false;
                    e.settled = 1.0;
                    e.quality = 1.0;
                    e.status = ShotStatus::Wide;
                }
            }
            e.available = src.available;
            if !src.available {
                e.quality = 0.0;
                e.status = ShotStatus::Unavailable;
            }
        }
        self.mark_same_pictures();
    }

    /// Sets [`ShotEntry::same_picture_as`] on every entry; see there.
    fn mark_same_pictures(&mut self) {
        for i in 0..self.entries.len() {
            let e = &self.entries[i];
            let twin = (e.limited_by_zoom && e.subject.is_some())
                .then(|| {
                    self.entries.iter().enumerate().find(|&(j, o)| {
                        j != i
                            && (j < i || !o.limited_by_zoom)
                            && o.source == e.source
                            && o.subject == e.subject
                            && o.crop.iou(&e.crop) > SAME_PICTURE_IOU
                    })
                })
                .flatten()
                .map(|(_, o)| o.shot.id.clone());
            self.entries[i].same_picture_as = twin;
        }
    }
}

/// A fresh entry for `spec`, framed on its initial box.
fn new_entry(
    spec: ShotSpec,
    aspects: &[f64],
    output_aspect: f64,
    framing: &FramerConfig,
) -> ShotEntry {
    let aspect = aspects.get(spec.source).copied().unwrap_or(output_aspect);
    let subject = match &spec.shot.target {
        ShotTarget::Subject { subject, .. } => Some(*subject),
        ShotTarget::Wide { .. } => None,
    };
    let framer = spec.preset.as_ref().map(|p| {
        let extent = spec.basis.framing_extent(spec.extent);
        let subject = SubjectGeometry::new(spec.initial, extent);
        let initial = compose_subject(&subject, p, aspect, output_aspect);
        ShotFramer::new(*framing, initial, aspect, output_aspect)
    });
    let limited_by_zoom = framer.as_ref().is_some_and(|f| f.current().limited_by_zoom);
    let crop = match &framer {
        Some(f) => f.current().rect(aspect, output_aspect),
        None => wide_crop(aspect, output_aspect),
    };
    ShotEntry {
        shot: spec.shot,
        source: spec.source,
        subject,
        preset: spec.preset,
        basis: spec.basis,
        framer,
        crop,
        settled: 1.0,
        limited_by_zoom,
        same_picture_as: None,
        quality: 0.0,
        available: false,
        status: ShotStatus::Unavailable,
        last_on_program: None,
    }
}

#[cfg(test)]
mod tests;
