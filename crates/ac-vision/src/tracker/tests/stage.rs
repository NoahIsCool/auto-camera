//! A synthetic stage for the contrast tests: a lit wall, a faint
//! person-shaped shadow on it, and textured performers of the same shape.
//!
//! Frames are 1280x720, the width the example configs decode to; the tracker
//! reduces them to its 384 px working width as it does on the Mac. Luma is
//! `light x albedo` with the performers' albedo attached to their bodies, so
//! a performer's texture moves with them and a light cue scales every grey
//! level of the scene by the same factor. A shadow multiplies the wall by
//! `1 - dark x mask`, so its depth is a fixed fraction of the wall level: 12 %
//! of a wall at 0.7 is 21 grey levels, and the shadow's box then has a
//! standard deviation of about 0.03 (7 grey levels), against 0.1 to 0.2 for a
//! performer's box.
//!
//! Every value is a pure function of the pixel centre and the frame number
//! (the sensor noise is a hash of both), so frames are deterministic and a
//! test that fails without the contrast check fails the same way every run.

use std::f64::consts::TAU;
use std::sync::Arc;

use rayon::prelude::*;

use super::*;

/// Frame width in pixels.
pub const SW: u32 = 1280;
/// Frame height in pixels.
pub const SH: u32 = 720;

/// Renders frame `seq` of `SW x SH` grey pixels: `luma` at each pixel centre
/// plus sensor noise of up to `noise` either way, clipped to `0..=1`.
pub fn frame(seq: u64, noise: f64, luma: impl Fn(f64, f64) -> f64 + Sync) -> VideoFrame {
    let mut data = vec![0u8; (SW * SH * 4) as usize];
    data.par_chunks_mut((SW * 4) as usize)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..SW as usize {
                let grain = lattice(x as i64, y as i64 * 7919 + 13, seq.wrapping_add(1000)) - 0.5;
                let v = luma(x as f64 + 0.5, y as f64 + 0.5) + 2.0 * noise * grain;
                let g = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                row[x * 4..x * 4 + 4].copy_from_slice(&[g, g, g, 255]);
            }
        });
    VideoFrame::new(
        SourceId("cam".into()),
        seq,
        FRAME_DT * seq as u32,
        SW,
        SH,
        Arc::from(data),
    )
    .unwrap()
}

/// A standing person seen from the front: a head ellipse over a neck and a
/// rectangular torso, in frame pixels.
#[derive(Debug, Clone, Copy)]
pub struct Figure {
    /// Horizontal centre.
    pub cx: f64,
    /// Top of the head.
    pub top: f64,
    /// Height from the top of the head to the bottom of the torso.
    pub height: f64,
    /// Width of the soft edge (a cast shadow is blurred, a body is not).
    pub soft: f64,
}

impl Figure {
    /// Head half-width.
    const HEAD_RX: f64 = 24.0;
    /// Head half-height.
    const HEAD_RY: f64 = 30.0;
    /// Torso half-width.
    const HALF: f64 = 60.0;

    /// Centre of the head.
    fn head(&self) -> (f64, f64) {
        (self.cx, self.top + Self::HEAD_RY)
    }

    /// Coverage of the pixel centred at `(x, y)`, in `0..=1`.
    pub fn mask(&self, x: f64, y: f64) -> f64 {
        let (hx, hy) = self.head();
        let r = (((x - hx) / Self::HEAD_RX).powi(2) + ((y - hy) / Self::HEAD_RY).powi(2)).sqrt();
        let head = (0.5 - (r - 1.0) * Self::HEAD_RX / self.soft).clamp(0.0, 1.0);
        let torso_top = self.top + 2.0 * Self::HEAD_RY + 4.0;
        let bottom = self.top + self.height;
        let edge = ((x - self.cx).abs() - Self::HALF)
            .max(torso_top - y)
            .max(y - bottom);
        let torso = (0.5 - edge / self.soft).clamp(0.0, 1.0);
        let neck_edge = ((x - self.cx).abs() - 10.0)
            .max(hy - y)
            .max(y - torso_top - 2.0);
        let neck = (0.5 - neck_edge / self.soft).clamp(0.0, 1.0);
        head.max(torso).max(neck)
    }

