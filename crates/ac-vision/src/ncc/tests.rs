//! Tests of NCC matching on small deterministic noise images: integral
//! sums, pyramid sizes, flat patches, exact and gain-shifted matches, border
//! overlaps, distractor peaks and sub-pixel refinement.

use super::*;

/// Deterministic pseudo-random texture in `0..1`.
fn noise(w: usize, h: usize, seed: u64) -> GrayImage {
    let mut s = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let data = (0..w * h)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % 1000) as f32 / 1000.0
        })
        .collect();
    GrayImage::new(w, h, data)
}

/// The `w x h` block of `img` with its top-left corner at `(x, y)`, row-major.
fn crop(img: &GrayImage, x: usize, y: usize, w: usize, h: usize) -> Vec<f32> {
    (y..y + h)
        .flat_map(|r| img.row(r)[x..x + w].to_vec())
        .collect()
}

#[test]
fn integral_sums_rectangles() {
    let data: Vec<f32> = (1..=6).map(|v| v as f32).collect();
    let (sum, sq) = Integral::pair(3, 2, &data);
    assert_eq!(sum.sum(0, 0, 3, 2), 21.0);
    assert_eq!(sum.sum(1, 0, 3, 2), 2.0 + 3.0 + 5.0 + 6.0);
    assert_eq!(sum.sum(1, 1, 1, 2), 0.0);
    assert_eq!(sq.sum(0, 1, 1, 2), 16.0);
}

#[test]
fn pyramid_halves_until_min_side() {
    let p = Pyramid::new(noise(64, 40, 1), 8, 10);
    let sizes: Vec<(usize, usize)> = p
        .levels
        .iter()
        .map(|l| (l.img.width, l.img.height))
        .collect();
    assert_eq!(sizes, vec![(64, 40), (32, 20), (16, 10)]);
    assert_eq!(Pyramid::new(noise(4, 4, 1), 8, 10).levels.len(), 1);
    assert_eq!(Pyramid::new(noise(64, 64, 1), 1, 2).levels.len(), 2);
}

#[test]
fn flat_template_is_rejected() {
    assert!(Template::new(4, 4, &[0.5; 16]).is_none());
    assert!(Template::new(2, 1, &[0.0, 1.0]).is_some());
}

#[test]
fn exact_match_scores_one_and_is_found() {
    let img = noise(60, 40, 7);
    let level = Level::new(img.clone());
    let t = Template::new(12, 10, &crop(&img, 30, 15, 12, 10)).unwrap();
    assert!((t.ncc_at(&level, 30, 15, 1.0).unwrap() - 1.0).abs() < 1e-5);
    let map = ScoreMap::compute(&level, &t, 27, 17, 5, 1.0);
    let peak = map.best().unwrap();
    assert_eq!((peak.dx, peak.dy), (3, -2));
    assert!(peak.score > 0.999);
    // Unrelated noise correlates weakly.
    assert!(map.get(-3, 0) < 0.5);
}

#[test]
fn ncc_ignores_brightness_and_contrast() {
    let img = noise(30, 30, 3);
    let t = Template::new(8, 8, &crop(&img, 10, 10, 8, 8)).unwrap();
    let shifted = GrayImage::new(30, 30, img.data.iter().map(|v| 0.2 + 0.5 * v).collect());
    let s = t.ncc_at(&Level::new(shifted), 10, 10, 1.0).unwrap();
    assert!((s - 1.0).abs() < 1e-4);
}

#[test]
fn flat_window_scores_zero() {
    let t = Template::new(4, 4, &crop(&noise(8, 8, 2), 0, 0, 4, 4)).unwrap();
    let flat = Level::new(GrayImage::new(10, 10, vec![0.3; 100]));
    assert_eq!(t.ncc_at(&flat, 2, 2, 1.0), Some(0.0));
}

#[test]
fn partial_overlap_matches_visible_part() {
    let img = noise(40, 30, 11);
    let level = Level::new(img.clone());
    // Template of the 10x10 block at (0, 5); place it 4 px further left so
    // only 6 of its 10 columns are inside the image.
    let block = crop(&img, 0, 5, 10, 10);
    let shifted: Vec<f32> = (0..10)
        .flat_map(|r| {
            let row = &block[r * 10..r * 10 + 10];
            let mut v = vec![0.9; 4];
            v.extend_from_slice(&row[..6]);
            v
        })
        .collect();
    let t = Template::new(10, 10, &shifted).unwrap();
    let s = t.ncc_at(&level, -4, 5, 0.5).unwrap();
    assert!((s - 1.0).abs() < 1e-4, "{s}");
    assert_eq!(t.ncc_at(&level, -4, 5, 0.7), None);
    assert_eq!(t.ncc_at(&level, -10, 5, 0.0), None);
    assert_eq!(t.ncc_at(&level, 40, 5, 0.0), None);
}

