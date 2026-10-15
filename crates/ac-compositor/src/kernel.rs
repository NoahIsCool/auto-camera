//! Row kernel: resample, weight, sum and quantise layers into RGBA8 rows.
//!
//! Each output row is produced independently, which is what lets rayon split
//! the picture into horizontal bands with no synchronisation and makes the
//! result identical whatever the thread count.
//!
//! For one output row and one layer the kernel runs the separable filter in
//! two passes:
//!
//! 1. **Vertical**: the row's vertical taps blend whole source rows, over just
//!    the columns the horizontal taps will read, into an `f32` scratch row.
//! 2. **Horizontal**: each output pixel sums its horizontal taps over that
//!    scratch row and adds the result, times the layer's weight, into an `f32`
//!    accumulator row.
//!
//! After every layer is accumulated the row is quantised once to RGBA8 with
//! round-half-up, so mixing and dipping add no extra rounding steps. Layer
//! weights already include the overlay opacity and the dip (see the crate
//! docs), and a layer that is absent contributes nothing, which is exactly
//! black. Output alpha is always 255: the program picture is opaque, and the
//! sources' alpha channel is carried through the filter but discarded.
//!
//! Work per output row is `O(span * vertical taps + width * horizontal taps)`
//! with no allocation once the scratch vectors have grown to the largest
//! frame seen.

use rayon::prelude::*;

use crate::filter::AxisTaps;

/// One source layer ready to be drawn: pixels, filter tables and weight.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Plan<'a> {
    /// Packed RGBA8 source pixels.
    pub(crate) data: &'a [u8],
    /// Source row length in bytes (`width * 4`).
    pub(crate) stride: usize,
    /// Horizontal taps, one per output column.
    pub(crate) x: &'a AxisTaps,
    /// Vertical taps, one per output row.
    pub(crate) y: &'a AxisTaps,
    /// Contribution of this layer to the final picture.
    pub(crate) weight: f32,
}

/// Per-band scratch rows, owned by the compositor and reused every frame.
#[derive(Debug, Default, Clone)]
pub(crate) struct BandScratch {
    /// Vertically filtered source columns for the layer being drawn, as
    /// interleaved RGBA `f32` values.
    column: Vec<f32>,
    /// Weighted sum of all layers for the output row being drawn.
    row: Vec<[f32; 4]>,
}

