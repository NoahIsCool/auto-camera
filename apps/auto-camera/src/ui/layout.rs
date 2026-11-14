//! Window-free layout math for the operator window.
//!
//! The multiview shows every shot as a tile: a picture with the program's
//! aspect ratio and a caption strip under it. [`grid`] picks the column count
//! that makes the tiles largest while all of them fit the area. When even the
//! best fit would make tiles narrower than [`MIN_TILE_WIDTH`], tiles keep
//! that width, as many columns as fit are used, and the area scrolls
//! vertically instead. Monitors letterbox their picture with [`fit`].
//!
//! Everything here is plain arithmetic on `emath` types, so it is tested
//! without a window.

use eframe::egui::{Pos2, Rect, Vec2, vec2};

/// Gap between tiles, in points.
pub const GAP: f32 = 8.0;
/// Height of a tile's caption strip.
pub const CAPTION: f32 = 28.0;
/// Narrowest tile before the multiview scrolls instead.
pub const MIN_TILE_WIDTH: f32 = 120.0;
/// Aspect ratio used when the given one is unusable.
const FALLBACK_ASPECT: f32 = 16.0 / 9.0;

/// A grid of equally sized tiles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// Tiles per row.
    pub columns: usize,
    /// Rows.
    pub rows: usize,
    /// Picture size of one tile, caption excluded.
    pub picture: Vec2,
}

impl Grid {
    /// Size of one tile, caption included.
    pub fn tile(&self) -> Vec2 {
        vec2(self.picture.x, self.picture.y + CAPTION)
    }

    /// Size of the whole grid.
    pub fn size(&self) -> Vec2 {
        let gaps = |n: usize| GAP * n.saturating_sub(1) as f32;
        let t = self.tile();
        vec2(
            t.x * self.columns as f32 + gaps(self.columns),
            t.y * self.rows as f32 + gaps(self.rows),
        )
    }

    /// Rectangle of tile `index` (row-major) when the grid starts at `origin`.
    pub fn tile_rect(&self, origin: Pos2, index: usize) -> Rect {
        let columns = self.columns.max(1);
        let (row, col) = (index / columns, index % columns);
        let t = self.tile();
        let min = origin + vec2(col as f32 * (t.x + GAP), row as f32 * (t.y + GAP));
        Rect::from_min_size(min, t)
    }

    /// Picture part of a tile rectangle.
    pub fn picture_rect(&self, tile: Rect) -> Rect {
        Rect::from_min_size(tile.min, self.picture)
    }
}

/// Usable aspect ratio: `aspect` if positive and finite, else 16:9.
fn usable(aspect: f32) -> f32 {
    if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        FALLBACK_ASPECT
    }
}

/// Lays out `count` tiles of picture aspect `aspect` in `area`; see the
/// module docs.
pub fn grid(count: usize, area: Vec2, aspect: f32) -> Grid {
    let aspect = usable(aspect);
    if count == 0 {
        return Grid {
            columns: 0,
            rows: 0,
            picture: Vec2::ZERO,
        };
    }
    let area = area.max(Vec2::ZERO);
    let width_for = |columns: usize| {
        let rows = count.div_ceil(columns);
        let by_width = (area.x - GAP * (columns - 1) as f32) / columns as f32;
        let by_height = ((area.y - GAP * (rows - 1) as f32) / rows as f32 - CAPTION) * aspect;
        (columns, rows, by_width.min(by_height))
    };
    let (mut columns, mut rows, mut width) = (1..=count).map(width_for).fold(
        (1, count, f32::MIN),
        |best, c| if c.2 > best.2 { c } else { best },
    );
    if width < MIN_TILE_WIDTH {
        // Scroll: as many minimum-width columns as fit the width.
        let fit = ((area.x + GAP) / (MIN_TILE_WIDTH + GAP)).floor() as usize;
        columns = fit.clamp(1, count);
        rows = count.div_ceil(columns);
        width = ((area.x - GAP * (columns - 1) as f32) / columns as f32).max(MIN_TILE_WIDTH);
    }
    Grid {
        columns,
        rows,
        picture: vec2(width, width / aspect),
    }
}

/// The largest size of aspect `aspect` that fits in `available`.
pub fn fit(aspect: f32, available: Vec2) -> Vec2 {
    let aspect = usable(aspect);
    let available = available.max(Vec2::ZERO);
    if available.x / aspect <= available.y {
        vec2(available.x, available.x / aspect)
    } else {
        vec2(available.y * aspect, available.y)
    }
}

