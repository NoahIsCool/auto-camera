//! Building the published [`EngineSnapshot`] from the core's state.
//!
//! Tally follows exposure, as on hardware switchers: a shot is lit red while
//! any of it is visible on program (both sides of a running mix, the
//! outgoing shot during the first half of a dip), and green while it is
//! staged on preview and not visible on program.

use std::collections::HashSet;

use ac_core::ShotId;
use ac_director::CandidateKind;

use super::EngineCore;
use crate::snapshot::{
    DirectorView, EngineSnapshot, OutputView, QueryView, ShotView, SourceView, SubjectView,
    SwitcherView, Tally, TimingView,
};

impl EngineCore {
    /// Shots visible on program right now.
    fn on_air(&self) -> HashSet<ShotId> {
        let out = self.switcher.output();
        let mut visible: HashSet<ShotId> = out.base.into_iter().collect();
        if let Some((shot, alpha)) = out.overlay
            && alpha > 0.0
        {
            visible.insert(shot);
        }
        visible
    }

    /// The snapshot of the newest tick, with the runtime's output and timing.
    pub fn snapshot(
        &self,
        output: OutputView,
        timing: TimingView,
        running: bool,
    ) -> EngineSnapshot {
        let on_air = self.on_air();
        let preview = self.switcher.preview();
        let shots = self
            .catalog
            .entries()
            .iter()
            .map(|e| {
                let program = on_air.contains(&e.shot.id);
                ShotView {
                    id: e.shot.id.clone(),
                    name: e.shot.name.clone(),
                    source: self.sources[e.source].id.clone(),
                    kind: CandidateKind::from(&e.shot.target),
                    subject: e.subject,
                    preset: e.preset.as_ref().map(|p| p.id.clone()),
                    crop: e.crop,
                    quality: e.quality,
                    settled: e.settled,
                    limited_by_zoom: e.limited_by_zoom,
                    same_picture_as: e.same_picture_as.clone(),
                    status: e.status,
                    available: e.available,
                    tally: Tally {
                        program,
                        preview: !program && preview == Some(&e.shot.id),
                    },
                    thumbnail: self.thumbnails.get(&e.shot.id).cloned(),
                }
            })
            .collect();
        let subjects = self
            .sources
            .iter()
            .flat_map(|s| {
                s.vision.store().records().map(move |r| SubjectView {
                    source: s.id.clone(),
                    id: r.subject.id,
                    query: r.subject.query.clone(),
                    aliases: r.aliases.iter().cloned().collect(),
                    bbox: r.subject.bbox,
                    state: r.subject.state,
                    confidence: s.vision.confidence(r.subject.id),
                    pinned: r.subject.pinned,
                    hidden: r.hidden,
                    protected: r.protected,
                })
            })
            .collect();
        let queries = self
            .queries
            .iter()
            .map(|q| QueryView {
                id: q.id.clone(),
                name: q.name.clone(),
                prompt: q.prompt.clone(),
                parse: q.parse.clone(),
                presets: q.presets.clone(),
                matches: self
                    .sources
                    .iter()
                    .filter_map(|s| s.discovery.matches.get(&q.id).cloned())
                    .collect(),
            })
            .collect();
        let out = self.switcher.output();
        let switcher = SwitcherView {
            program: self.switcher.program().cloned(),
            preview: preview.cloned(),
            transition: self.switcher.transition(),
            in_transition: self.switcher.in_transition(),
            overlay: out.overlay,
            dip: out.dip,
        };
        let config = self.director.config();
        let director = DirectorView {
            mode: config.mode,
            profile: config.profile,
            suggestion: self.decision.suggestion.clone(),
            reason: self.decision.reason.clone(),
            reason_text: self.decision.reason.to_string(),
            takes: self.director_takes,
        };
        let now = self.now;
        let sources = self
            .sources
            .iter()
            .map(|s| SourceView {
                id: s.id.clone(),
                name: s.name.clone(),
                state: s.state,
                error: s.error.clone(),
                width: s.info.width,
                height: s.info.height,
                nominal_fps: s.info.fps,
                fps: s.rate.rate(now),
                frame_age: s.last_frame_at.map(|t| now.saturating_sub(t)),
                frames: s.frames,
                missed: s.missed,
                available: s.available(now),
                subjects: s.vision.store().len(),
                discovery_enabled: s.discovery.enabled,
                discovery: s.discovery.stats.clone(),
                vision: s.vision.stats(),
            })
            .collect();
        EngineSnapshot {
            tick: self.ticks,
            media_time: self.now,
            running,
            fatal: None,
            shots,
            subjects,
            queries,
            switcher,
            director,
            sources,
            output,
            timing,
            program_monitor: self.program_monitor.clone(),
            preview_monitor: self.preview_monitor.clone(),
            render_issues: self.render_issues.clone(),
            program_changes: self.program_changes,
            shared_detector: self.shared_detector,
        }
    }
}
