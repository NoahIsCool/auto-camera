//! Normalized 2-D geometry.
//!
//! Coordinates are fractions of the source frame: `x` grows right and `y` grows
//! down, both in `0.0..=1.0`. Widths and heights are fractions of the source's
//! width and height, so a 16:9 crop of a 16:9 source has `w == h`.
//!
//! A box alone does not say what it covers. [`Extent`] does: a detector
//! that returns whole people (OWLv2 people-first) and one that returns the
//! thing a prompt names (a guitar, an upper body) produce boxes that must be
//! framed differently, so every detection and subject carries its extent
//! from the detector to the framing.

use serde::{Deserialize, Serialize};

/// What a box covers, which decides how a shot is composed on it.
///
/// The detector that made the box sets it; the subject store keeps the
/// extent of the newest detection applied to a subject, and framing reads it
/// (`ac_framing::compose_subject`). The default is [`Extent::Object`], the
/// historical meaning of every box, so data written before extents existed
/// keeps its framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Extent {
    /// The box is exactly the thing to frame: an object, or a part such as
    /// an upper body. Shots size it by how much of the frame it fills.
    #[default]
    Object,
    /// The box is a whole person, crown at the top and as much of the body
    /// below as is visible. Shots are cut at body lines measured in head
    /// heights from the crown.
    Person,
}

/// A point in normalized source coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    /// Horizontal position, `0.0` at the left edge.
    pub x: f64,
    /// Vertical position, `0.0` at the top edge.
    pub y: f64,
}

/// An axis-aligned rectangle in normalized source coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width as a fraction of the source width.
    pub w: f64,
    /// Height as a fraction of the source height.
    pub h: f64,
}

impl Rect {
    /// The full source frame.
    pub const FULL: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    /// Creates a rectangle from its left/top corner and size.
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Creates a rectangle centred on `center` with the given size.
    pub fn from_center(center: Point, w: f64, h: f64) -> Self {
        Self {
            x: center.x - w / 2.0,
            y: center.y - h / 2.0,
            w,
            h,
        }
    }

    /// The rectangle's centre.
    pub fn center(&self) -> Point {
        Point {
            x: self.x + self.w / 2.0,
            y: self.y + self.h / 2.0,
        }
    }

    /// Right edge.
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    /// Bottom edge.
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    /// Area in normalized units.
    pub fn area(&self) -> f64 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    /// Intersection with `other`, or `None` when they do not overlap.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    /// Intersection over union, in `0.0..=1.0`.
    pub fn iou(&self, other: &Rect) -> f64 {
        let inter = self.intersection(other).map_or(0.0, |r| r.area());
        let union = self.area() + other.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    /// True when `other` lies entirely inside `self` (edges inclusive).
    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// Moves the rectangle by the smallest amount that keeps it inside `bounds`.
    ///
    /// If the rectangle is larger than `bounds` on an axis it is shrunk to fit
    /// on that axis, keeping its centre where possible.
    pub fn clamped_within(&self, bounds: &Rect) -> Rect {
        let w = self.w.min(bounds.w);
        let h = self.h.min(bounds.h);
        let c = self.center();
        let x = (c.x - w / 2.0).clamp(bounds.x, bounds.right() - w);
        let y = (c.y - h / 2.0).clamp(bounds.y, bounds.bottom() - h);
        Rect::new(x, y, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_of_identical_rects_is_one() {
        let r = Rect::new(0.1, 0.1, 0.2, 0.3);
        assert!((r.iou(&r) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn iou_of_disjoint_rects_is_zero() {
        let a = Rect::new(0.0, 0.0, 0.1, 0.1);
        let b = Rect::new(0.5, 0.5, 0.1, 0.1);
        assert_eq!(a.iou(&b), 0.0);
        assert!(a.intersection(&b).is_none());
    }

    #[test]
    fn clamping_moves_rect_back_inside_bounds() {
        let r = Rect::new(0.9, -0.1, 0.3, 0.3).clamped_within(&Rect::FULL);
        assert!(Rect::FULL.contains_rect(&r));
        assert!((r.w - 0.3).abs() < 1e-12 && (r.h - 0.3).abs() < 1e-12);
        assert!((r.right() - 1.0).abs() < 1e-12 && r.y.abs() < 1e-12);
    }

    #[test]
    fn extent_defaults_to_object() {
        // Boxes from before extents existed keep the fill-rule framing.
        assert_eq!(Extent::default(), Extent::Object);
        assert_ne!(Extent::Person, Extent::Object);
    }

    #[test]
    fn clamping_shrinks_oversized_rect() {
        let r = Rect::new(-0.5, 0.2, 2.0, 0.5).clamped_within(&Rect::FULL);
        assert_eq!(r.w, 1.0);
        assert_eq!(r.x, 0.0);
    }
}