/// `inner` centred in `outer`.
pub fn centred(outer: Rect, inner: Vec2) -> Rect {
    Rect::from_center_size(outer.center(), inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    /// 16:9.
    const WIDE: f32 = 16.0 / 9.0;

    /// Equal within float rounding.
    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    /// The whole grid fits in `area`.
    fn fits(g: &Grid, area: Vec2) -> bool {
        g.size().x <= area.x + 1e-3 && g.size().y <= area.y + 1e-3
    }

    #[test]
    fn one_tile_fills_the_limiting_side() {
        let area = vec2(800.0, 400.0);
        let g = grid(1, area, WIDE);
        assert_eq!((g.columns, g.rows), (1, 1));
        assert!((g.picture.y + CAPTION - 400.0).abs() < 1e-3);
        assert!((g.picture.x / g.picture.y - WIDE).abs() < 1e-4);
        assert!(fits(&g, area));
    }

    #[test]
    fn many_tiles_pick_the_largest_layout_that_fits() {
        let area = vec2(1200.0, 700.0);
        let g = grid(5, area, WIDE);
        assert!(fits(&g, area), "{g:?}");
        assert!(g.columns * g.rows >= 5);
        // No other column count gives bigger tiles.
        for columns in 1..=5 {
            let rows = 5usize.div_ceil(columns);
            let w = ((1200.0 - GAP * (columns - 1) as f32) / columns as f32)
                .min(((700.0 - GAP * (rows - 1) as f32) / rows as f32 - CAPTION) * WIDE);
            assert!(w <= g.picture.x + 1e-3, "{columns} columns: {w} > {g:?}");
        }
        // A wide, short area prefers one row.
        let strip = grid(4, vec2(2000.0, 200.0), WIDE);
        assert_eq!((strip.columns, strip.rows), (4, 1));
        // A tall, narrow area prefers one column.
        let column = grid(3, vec2(300.0, 2000.0), WIDE);
        assert_eq!((column.columns, column.rows), (1, 3));
    }

    #[test]
    fn crowded_grids_keep_a_minimum_size_and_scroll() {
        let area = vec2(500.0, 200.0);
        let g = grid(40, area, WIDE);
        assert!(g.picture.x >= MIN_TILE_WIDTH);
        let fit = ((500.0 + GAP) / (MIN_TILE_WIDTH + GAP)).floor() as usize;
        assert_eq!((g.columns, g.rows), (fit, 40usize.div_ceil(fit)));
        assert!(g.size().x <= area.x + 1e-3);
        assert!(g.size().y > area.y, "scrolls vertically");
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let empty = grid(0, vec2(500.0, 500.0), WIDE);
        assert_eq!(
            (empty.columns, empty.rows, empty.size()),
            (0, 0, Vec2::ZERO)
        );
        let tiny = grid(3, vec2(-5.0, 0.0), WIDE);
        assert_eq!(tiny.columns, 1);
        assert!(tiny.picture.x >= MIN_TILE_WIDTH);
        let bad = grid(2, vec2(400.0, 400.0), f32::NAN);
        assert!((bad.picture.x / bad.picture.y - WIDE).abs() < 1e-4);
        assert!(close(fit(0.0, vec2(160.0, 900.0)), vec2(160.0, 90.0)));
        assert_eq!(fit(WIDE, vec2(-1.0, 10.0)), Vec2::ZERO);
    }

    #[test]
    fn tiles_are_placed_row_major_with_gaps() {
        let g = Grid {
            columns: 3,
            rows: 2,
            picture: vec2(160.0, 90.0),
        };
        let o = pos2(10.0, 20.0);
        assert_eq!(g.tile_rect(o, 0).min, o);
        assert_eq!(g.tile_rect(o, 1).min, pos2(10.0 + 160.0 + GAP, 20.0));
        assert_eq!(
            g.tile_rect(o, 3).min,
            pos2(10.0, 20.0 + 90.0 + CAPTION + GAP)
        );
        assert_eq!(g.tile_rect(o, 4).size(), vec2(160.0, 90.0 + CAPTION));
        assert_eq!(g.picture_rect(g.tile_rect(o, 4)).size(), vec2(160.0, 90.0));
        assert_eq!(
            g.size(),
            vec2(3.0 * 160.0 + 2.0 * GAP, 2.0 * (90.0 + CAPTION) + GAP)
        );
    }

    #[test]
    fn monitors_letterbox_and_centre() {
        assert!(close(fit(WIDE, vec2(1600.0, 2000.0)), vec2(1600.0, 900.0)));
        assert!(close(fit(WIDE, vec2(1600.0, 450.0)), vec2(800.0, 450.0)));
        let outer = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0));
        let r = centred(outer, vec2(100.0, 50.0));
        assert_eq!(r.min, pos2(50.0, 25.0));
    }
}
