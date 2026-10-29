//! Building the published [`EngineSnapshot`] from the core's state.
//!
//! Tally comes from the switcher, which owns the buses
//! ([`ac_switcher::Switcher::tally`]): it follows exposure, as on hardware
//! switchers, so a shot is lit red while any of it is on program (both sides
//! of a running mix, the outgoing shot during the first half of a dip), and
//! green while it is staged on preview and not on program.

use ac_director::CandidateKind;

use super::EngineCore;
use crate::snapshot::{
    DirectorView, EngineSnapshot, OutputView, QueryView, ShotView, SourceView, SubjectView,
    SwitcherView, TimingView,
};

impl EngineCore {
    /// The snapshot of the newest tick, with the runtime's output and timing.
    pub fn snapshot(
        &self,
        output: OutputView,
        timing: TimingView,
        running: bool,
    ) -> EngineSnapshot {
        let preview = self.switcher.preview();
        let shots = self
            .catalog
            .entries()
            .iter()
            .map(|e| ShotView {
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
                tally: self.switcher.tally(&e.shot.id),
                thumbnail: self.thumbnails.get(&e.shot.id).cloned(),
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
            tbar_held: self.switcher.tbar_held(),
            overlay: out.overlay,
            dip: out.dip,
            queued: self.switcher.queued().cloned(),
            dropped_takes: self.switcher.dropped_takes(),
            last_dropped: self.switcher.last_dropped().cloned(),
        };
        let config = self.director.config();
        let director = DirectorView {
            mode: config.mode,
            profile: config.profile,
            suggestion: self.decision.suggestion.clone(),
            reason: self.decision.reason.clone(),
            reason_text: self.decision.reason.to_string(),
            takes: self.director_takes,
            takeover: config.takeover,
            pause: config.pause,
            paused: self.director.paused(self.now),
            skip: config.skip,
            skipped: self.director.skipped(self.now),
            preview_held: self.preview_hold.clone(),
            held_back: self.held_back,
            last_held_back: self.last_held_back.clone(),
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
                presentation: s.presentation,
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
            sidecar: None,
        }
    }
}
