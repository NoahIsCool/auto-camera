//! Rendering: the program every tick, monitors and multiview tiles less often.
//!
//! The program is rendered on every tick because the encoder needs exactly
//! one frame per tick. Only the shots the switcher output shows are
//! sampled (the base, and the overlay during a mix), each from its source's
//! newest frame and its framer's current crop. A source whose frames stopped
//! keeps showing its last frame (a freeze is better than black on air); a
//! shot with no frame at all is drawn black and reported.
//!
//! Monitors (program and preview at monitor size) and multiview tiles exist
//! only for the operator, so they are refreshed at their own configured
//! rates, and not at all when the rate is zero (headless runs).

use std::time::Duration;

use ac_compositor::{Layer, LayerIssue, Layers};
use ac_core::{Rect, ShotId};
use ac_media::VideoFrame;
use ac_switcher::Output;

use super::{EngineCore, SourceSlot};
use crate::catalog::Catalog;

/// The newest frame of the source of `shot`, and the shot's crop.
fn shot_picture<'a>(
    catalog: &Catalog,
    sources: &'a [SourceSlot],
    shot: &ShotId,
) -> Option<(&'a VideoFrame, Rect)> {
    let e = catalog.get(shot)?;
    let frame = sources.get(e.source)?.frame.as_ref()?;
    Some((frame, e.crop))
}

/// The layers `out` needs.
fn layers_for<'a>(out: &Output, catalog: &Catalog, sources: &'a [SourceSlot]) -> Layers<'a> {
    let mut layers = Layers::new();
    let needed = out.base.iter().chain(out.overlay.as_ref().map(|(s, _)| s));
    for id in needed {
        if let Some((frame, crop)) = shot_picture(catalog, sources, id) {
            layers.insert(id.clone(), Layer::new(frame, crop));
        }
    }
    layers
}

/// Operator-readable text for a render issue.
fn describe(issue: &LayerIssue) -> String {
    match issue {
        LayerIssue::Missing(shot) => format!("{}: no picture", shot.0),
        LayerIssue::Invalid { shot, reason } => format!("{}: {reason}", shot.0),
    }
}

/// Interval for a refresh rate in Hz, or `None` when disabled.
fn interval(rate_hz: f64) -> Option<Duration> {
    (rate_hz > 0.0).then(|| Duration::from_secs_f64(1.0 / rate_hz))
}

impl EngineCore {
    /// Renders the program frame and, when due, monitors and tiles.
    pub(super) fn render(&mut self) -> VideoFrame {
        let out = self.switcher.output();
        let layers = layers_for(&out, &self.catalog, &self.sources);
        let rendered = self.compositor.render(&out, &layers, self.now);
        self.render_issues = rendered.report.issues.iter().map(describe).collect();
        let program = rendered.frame;
        self.refresh_monitors(&program);
        self.refresh_thumbnails();
        self.program_frame = Some(program.clone());
        program
    }

    /// Re-renders the program and preview monitors when due.
    fn refresh_monitors(&mut self, program: &VideoFrame) {
        let Some(every) = interval(self.monitors.rate_hz) else {
            self.program_monitor = None;
            self.preview_monitor = None;
            return;
        };
        if self.now < self.next_monitor_at {
            return;
        }
        self.next_monitor_at = self.now + every;
        let (w, h) = (self.monitors.width, self.monitors.height);
        self.program_monitor = self.compositor.render_tile(program, Rect::FULL, w, h).ok();
        let preview = self
            .switcher
            .preview()
            .and_then(|id| shot_picture(&self.catalog, &self.sources, id));
        self.preview_monitor = match preview {
            Some((frame, crop)) => self.compositor.render_tile(frame, crop, w, h).ok(),
            None => None,
        };
    }

    /// Re-renders every shot's multiview tile when due.
    fn refresh_thumbnails(&mut self) {
        let Some(every) = interval(self.monitors.thumb_rate_hz) else {
            self.thumbnails.clear();
            return;
        };
        if self.now < self.next_thumbs_at {
            return;
        }
        self.next_thumbs_at = self.now + every;
        let (w, h) = (self.monitors.thumb_width, self.monitors.thumb_height);
        for e in self.catalog.entries() {
            let Some(frame) = self.sources.get(e.source).and_then(|s| s.frame.as_ref()) else {
                continue;
            };
            if let Ok(tile) = self.compositor.render_tile(frame, e.crop, w, h) {
                self.thumbnails.insert(e.shot.id.clone(), tile);
            }
        }
    }
}
