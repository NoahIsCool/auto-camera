//! Gated integrated loudness (ITU-R BS.1770-4 §2.8; EBU Tech 3341 §2.3).
//!
//! The programme's loudness is the power mean of its 400 ms blocks after
//! two gates. Blocks at or below the **absolute gate**, −70 LUFS (digital
//! silence, hum, room tone), never count. Of the rest, blocks at or below
//! the **relative gate**, 10 LU under the power mean of all blocks above
//! the absolute gate, are dropped as pauses and quiet passages. The power
//! mean of the remaining blocks is the integrated loudness. Gates compare
//! loudness, but means always average power (the channel-weighted mean
//! square), never decibels.
//!
//! A three-hour show has more than 100 000 blocks, and the relative gate
//! moves with every block, so a meter either keeps every block or bins
//! them. [`GatedLoudness`] keeps a histogram with one bin per [`BIN_LU`]
//! from the absolute gate up to [`TOP_LUFS`] (louder blocks go into the top
//! bin), and in each bin the number of blocks and the exact sum of their
//! powers. The means are therefore exact. Only the relative gate's verdict
//! on the bin it falls in is approximate: that bin is kept when the mean of
//! its own blocks is above the gate, so at most blocks within 0.01 LU of
//! the gate are misjudged, far below Tech 3341's ±0.1 LU tolerance. Memory
//! is fixed (about 160 kB) for a show of any length.

use crate::{loudness, power_at};

/// The absolute gate, in LUFS: blocks at or below it never count.
pub const ABSOLUTE_GATE_LUFS: f64 = -70.0;

/// The relative gate, in LU below the mean of the blocks above the
/// absolute gate.
pub const RELATIVE_GATE_LU: f64 = -10.0;

/// Width of a histogram bin, in LU.
pub const BIN_LU: f64 = 0.01;

/// Lower edge of the top bin's range, in LUFS; louder blocks share the top
/// bin (their powers are still summed exactly).
pub const TOP_LUFS: f64 = 30.0;

/// Number of histogram bins.
const BINS: usize = ((TOP_LUFS - ABSOLUTE_GATE_LUFS) / BIN_LU) as usize;

/// The blocks of one histogram bin.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Bin {
    /// Blocks in the bin.
    count: u64,
    /// Sum of their powers.
    power: f64,
}

/// Integrated loudness from block powers; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct GatedLoudness {
    /// The histogram, [`BINS`] long.
    bins: Box<[Bin]>,
    /// Blocks above the absolute gate.
    count: u64,
    /// Sum of their powers.
    power: f64,
    /// Highest bin that holds a block.
    highest: usize,
    /// Power of a block at the absolute gate.
    absolute: f64,
}

impl Default for GatedLoudness {
    fn default() -> Self {
        Self::new()
    }
}

impl GatedLoudness {
    /// An empty measurement.
    pub fn new() -> Self {
        Self {
            bins: vec![Bin::default(); BINS].into_boxed_slice(),
            count: 0,
            power: 0.0,
            highest: 0,
            absolute: power_at(ABSOLUTE_GATE_LUFS),
        }
    }

    /// The bin of a block of loudness `lufs`, which is above the absolute
    /// gate.
    fn bin_of(lufs: f64) -> usize {
        (((lufs - ABSOLUTE_GATE_LUFS) / BIN_LU) as usize).min(BINS - 1)
    }

    /// Adds one block of channel-weighted mean square `power`. Blocks at or
    /// below the absolute gate, and powers that are not finite, are left
    /// out.
    pub fn add(&mut self, power: f64) {
        if !power.is_finite() || power <= self.absolute {
            return;
        }
        let Some(lufs) = loudness(power) else {
            return;
        };
        let i = Self::bin_of(lufs);
        let bin = &mut self.bins[i];
        bin.count += 1;
        bin.power += power;
        self.count += 1;
        self.power += power;
        self.highest = self.highest.max(i);
    }

    /// Blocks counted so far (those above the absolute gate).
    pub fn blocks(&self) -> u64 {
        self.count
    }