/// Renders every row of a `width`-pixel-wide RGBA8 picture into `dst` by
/// summing `plans`, in parallel over bands of rows.
///
/// `bands` grows to the number of bands used; its rows grow to the widest
/// picture seen and are then reused. With no plans the picture is black.
pub(crate) fn render(
    plans: &[Plan<'_>],
    width: usize,
    dst: &mut [u8],
    bands: &mut Vec<BandScratch>,
) {
    let row_bytes = width * 4;
    if row_bytes == 0 || dst.is_empty() {
        return;
    }
    let height = dst.len() / row_bytes;
    let wanted = (rayon::current_num_threads() * 4).clamp(1, height);
    let rows_per_band = height.div_ceil(wanted);
    let band_count = height.div_ceil(rows_per_band);
    if bands.len() < band_count {
        bands.resize_with(band_count, BandScratch::default);
    }
    dst.par_chunks_mut(rows_per_band * row_bytes)
        .zip(bands.par_iter_mut())
        .enumerate()
        .for_each(|(band, (chunk, scratch))| {
            for (r, out) in chunk.chunks_exact_mut(row_bytes).enumerate() {
                render_row(plans, band * rows_per_band + r, width, scratch, out);
            }
        });
}

/// Renders output row `j` into `out` (`width * 4` bytes).
fn render_row(
    plans: &[Plan<'_>],
    j: usize,
    width: usize,
    scratch: &mut BandScratch,
    out: &mut [u8],
) {
    let BandScratch { column, row } = scratch;
    row.clear();
    row.resize(width, [0.0; 4]);
    for plan in plans {
        accumulate(plan, j, column, row);
    }
    quantise(row, out);
}

/// Adds `plan.weight` times the filtered row `j` of the layer to `row`.
fn accumulate(plan: &Plan<'_>, j: usize, column: &mut Vec<f32>, row: &mut [[f32; 4]]) {
    let (c0, c1) = plan.x.span();
    let len = (c1 - c0) * 4;
    let (y0, y_weights) = plan.y.taps(j);
    column.resize(len, 0.0);
    // Flat channel-interleaved loops over bytes vectorise well; the first tap
    // overwrites, so the scratch row needs no clearing.
    for (t, &w) in y_weights.iter().enumerate() {
        let from = (y0 + t) * plan.stride + c0 * 4;
        let src = &plan.data[from..from + len];
        if t == 0 {
            for (a, &p) in column.iter_mut().zip(src) {
                *a = f32::from(p) * w;
            }
        } else {
            for (a, &p) in column.iter_mut().zip(src) {
                *a += f32::from(p) * w;
            }
        }
    }
    let (pixels, _) = column.as_chunks::<4>();
    for (i, out) in row.iter_mut().enumerate() {
        let (x0, x_weights) = plan.x.taps(i);
        let mut sum = [0.0f32; 4];
        for (px, &w) in pixels[x0 - c0..].iter().zip(x_weights) {
            for (s, &a) in sum.iter_mut().zip(px) {
                *s += a * w;
            }
        }
        for (o, s) in out.iter_mut().zip(sum) {
            *o += s * plan.weight;
        }
    }
}

/// Converts an `f32` row to opaque RGBA8, rounding half up and saturating.
fn quantise(row: &[[f32; 4]], out: &mut [u8]) {
    for (px, v) in out.chunks_exact_mut(4).zip(row) {
        // `as u8` saturates, so tiny overshoots from f32 rounding clamp to 255.
        px[0] = (v[0] + 0.5) as u8;
        px[1] = (v[1] + 0.5) as u8;
        px[2] = (v[2] + 0.5) as u8;
        px[3] = u8::MAX;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taps(src_len: u32, start: f64, len: f64, out_len: u32) -> AxisTaps {
        let mut t = AxisTaps::default();
        t.rebuild(src_len, start, len, out_len);
        t
    }

    #[test]
    fn no_plans_render_opaque_black() {
        let mut dst = vec![7u8; 3 * 2 * 4];
        render(&[], 3, &mut dst, &mut Vec::new());
        assert!(dst.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
    }

    #[test]
    fn weights_scale_and_sum_layers() {
        let src = [100u8, 50, 200, 9].repeat(4);
        let (x, y) = (taps(2, 0.0, 2.0, 2), taps(2, 0.0, 2.0, 2));
        let plan = |weight| Plan {
            data: &src,
            stride: 8,
            x: &x,
            y: &y,
            weight,
        };
        let mut dst = vec![0u8; 16];
        render(&[plan(0.25), plan(0.25)], 2, &mut dst, &mut Vec::new());
        assert!(dst.chunks_exact(4).all(|p| p == [50, 25, 100, 255]));
    }

    #[test]
    fn quantise_rounds_half_up_and_saturates() {
        let mut out = [0u8; 8];
        quantise(
            &[[127.5, -3.0, 255.4, 0.0], [300.0, 0.49, 0.5, 1.0]],
            &mut out,
        );
        assert_eq!(out, [128, 0, 255, 255, 255, 0, 1, 255]);
    }

    #[test]
    fn rows_are_independent_of_band_split() {
        let src: Vec<u8> = (0..16 * 16 * 4).map(|i| (i * 7 % 251) as u8).collect();
        let (x, y) = (taps(16, 1.5, 11.0, 5), taps(16, -2.0, 15.0, 13));
        let plans = [Plan {
            data: &src,
            stride: 64,
            x: &x,
            y: &y,
            weight: 1.0,
        }];
        let mut parallel = vec![0u8; 5 * 13 * 4];
        render(&plans, 5, &mut parallel, &mut Vec::new());
        let mut serial = vec![0u8; 5 * 13 * 4];
        let mut scratch = BandScratch::default();
        for (j, out) in serial.chunks_exact_mut(20).enumerate() {
            render_row(&plans, j, 5, &mut scratch, out);
        }
        assert_eq!(parallel, serial);
    }

    #[test]
    fn empty_destination_is_a_no_op() {
        render(&[], 0, &mut [], &mut Vec::new());
        render(&[], 4, &mut [], &mut Vec::new());
    }
}