    /// The box a detector would report, normalized and clipped to the frame.
    pub fn rect(&self) -> Rect {
        let x0 = self.cx - Self::HALF - self.soft / 2.0;
        let x1 = self.cx + Self::HALF + self.soft / 2.0;
        let y0 = (self.top - self.soft / 2.0).max(0.0);
        let y1 = (self.top + self.height + self.soft / 2.0).min(SH as f64);
        Rect::new(
            x0 / SW as f64,
            y0 / SH as f64,
            (x1 - x0) / SW as f64,
            (y1 - y0) / SH as f64,
        )
    }

    /// A guitarist's albedo at `(x, y)`: dark hair over a lit face, a
    /// mottled shirt and a bright guitar across the body. `seed` varies the
    /// texture between performers.
    pub fn albedo(&self, x: f64, y: f64, seed: u64) -> f64 {
        let (hx, hy) = self.head();
        let (rx, ry) = (x - self.cx, y - self.top);
        let in_head =
            ((x - hx) / Self::HEAD_RX).powi(2) + ((y - hy) / Self::HEAD_RY).powi(2) <= 1.05;
        if in_head {
            let rel = (y - (hy - Self::HEAD_RY)) / (2.0 * Self::HEAD_RY);
            if rel < 0.4 || (x - hx).abs() > 0.75 * Self::HEAD_RX {
                0.08 + 0.06 * value_noise(rx / 3.0, ry / 3.0, 11 + seed)
            } else {
                0.55 + 0.15 * value_noise(rx / 5.0, ry / 5.0, 12 + seed)
            }
        } else if (ry - 260.0 - 0.5 * rx).abs() < 40.0 && rx.abs() < 70.0 {
            0.45 + 0.2 * value_noise(rx / 8.0, ry / 8.0, 13 + seed)
        } else {
            0.15 + 0.25 * value_noise(rx / 10.0, ry / 10.0, 14 + seed)
        }
    }

    /// The figure moved sideways by `dx` pixels.
    pub fn shifted(self, dx: f64) -> Self {
        Self {
            cx: self.cx + dx,
            ..self
        }
    }
}

/// `amplitude x sin(2 pi seq / period)`: a performer's sway, in pixels.
pub fn sway(seq: u64, amplitude: f64, period: f64) -> f64 {
    amplitude * (TAU * seq as f64 / period).sin()
}

/// A wall at `wall` with a shadow `shadow` darkening it by `dark`, and an
/// opaque textured `performer` in front, at pixel `(x, y)`.
pub fn shadow_scene(
    (x, y): (f64, f64),
    wall: f64,
    (shadow, dark): (Figure, f64),
    performer: Option<Figure>,
) -> f64 {
    let lit = wall * (1.0 - dark * shadow.mask(x, y));
    match performer {
        Some(p) => {
            let m = p.mask(x, y);
            if m > 0.0 {
                lit * (1.0 - m) + p.albedo(x, y, 0) * m
            } else {
                lit
            }
        }
        None => lit,
    }
}

/// Albedo of a mottled backdrop: `0.35` give or take `0.075`, varying over
/// about 40 px.
pub fn backdrop(x: f64, y: f64) -> f64 {
    0.35 + 0.15 * (value_noise(x / 40.0, y / 40.0, 21) - 0.5)
}

/// A performer `figure` in front of the backdrop, under `light`, at `(x, y)`.
pub fn lit_performer((x, y): (f64, f64), figure: Figure, light: f64) -> f64 {
    let m = figure.mask(x, y);
    light * (backdrop(x, y) * (1.0 - m) + figure.albedo(x, y, 0) * m)
}
