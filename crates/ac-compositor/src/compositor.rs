//! The [`Compositor`]: program renders and multiview tiles.
//!
//! The compositor is the single owner of its scratch state: two pairs of
//! filter tables (base and overlay), one scratch row set per rayon band, and
//! two buffer pools (program frames and tiles, kept apart so a multiview
//! refresh cannot evict the program buffers). It is driven from one thread,
//! the engine's render loop, and parallelises internally.
//!
//! A render resolves the switcher's base and overlay shots against the layer
//! map, computes each layer's weight from the opacity and dip (see the crate
//! docs), skips layers whose weight is zero, rebuilds the filter tables of the
//! remaining layers for this frame's crops, and runs the row kernel into a
//! recycled buffer. Program frames are numbered `0, 1, 2, ...` in render
//! order; their PTS is whatever the caller's master clock says, because the
//! compositor has no clock of its own.

use std::time::Duration;

use ac_core::{Rect, ShotId};
use ac_media::VideoFrame;
use ac_switcher::Output;

use crate::filter::AxisTaps;
use crate::kernel::{self, BandScratch, Plan};
use crate::pool::FramePool;
use crate::{
    CompositorConfig, CompositorError, Layer, LayerIssue, Layers, RenderReport, Rendered,
    check_crop, check_frame, check_size,
};

/// Program buffers kept for reuse: one being encoded, one being displayed,
/// one being rendered, and one spare.
const PROGRAM_POOL: usize = 4;

/// Tile buffers kept for reuse: enough for a multiview of every shot of a
/// few sources, refreshed every frame.
const TILE_POOL: usize = 32;

/// CPU compositor owning its scratch memory and output buffers.
#[derive(Debug)]
pub struct Compositor {
    /// Output size and program source id.
    config: CompositorConfig,
    /// Sequence number of the next program frame.
    next_seq: u64,
    /// Horizontal and vertical filter tables for the base and overlay layers.
    taps: [(AxisTaps, AxisTaps); 2],
    /// Per-band scratch rows for the row kernel.
    bands: Vec<BandScratch>,
    /// Recycled program frame buffers.
    program: FramePool,
    /// Recycled tile buffers.
    tiles: FramePool,
}

impl Compositor {
    /// A compositor rendering `config.width` x `config.height` program frames.
    pub fn new(config: CompositorConfig) -> Result<Self, CompositorError> {
        check_size(config.width, config.height)?;
        Ok(Self {
            config,
            next_seq: 0,
            taps: Default::default(),
            bands: Vec::new(),
            program: FramePool::new(PROGRAM_POOL),
            tiles: FramePool::new(TILE_POOL),
        })
    }

    /// The output configuration.
    pub fn config(&self) -> &CompositorConfig {
        &self.config
    }

    /// Number of program frames rendered so far (the next frame's `seq`).
    pub fn frames_rendered(&self) -> u64 {
        self.next_seq
    }

