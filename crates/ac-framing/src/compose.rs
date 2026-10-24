//! Ideal crop composition.
//!
//! A crop is described by its centre and its width as a fraction of the
//! source width. Its height follows from the output aspect ratio:
//!
//! ```text
//! h = w * source_aspect / output_aspect
//! ```
//!
//! For a subject box `b` and a preset with fill `f` and headroom `r`:
//!
//! * frame height `h = b.h / f`, so the subject fills `f` of the frame;
//! * the frame's top edge sits `r * h` above the subject's top edge;
//! * the frame is centred horizontally on the subject.
//!
//! The width is then limited by the preset's zoom ceiling (`w >= 1 / max_zoom`)
//! and by the source (`w <= 1`, `h <= 1`), and the crop is moved, never
//! resized, to stay inside the source picture.

use ac_core::{Point, Preset, Rect, ShotSize};

/// An ideal crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Composition {
    /// Centre of the crop.
    pub center: Point,
    /// Width as a fraction of the source width.
    pub width: f64,
}

impl Composition {
    /// The crop as a rectangle, given the source and output aspect ratios.
    pub fn rect(&self, source_aspect: f64, output_aspect: f64) -> Rect {
        let h = self.width * source_aspect / output_aspect;
        Rect::from_center(self.center, self.width, h)
    }
}

/// Largest crop width that fits the source for the given aspect ratios.
pub fn max_width(source_aspect: f64, output_aspect: f64) -> f64 {
    (output_aspect / source_aspect).min(1.0)
}

/// Computes the ideal crop for `subject` under `preset`.
pub fn compose(
    subject: &Rect,
    preset: &Preset,
    source_aspect: f64,
    output_aspect: f64,
) -> Composition {
    let widest = max_width(source_aspect, output_aspect);
    if preset.size == ShotSize::Wide {
        return Composition {
            center: Point { x: 0.5, y: 0.5 },
            width: widest,
        };
    }
    let height_per_width = source_aspect / output_aspect;
    let desired_h = subject.h / preset.size.fill();
    let narrowest = (1.0 / preset.max_zoom.max(1.0)).min(widest);
    let width = (desired_h / height_per_width).clamp(narrowest, widest);
    let h = width * height_per_width;

    let top = subject.y - preset.headroom * h;
    let center = Point {
        x: subject.center().x,
        y: top + h / 2.0,
    };
    let bounded = Rect::from_center(center, width, h).clamped_within(&Rect::FULL);
    Composition {
        center: bounded.center(),
        width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: f64 = 16.0 / 9.0;
    const OUT: f64 = 16.0 / 9.0;

    fn person() -> Rect {
        Rect::new(0.45, 0.3, 0.1, 0.3)
    }

    #[test]
    fn wide_preset_frames_the_whole_source() {
        let c = compose(
            &person(),
            &Preset::new("w", "Wide", ShotSize::Wide),
            SRC,
            OUT,
        );
        assert_eq!(c.rect(SRC, OUT), Rect::FULL);
    }

    #[test]
    fn subject_fills_the_requested_fraction_and_headroom() {
        let p = Preset::new("m", "Medium", ShotSize::Medium);
        let c = compose(&person(), &p, SRC, OUT);
        let r = c.rect(SRC, OUT);
        assert!((person().h / r.h - ShotSize::Medium.fill()).abs() < 1e-9);
        assert!(((person().y - r.y) / r.h - p.headroom).abs() < 1e-9);
        assert!((r.center().x - person().center().x).abs() < 1e-9);
    }

    #[test]
    fn zoom_ceiling_limits_tiny_subjects() {
        let tiny = Rect::new(0.5, 0.5, 0.01, 0.02);
        let p = Preset::new("c", "Close", ShotSize::Close);
        let c = compose(&tiny, &p, SRC, OUT);
        assert!((c.width - 1.0 / p.max_zoom).abs() < 1e-12);
    }

    #[test]
    fn crop_stays_inside_source_near_edges() {
        let edge = Rect::new(0.95, 0.02, 0.05, 0.2);
        let c = compose(
            &edge,
            &Preset::new("m", "Medium", ShotSize::Medium),
            SRC,
            OUT,
        );
        assert!(Rect::FULL.contains_rect(&c.rect(SRC, OUT)));
    }

    #[test]
    fn portrait_output_from_landscape_source_fits() {
        let c = compose(
            &person(),
            &Preset::new("w", "Wide", ShotSize::Wide),
            SRC,
            9.0 / 16.0,
        );
        let r = c.rect(SRC, 9.0 / 16.0);
        assert!((r.h - 1.0).abs() < 1e-12);
        assert!(Rect::FULL.contains_rect(&r));
    }
}
