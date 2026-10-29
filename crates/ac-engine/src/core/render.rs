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
//! rates, and not at all when the rate is zero (headless runs). Monitors
//! come from their own buffer pool, and the tile pool is sized to the shot
//! count before each refresh, so a large multiview held by the window
//! recycles its buffers instead of allocating on the engine thread.
//!
//! Each program frame is also checked for black ([`is_black`]), and when
//! the run of consecutive black frames ending with the newest began is
//! published
//! ([`EngineSnapshot::program_black_since`](crate::EngineSnapshot::program_black_since))
//! for the program-black alarm ([`crate::alarms`]). The check is on the
//! pixels, not on what the switcher asked for, so it also sees a source
//! that sends black (a capture card without signal, a capped lens) and a
//! full dip. It samples a fixed grid rather than every pixel, so it costs
//! the same at any program size, and follows ffmpeg's `blackdetect`
//! defaults in spirit: a picture is black when nearly all of it is dark.

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

/// Columns and rows of the grid of pixels [`is_black`] samples (fewer on
/// a smaller picture: one per pixel at most).
const BLACK_GRID: (u32, u32) = (32, 18);

/// Largest BT.709 luma of a dark pixel, in 1/10000 of a code value (so
/// 16 of 255): the black of a limited-range picture, below what a dark
/// stage shows, so a dim scene is not taken for black.
const DARK_LUMA: u32 = 16 * 10_000;

/// Smallest share of the samples, in percent, that must be dark for the
/// picture to be black (`blackdetect`'s `picture_black_ratio_th`), so a
/// black picture with a small caption or logo still counts.
const BLACK_SHARE: usize = 98;

/// True when `frame` looks black: at least [`BLACK_SHARE`] percent of a
/// [`BLACK_GRID`] of samples, one at the centre of each cell, have a luma
/// of at most [`DARK_LUMA`]. An empty frame shows nothing and is black; a
/// sample past the end of malformed data counts as dark, as the compositor
/// would draw such a frame black.
pub(super) fn is_black(frame: &VideoFrame) -> bool {
    let cols = BLACK_GRID.0.min(frame.width);
    let rows = BLACK_GRID.1.min(frame.height);
    if cols == 0 || rows == 0 {
        return true;
    }
    let mut dark = 0;
    for row in 0..rows {
        let y = (2 * row + 1) * frame.height / (2 * rows);
        for col in 0..cols {
            let x = (2 * col + 1) * frame.width / (2 * cols);
            let i = (y as usize * frame.width as usize + x as usize) * 4;
            let luma = match frame.data.get(i..i + 3) {
                Some(&[r, g, b]) => 2126 * u32::from(r) + 7152 * u32::from(g) + 722 * u32::from(b),
                _ => 0,
            };
            if luma <= DARK_LUMA {
                dark += 1;
            }
        }
    }
    dark * 100 >= (cols * rows) as usize * BLACK_SHARE
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
        if !is_black(&program) {
            self.program_black_since = None;
        } else if self.program_black_since.is_none() {
            self.program_black_since = Some(self.now);
        }
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
        self.program_monitor = self
            .compositor
            .render_monitor(program, Rect::FULL, w, h)
            .ok();
        let preview = self
            .switcher
            .preview()
            .and_then(|id| shot_picture(&self.catalog, &self.sources, id));
        self.preview_monitor = match preview {
            Some((frame, crop)) => self.compositor.render_monitor(frame, crop, w, h).ok(),
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
        self.compositor.reserve_tiles(self.catalog.len());
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