    /// Renders the program picture described by `output` for time `pts`.
    ///
    /// Shots are looked up in `layers`. Every requested shot that is missing
    /// or unusable is drawn as black and listed in the report; the frame is
    /// produced regardless.
    pub fn render(&mut self, output: &Output, layers: &Layers<'_>, pts: Duration) -> Rendered {
        let mut report = RenderReport::default();
        let fade = 1.0 - unit(output.dip);
        let alpha = output.overlay.as_ref().map_or(0.0, |(_, a)| unit(*a));
        let base = output
            .base
            .as_ref()
            .and_then(|shot| resolve(shot, layers, &mut report));
        let overlay = output
            .overlay
            .as_ref()
            .and_then(|(shot, _)| resolve(shot, layers, &mut report));
        let weighted = [
            base.map(|layer| (layer, ((1.0 - alpha) * fade) as f32)),
            overlay.map(|layer| (layer, (alpha * fade) as f32)),
        ];
        let (width, height) = (self.config.width, self.config.height);
        let Self {
            taps,
            bands,
            program,
            ..
        } = self;
        let mut plans: [Option<Plan<'_>>; 2] = [None, None];
        for ((slot, entry), plan) in taps.iter_mut().zip(weighted).zip(&mut plans) {
            if let Some((layer, weight)) = entry
                && weight > 0.0
            {
                build_axes(slot, layer.frame, &layer.crop, width, height);
                *plan = Some(Plan {
                    data: &layer.frame.data,
                    stride: layer.frame.width as usize * 4,
                    x: &slot.0,
                    y: &slot.1,
                    weight,
                });
            }
        }
        let data = program.fill_with(pixel_bytes(width, height), |dst| {
            let row = width as usize;
            match plans {
                [Some(a), Some(b)] => kernel::render(&[a, b], row, dst, bands),
                [Some(a), None] | [None, Some(a)] => kernel::render(&[a], row, dst, bands),
                [None, None] => kernel::render(&[], row, dst, bands),
            }
        });
        let frame = VideoFrame {
            source: self.config.program.clone(),
            seq: self.next_seq,
            pts,
            width,
            height,
            data,
        };
        self.next_seq += 1;
        Rendered { frame, report }
    }

    /// Renders `crop` of `frame` at `width` x `height`, for multiview tiles and
    /// preview monitors.
    ///
    /// The tile keeps the source frame's id, sequence number and PTS, since it
    /// shows exactly that picture.
    pub fn render_tile(
        &mut self,
        frame: &VideoFrame,
        crop: Rect,
        width: u32,
        height: u32,
    ) -> Result<VideoFrame, CompositorError> {
        check_size(width, height)?;
        check_frame(frame)?;
        check_crop(&crop)?;
        let Self {
            taps, bands, tiles, ..
        } = self;
        let slot = &mut taps[0];
        build_axes(slot, frame, &crop, width, height);
        let plan = Plan {
            data: &frame.data,
            stride: frame.width as usize * 4,
            x: &slot.0,
            y: &slot.1,
            weight: 1.0,
        };
        let data = tiles.fill_with(pixel_bytes(width, height), |dst| {
            kernel::render(&[plan], width as usize, dst, bands);
        });
        Ok(VideoFrame {
            source: frame.source.clone(),
            seq: frame.seq,
            pts: frame.pts,
            width,
            height,
            data,
        })
    }
}

/// Bytes of a packed RGBA8 picture.
fn pixel_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

/// Clamps an opacity to `0.0..=1.0`, treating NaN as `0.0`.
fn unit(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// Rebuilds `taps` to map `crop` of `frame` onto `width` x `height` pixels.
fn build_axes(
    taps: &mut (AxisTaps, AxisTaps),
    frame: &VideoFrame,
    crop: &Rect,
    width: u32,
    height: u32,
) {
    let (sw, sh) = (f64::from(frame.width), f64::from(frame.height));
    taps.0.rebuild(frame.width, crop.x * sw, crop.w * sw, width);
    taps.1
        .rebuild(frame.height, crop.y * sh, crop.h * sh, height);
}

/// Looks `shot` up in `layers`, recording an issue (once per shot) when it is
/// missing or unusable.
fn resolve<'a>(shot: &ShotId, layers: &Layers<'a>, report: &mut RenderReport) -> Option<Layer<'a>> {
    let checked = match layers.get(shot) {
        None => Err(LayerIssue::Missing(shot.clone())),
        Some(layer) => check_frame(layer.frame)
            .and_then(|()| check_crop(&layer.crop))
            .map(|()| *layer)
            .map_err(|reason| LayerIssue::Invalid {
                shot: shot.clone(),
                reason,
            }),
    };
    checked
        .map_err(|issue| {
            if !report.issues.iter().any(|i| i.shot() == shot) {
                report.issues.push(issue);
            }
        })
        .ok()
}

#[cfg(test)]
mod tests;