    /// The integrated loudness in LUFS, or `None` while no block is above
    /// the absolute gate.
    pub fn integrated(&self) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        // The relative gate as a power: 10 LU below is a tenth.
        let gate = self.power / self.count as f64 * 10f64.powf(RELATIVE_GATE_LU / 10.0);
        // Bins below the gate's own bin lie wholly below the gate.
        let first = loudness(gate).map_or(0, |l| {
            if l > ABSOLUTE_GATE_LUFS {
                Self::bin_of(l)
            } else {
                0
            }
        });
        let (count, power) = self.bins[first..=self.highest]
            .iter()
            .filter(|b| b.count > 0 && b.power / b.count as f64 > gate)
            .fold((0u64, 0.0), |(n, p), b| (n + b.count, p + b.power));
        if count == 0 {
            return None;
        }
        loudness(power / count as f64)
    }

    /// Forgets every block.
    pub fn reset(&mut self) {
        self.bins.fill(Bin::default());
        self.count = 0;
        self.power = 0.0;
        self.highest = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A measurement of `n` blocks at each `(lufs, n)`.
    fn blocks(levels: &[(f64, usize)]) -> GatedLoudness {
        let mut g = GatedLoudness::new();
        for &(lufs, n) in levels {
            for _ in 0..n {
                g.add(power_at(lufs));
            }
        }
        g
    }

    /// Asserts that `got` is `Some` and within `tol` of `want`.
    fn near(got: Option<f64>, want: f64, tol: f64) {
        let got = got.expect("a reading");
        assert!((got - want).abs() <= tol, "{got} vs {want}");
    }

    #[test]
    fn nothing_above_the_absolute_gate_is_no_reading() {
        let mut g = GatedLoudness::new();
        assert_eq!(g.integrated(), None);
        g.add(0.0);
        g.add(power_at(-90.0));
        // Exactly at the gate does not count either ("above" is strict).
        g.add(power_at(ABSOLUTE_GATE_LUFS));
        g.add(f64::NAN);
        g.add(f64::INFINITY);
        g.add(-1.0);
        assert_eq!(g.blocks(), 0);
        assert_eq!(g.integrated(), None);
    }

    #[test]
    fn one_level_reads_that_level_exactly() {
        for lufs in [-69.9, -40.0, -23.0, -0.5, 6.0] {
            near(blocks(&[(lufs, 50)]).integrated(), lufs, 1e-9);
        }
    }

    #[test]
    fn levels_within_10_lu_are_averaged_as_power() {
        // Half at −20, half at −26: the power mean, not the dB mean (−23).
        let g = blocks(&[(-20.0, 100), (-26.0, 100)]);
        let want = loudness((power_at(-20.0) + power_at(-26.0)) / 2.0).unwrap();
        near(g.integrated(), want, 1e-9);
        assert!(want > -22.1, "{want}");
    }

    #[test]
    fn the_relative_gate_drops_quiet_blocks() {
        // The mean of the blocks above −70 is about −24.2 LUFS, the gate
        // about −34.2: the −36 blocks fall below it, the −75 ones below the
        // absolute gate, and only the −23 ones count.
        let g = blocks(&[(-36.0, 100), (-23.0, 600), (-36.0, 100), (-75.0, 300)]);
        assert_eq!(g.blocks(), 800);
        near(g.integrated(), -23.0, 1e-9);
        // Close to the gate but above it: kept.
        let g = blocks(&[(-23.0, 100), (-32.0, 100)]);
        let want = loudness((power_at(-23.0) + power_at(-32.0)) / 2.0).unwrap();
        near(g.integrated(), want, 1e-9);
    }

    /// BS.1770's integrated loudness of blocks at `levels`, computed
    /// directly from every block.
    fn exact(levels: &[f64]) -> Option<f64> {
        let mean = |p: &[f64]| p.iter().sum::<f64>() / p.len() as f64;
        let above: Vec<f64> = levels
            .iter()
            .map(|&l| power_at(l))
            .filter(|&p| p > power_at(ABSOLUTE_GATE_LUFS))
            .collect();
        if above.is_empty() {
            return None;
        }
        let gate = mean(&above) / 10.0;
        let kept: Vec<f64> = above.into_iter().filter(|&p| p > gate).collect();
        loudness(mean(&kept))
    }

    #[test]
    fn the_histogram_agrees_with_keeping_every_block() {
        // Blocks spread evenly over −75..0 LUFS (a golden-ratio sequence),
        // so some always sit next to the relative gate, wherever it falls.
        for n in [10, 137, 2_000, 20_000] {
            let levels: Vec<f64> = (0..n)
                .map(|i| -75.0 + 75.0 * (f64::from(i) * 0.618_033_988_749_895).fract())
                .collect();
            let mut g = GatedLoudness::new();
            for &l in &levels {
                g.add(power_at(l));
            }
            let want = exact(&levels).unwrap();
            near(g.integrated(), want, 0.005);
        }
    }

    #[test]
    fn blocks_louder_than_the_histogram_keep_their_exact_power() {
        let g = blocks(&[(40.0, 3), (50.0, 1)]);
        let want = loudness((3.0 * power_at(40.0) + power_at(50.0)) / 4.0).unwrap();
        near(g.integrated(), want, 1e-9);
    }

    #[test]
    fn reset_forgets_every_block() {
        let mut g = blocks(&[(-23.0, 10)]);
        g.reset();
        assert_eq!(g, GatedLoudness::new());
        assert_eq!(g.integrated(), None);
    }
}