#[test]
fn second_peak_finds_a_distractor_but_not_the_main_slope() {
    let mut img = noise(80, 30, 5);
    let patch = crop(&img, 10, 10, 10, 10);
    // Paste an identical copy 30 px to the right.
    for r in 0..10 {
        for c in 0..10 {
            img.data[(10 + r) * 80 + 40 + c] = patch[r * 10 + c];
        }
    }
    let level = Level::new(img);
    let t = Template::new(10, 10, &patch).unwrap();
    let map = ScoreMap::compute(&level, &t, 25, 10, 18, 1.0);
    let peak = map.best().unwrap();
    assert!(peak.score > 0.999);
    assert!(map.second_peak(peak, 5) > 0.999);
    let near = ScoreMap::compute(&level, &t, 10, 10, 6, 1.0);
    let p = near.best().unwrap();
    assert_eq!((p.dx, p.dy), (0, 0));
    assert!(near.second_peak(p, 5) < 0.6);
}

#[test]
fn subpixel_refines_towards_the_higher_neighbour() {
    let map = ScoreMap::from_scores(1, vec![0.0, 0.5, 0.0, 0.6, 1.0, 0.8, 0.0, 0.5, 0.0]);
    let peak = map.best().unwrap();
    let (sx, sy) = map.subpixel(peak);
    assert!(sx > 0.0 && sx < 0.5, "{sx}");
    assert!(sy.abs() < 1e-9);
    // A peak on the window edge has no right neighbour.
    let edge = Peak {
        dx: 1,
        dy: 0,
        score: 0.8,
    };
    assert_eq!(map.subpixel(edge).0, 0.0);
}

#[test]
fn patch_ncc_matches_placement_ncc() {
    let img = noise(30, 30, 9);
    let block = crop(&img, 5, 6, 9, 7);
    let t = Template::new(9, 7, &block).unwrap();
    assert!((t.ncc_patch(&block).0 - 1.0).abs() < 1e-5);
    let other = crop(&img, 15, 16, 9, 7);
    let level = Level::new(img);
    let placed = t.ncc_at(&level, 15, 16, 1.0).unwrap();
    assert!((t.ncc_patch(&other).0 - placed).abs() < 1e-4);
    assert_eq!(t.ncc_patch(&[0.2; 63]).0, 0.0);
}

#[test]
fn empty_map_has_no_peak() {
    let t = Template::new(4, 4, &crop(&noise(8, 8, 2), 0, 0, 4, 4)).unwrap();
    let level = Level::new(noise(10, 10, 4));
    let map = ScoreMap::compute(&level, &t, 100, 100, 2, 1.0);
    assert!(map.best().is_none());
    assert_eq!(map.get(5, 0), f32::NEG_INFINITY);
}

/// `a * img + b`, sample by sample.
fn affine(img: &GrayImage, a: f32, b: f32) -> GrayImage {
    GrayImage::new(
        img.width,
        img.height,
        img.data.iter().map(|v| a * v + b).collect(),
    )
}

#[test]
fn gain_is_the_contrast_factor_of_an_affine_window() {
    // Texture in 0.1..0.3, so that 3x it plus 0.05 stays inside 0..1.
    let img = affine(&noise(40, 30, 13), 0.2, 0.1);
    let block = crop(&img, 12, 8, 10, 10);
    let t = Template::new(10, 10, &block).unwrap();
    for a in [0.5f32, 1.0, 3.0] {
        let level = Level::new(affine(&img, a, 0.05));
        let at = t.contrast_at(&level, 12, 8, 1.0).unwrap();
        assert!((at.gain() - a as f64).abs() < 1e-4, "a = {a}: {at:?}");
        let patch = crop(&level.img, 12, 8, 10, 10);
        let (score, on_patch) = t.ncc_patch(&patch);
        assert!(
            (on_patch.gain() - a as f64).abs() < 1e-4,
            "a = {a}: {on_patch:?}"
        );
        assert!(score > 0.9999, "NCC ignores the gain: {score}");
    }
    // A flat window has no contrast; NCC scores it 0 anyway.
    let flat = Level::new(GrayImage::new(40, 30, vec![0.3; 1200]));
    assert_eq!(t.contrast_at(&flat, 12, 8, 1.0).unwrap().gain(), 0.0);
    assert_eq!(t.contrast_at(&flat, 40, 8, 0.5), None);
}

#[test]
fn gain_is_measured_on_the_visible_overlap_at_borders() {
    // The template's 4 left columns are a strong checkerboard and its 6
    // right columns are the image's first 6 columns; placed 4 px over the
    // left border, only the latter are compared.
    let img = affine(&noise(40, 30, 17), 0.2, 0.1);
    let block = crop(&img, 0, 5, 6, 10);
    let raw: Vec<f32> = (0..10)
        .flat_map(|r| {
            let mut row: Vec<f32> = (0..4).map(|c| ((r + c) % 2) as f32).collect();
            row.extend_from_slice(&block[r * 6..r * 6 + 6]);
            row
        })
        .collect();
    let t = Template::new(10, 10, &raw).unwrap();
    let level = Level::new(affine(&img, 2.0, 0.05));
    let c = t.contrast_at(&level, -4, 5, 0.5).unwrap();
    assert!((c.gain() - 2.0).abs() < 1e-4, "{c:?}");
    // Against the whole template's variance the gain would be far lower.
    let whole = (c.window / (t.energy / 100.0)).sqrt();
    assert!(whole < 0.5, "{whole}");
}
