//! Behavioural tests of [`Compositor`]: exactness of 1:1 and solid-colour
//! resampling, mixing and dipping arithmetic, robustness to bad input, tile
//! sizes, provenance tagging and buffer reuse.

use std::sync::Arc;
use std::time::Duration;

use ac_core::{Rect, ShotId, SourceId};
use ac_media::VideoFrame;
use ac_switcher::Output;

use super::{Compositor, TILE_GENERATIONS};
use crate::{CompositorConfig, CompositorError, Layer, LayerIssue, Layers};

/// A shot id.
fn id(s: &str) -> ShotId {
    ShotId(s.into())
}

/// A frame from raw RGBA8 bytes.
fn frame(width: u32, height: u32, data: Vec<u8>) -> VideoFrame {
    VideoFrame::new(
        SourceId("cam".into()),
        7,
        Duration::from_millis(40),
        width,
        height,
        Arc::from(data),
    )
    .unwrap()
}

/// A frame filled with one opaque colour.
fn solid(width: u32, height: u32, rgb: [u8; 3]) -> VideoFrame {
    let px = [rgb[0], rgb[1], rgb[2], 255];
    frame(width, height, px.repeat(width as usize * height as usize))
}

/// An opaque frame with a distinct value in every channel of every pixel.
fn pattern(width: u32, height: u32) -> VideoFrame {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            data.extend([
                (x * 7 + y) as u8,
                (y * 13 + x * 3) as u8,
                (x ^ y) as u8,
                255,
            ]);
        }
    }
    frame(width, height, data)
}

/// A compositor with the given output size.
fn compositor(width: u32, height: u32) -> Compositor {
    Compositor::new(CompositorConfig::new(width, height)).unwrap()
}

/// The switcher output showing `shot` alone.
fn program(shot: &str) -> Output {
    Output {
        base: Some(id(shot)),
        overlay: None,
        dip: 0.0,
    }
}

/// True when every pixel of `f` equals `rgb`, opaque.
fn all_pixels(f: &VideoFrame, rgb: [u8; 3]) -> bool {
    f.data
        .chunks_exact(4)
        .all(|p| p == [rgb[0], rgb[1], rgb[2], 255])
}

#[test]
fn identity_crop_reproduces_the_source_when_sizes_match() {
    let src = pattern(64, 36);
    let mut c = compositor(64, 36);
    let layers = Layers::from([(id("a"), Layer::new(&src, Rect::FULL))]);
    let out = c.render(&program("a"), &layers, Duration::ZERO);
    assert!(out.report.is_clean());
    assert_eq!(&*out.frame.data, &*src.data);
}

#[test]
fn whole_pixel_one_to_one_crop_copies_that_region() {
    let src = pattern(64, 32);
    let mut c = compositor(16, 8);
    let crop = Rect::new(0.25, 0.5, 0.25, 0.25);
    let layers = Layers::from([(id("a"), Layer::new(&src, crop))]);
    let out = c.render(&program("a"), &layers, Duration::ZERO).frame;
    for y in 0..8 {
        for x in 0..16 {
            assert_eq!(out.pixel(x, y), src.pixel(x + 16, y + 16));
        }
    }
}

#[test]
fn solid_colours_scale_exactly() {
    let rgb = [200, 17, 99];
    let cases = [
        ((3840, 2160), Rect::FULL, (1920, 1080)),
        ((64, 36), Rect::FULL, (192, 108)),
        ((100, 70), Rect::new(0.13, 0.07, 0.61, 0.83), (37, 53)),
        ((5, 3), Rect::new(0.2, 0.0, 0.5, 1.0), (128, 72)),
        ((1, 1), Rect::FULL, (16, 9)),
        ((50, 50), Rect::new(-0.3, 0.6, 0.9, 0.9), (30, 31)),
    ];
    for ((sw, sh), crop, (ow, oh)) in cases {
        let src = solid(sw, sh, rgb);
        let mut c = compositor(ow, oh);
        let layers = Layers::from([(id("a"), Layer::new(&src, crop))]);
        let out = c.render(&program("a"), &layers, Duration::ZERO).frame;
        assert!(all_pixels(&out, rgb), "{sw}x{sh} {crop:?} -> {ow}x{oh}");
    }
}

#[test]
fn downscale_averages_fine_detail_instead_of_aliasing() {
    // A one-pixel checkerboard: point sampling at 2:1 or 4:1 would return a
    // single phase (all black or all white); area averaging returns grey.
    let mut data = Vec::new();
    for y in 0..64u32 {
        for x in 0..64u32 {
            let v = if (x + y) % 2 == 0 { 0 } else { 255 };
            data.extend([v, v, v, 255]);
        }
    }
    let src = frame(64, 64, data);
    for size in [32, 16] {
        let mut c = compositor(size, size);
        let layers = Layers::from([(id("a"), Layer::new(&src, Rect::FULL))]);
        let out = c.render(&program("a"), &layers, Duration::ZERO).frame;
        assert!(all_pixels(&out, [128, 128, 128]), "{size}");
    }
}

#[test]
fn upscale_interpolates_smoothly() {
    let src = frame(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]);
    let mut c = compositor(8, 1);
    let layers = Layers::from([(id("a"), Layer::new(&src, Rect::FULL))]);
    let out = c.render(&program("a"), &layers, Duration::ZERO).frame;
    let reds: Vec<u8> = out.data.chunks_exact(4).map(|p| p[0]).collect();
    assert_eq!(reds.first(), Some(&0));
    assert_eq!(reds.last(), Some(&255));
    assert!(reds.windows(2).all(|w| w[0] <= w[1]), "{reds:?}");
    assert!(reds.iter().any(|&r| r > 0 && r < 255));
}

#[test]
fn half_alpha_mix_averages() {
    let a = solid(8, 8, [100, 20, 240]);
    let b = solid(16, 16, [200, 60, 40]);
    let mut c = compositor(4, 4);
    let layers = Layers::from([
        (id("a"), Layer::new(&a, Rect::FULL)),
        (id("b"), Layer::new(&b, Rect::FULL)),
    ]);
    let mix = Output {
        base: Some(id("a")),
        overlay: Some((id("b"), 0.5)),
        dip: 0.0,
    };
    let out = c.render(&mix, &layers, Duration::ZERO).frame;
    assert!(all_pixels(&out, [150, 40, 140]));
}

#[test]
fn mix_of_black_and_white_rounds_half_up() {
    let black = solid(4, 4, [0, 0, 0]);
    let white = solid(4, 4, [255, 255, 255]);
    let mut c = compositor(4, 4);
    let layers = Layers::from([
        (id("k"), Layer::new(&black, Rect::FULL)),
        (id("w"), Layer::new(&white, Rect::FULL)),
    ]);
    let mix = Output {
        base: Some(id("k")),
        overlay: Some((id("w"), 0.5)),
        dip: 0.0,
    };
    let out = c.render(&mix, &layers, Duration::ZERO).frame;
    assert!(all_pixels(&out, [128, 128, 128]));
}

#[test]
fn full_alpha_shows_only_the_overlay() {
    let a = solid(4, 4, [10, 20, 30]);
    let b = pattern(4, 4);
    let mut c = compositor(4, 4);
    let layers = Layers::from([
        (id("a"), Layer::new(&a, Rect::FULL)),
        (id("b"), Layer::new(&b, Rect::FULL)),
    ]);
    let out = Output {
        base: Some(id("a")),
        overlay: Some((id("b"), 1.0)),
        dip: 0.0,
    };
    assert_eq!(
        &*c.render(&out, &layers, Duration::ZERO).frame.data,
        &*b.data
    );
}

#[test]
fn dip_one_is_black_and_half_dip_halves() {
    let a = solid(8, 8, [200, 100, 50]);
    let b = solid(8, 8, [10, 10, 10]);
    let mut c = compositor(8, 8);
    let layers = Layers::from([
        (id("a"), Layer::new(&a, Rect::FULL)),
        (id("b"), Layer::new(&b, Rect::FULL)),
    ]);
    let mut out = Output {
        base: Some(id("a")),
        overlay: Some((id("b"), 0.3)),
        dip: 1.0,
    };
    assert!(all_pixels(
        &c.render(&out, &layers, Duration::ZERO).frame,
        [0, 0, 0]
    ));
    out.overlay = None;
    out.dip = 0.5;
    let half = c.render(&out, &layers, Duration::ZERO).frame;
    assert!(all_pixels(&half, [100, 50, 25]));
}

#[test]
fn opacities_are_clamped_and_nan_is_ignored() {
    let a = solid(4, 4, [100, 100, 100]);
    let b = solid(4, 4, [200, 200, 200]);
    let mut c = compositor(4, 4);
    let layers = Layers::from([
        (id("a"), Layer::new(&a, Rect::FULL)),
        (id("b"), Layer::new(&b, Rect::FULL)),
    ]);
    let nan = Output {
        base: Some(id("a")),
        overlay: Some((id("b"), f64::NAN)),
        dip: f64::NAN,
    };
    assert!(all_pixels(
        &c.render(&nan, &layers, Duration::ZERO).frame,
        [100, 100, 100]
    ));
    let over = Output {
        base: Some(id("a")),
        overlay: Some((id("b"), 7.0)),
        dip: -3.0,
    };
    assert!(all_pixels(
        &c.render(&over, &layers, Duration::ZERO).frame,
        [200, 200, 200]
    ));
}

#[test]
fn explicit_black_is_not_an_issue() {
    let mut c = compositor(4, 4);
    let black = Output {
        base: None,
        overlay: None,
        dip: 0.0,
    };
    let out = c.render(&black, &Layers::new(), Duration::ZERO);
    assert!(out.report.is_clean());
    assert!(all_pixels(&out.frame, [0, 0, 0]));
}

#[test]
fn missing_shots_render_black_and_are_reported() {
    let a = solid(4, 4, [100, 60, 20]);
    let layers = Layers::from([(id("a"), Layer::new(&a, Rect::FULL))]);
    let mut c = compositor(4, 4);
    let out = c.render(&program("gone"), &layers, Duration::ZERO);
    assert!(all_pixels(&out.frame, [0, 0, 0]));
    assert_eq!(out.report.issues, vec![LayerIssue::Missing(id("gone"))]);
    // A missing overlay mixes towards black.
    let mix = Output {
        base: Some(id("a")),
        overlay: Some((id("gone"), 0.5)),
        dip: 0.0,
    };
    let out = c.render(&mix, &layers, Duration::ZERO);
    assert!(all_pixels(&out.frame, [50, 30, 10]));
    assert_eq!(out.report.issues, vec![LayerIssue::Missing(id("gone"))]);
}

#[test]
fn same_missing_shot_is_reported_once() {
    let mut c = compositor(2, 2);
    let out = Output {
        base: Some(id("x")),
        overlay: Some((id("x"), 0.5)),
        dip: 0.0,
    };
    let report = c.render(&out, &Layers::new(), Duration::ZERO).report;
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn invalid_crops_and_frames_are_reported_not_drawn() {
    let a = solid(4, 4, [9, 9, 9]);
    let mut broken = solid(4, 4, [9, 9, 9]);
    broken.height = 5; // fields are public; data no longer matches
    let nan = Rect::new(f64::NAN, 0.0, 1.0, 1.0);
    let layers = Layers::from([
        (id("nan"), Layer::new(&a, nan)),
        (id("flat"), Layer::new(&a, Rect::new(0.0, 0.0, 0.0, 1.0))),
        (id("broken"), Layer::new(&broken, Rect::FULL)),
    ]);
    let mut c = compositor(4, 4);
    for shot in ["nan", "flat", "broken"] {
        let out = c.render(&program(shot), &layers, Duration::ZERO);
        assert!(all_pixels(&out.frame, [0, 0, 0]), "{shot}");
        assert!(matches!(
            &out.report.issues[..],
            [LayerIssue::Invalid { shot: s, .. }] if s == &id(shot)
        ));
    }
    let out = c.render(&program("flat"), &layers, Duration::ZERO);
    assert_eq!(
        out.report.issues,
        vec![LayerIssue::Invalid {
            shot: id("flat"),
            reason: CompositorError::InvalidCrop(Rect::new(0.0, 0.0, 0.0, 1.0)),
        }]
    );
}

#[test]
fn out_of_bounds_crops_do_not_panic_and_repeat_the_edge() {
    // Left half red, right half blue.
    let mut data = Vec::new();
    for _ in 0..20 {
        for x in 0..40 {
            data.extend(if x < 20 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            });
        }
    }
    let src = frame(40, 20, data);
    let mut c = compositor(32, 18);
    let crops = [
        Rect::new(-3.0, 0.0, 1.0, 1.0),
        Rect::new(2.5, -4.0, 0.5, 0.5),
        Rect::new(-0.5, -0.5, 2.0, 2.0),
        Rect::new(-9000.0, -9000.0, 9999.0, 9999.0),
        Rect::new(0.999, 0.999, 1e-9, 1e-9),
        Rect::new(0.9, 0.2, 0.5, 0.5),
    ];
    for crop in crops {
        let layers = Layers::from([(id("a"), Layer::new(&src, crop))]);
        let out = c.render(&program("a"), &layers, Duration::ZERO);
        assert!(out.report.is_clean(), "{crop:?}");
        assert_eq!(out.frame.data.len(), 32 * 18 * 4);
    }
    let left = Layers::from([(id("a"), Layer::new(&src, crops[0]))]);
    assert!(all_pixels(
        &c.render(&program("a"), &left, Duration::ZERO).frame,
        [255, 0, 0]
    ));
    let right = Layers::from([(id("a"), Layer::new(&src, crops[1]))]);
    assert!(all_pixels(
        &c.render(&program("a"), &right, Duration::ZERO).frame,
        [0, 0, 255]
    ));
}

#[test]
fn program_frames_are_tagged_and_numbered() {
    let src = solid(4, 4, [1, 2, 3]);
    let layers = Layers::from([(id("a"), Layer::new(&src, Rect::FULL))]);
    let mut config = CompositorConfig::new(8, 6);
    config.program = SourceId("pgm".into());
    let mut c = Compositor::new(config).unwrap();
    for seq in 0..3 {
        let pts = Duration::from_millis(33 * seq);
        let f = c.render(&program("a"), &layers, pts).frame;
        assert_eq!(f.source, SourceId("pgm".into()));
        assert_eq!((f.seq, f.pts, f.width, f.height), (seq, pts, 8, 6));
    }
    assert_eq!(c.frames_rendered(), 3);
    assert_eq!(c.config().width, 8);
}

#[test]
fn released_program_buffers_are_reused() {
    let src = pattern(16, 9);
    let layers = Layers::from([(id("a"), Layer::new(&src, Rect::FULL))]);
    let mut c = compositor(16, 9);
    let first = c.render(&program("a"), &layers, Duration::ZERO).frame;
    let ptr = first.data.as_ptr();
    let held = c.render(&program("a"), &layers, Duration::ZERO).frame;
    assert_ne!(held.data.as_ptr(), ptr, "a held frame is never overwritten");
    drop(first);
    let third = c.render(&program("a"), &layers, Duration::ZERO).frame;
    assert_eq!(third.data.as_ptr(), ptr);
    assert_eq!(&*held.data, &*third.data);
}

#[test]
fn output_does_not_depend_on_thread_count() {
    let a = pattern(97, 61);
    let b = pattern(40, 30);
    let layers = Layers::from([
        (id("a"), Layer::new(&a, Rect::new(0.1, -0.2, 0.7, 0.9))),
        (id("b"), Layer::new(&b, Rect::new(0.3, 0.3, 0.2, 0.2))),
    ]);
    let out = Output {
        base: Some(id("a")),
        overlay: Some((id("b"), 0.37)),
        dip: 0.21,
    };
    let render = || {
        compositor(53, 29)
            .render(&out, &layers, Duration::ZERO)
            .frame
    };
    let parallel = render();
    let single = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap()
        .install(render);
    assert_eq!(&*parallel.data, &*single.data);
}

#[test]
fn tiles_have_the_requested_size_and_source_provenance() {
    let src = pattern(64, 36);
    let mut c = compositor(16, 9);
    for (w, h) in [(320, 180), (1, 1), (7, 3), (64, 36), (200, 50)] {
        let tile = c
            .render_tile(&src, Rect::new(0.2, 0.1, 0.5, 0.5), w, h)
            .unwrap();
        assert_eq!((tile.width, tile.height), (w, h));
        assert_eq!(tile.data.len(), (w * h * 4) as usize);
        assert_eq!(
            (&tile.source, tile.seq, tile.pts),
            (&src.source, src.seq, src.pts)
        );
    }
    let full = c.render_tile(&src, Rect::FULL, 64, 36).unwrap();
    assert_eq!(&*full.data, &*src.data);
}

#[test]
fn tiles_reject_bad_sizes_crops_and_frames() {
    let src = solid(8, 8, [1, 1, 1]);
    let mut c = compositor(4, 4);
    assert_eq!(
        c.render_tile(&src, Rect::FULL, 0, 9).unwrap_err(),
        CompositorError::InvalidSize {
            width: 0,
            height: 9
        }
    );
    assert!(c.render_tile(&src, Rect::FULL, 20_000, 9).is_err());
    let flat = Rect::new(0.0, 0.0, 1.0, 0.0);
    assert_eq!(
        c.render_tile(&src, flat, 4, 4).unwrap_err(),
        CompositorError::InvalidCrop(flat)
    );
    let mut broken = src.clone();
    broken.data = Arc::from(vec![0u8; 3]);
    assert!(matches!(
        c.render_tile(&broken, Rect::FULL, 4, 4),
        Err(CompositorError::BadFrame { len: 3, .. })
    ));
}

#[test]
fn tiles_do_not_disturb_program_rendering() {
    let src = pattern(32, 18);
    let layers = Layers::from([(id("a"), Layer::new(&src, Rect::FULL))]);
    let mut c = compositor(32, 18);
    let before = c.render(&program("a"), &layers, Duration::ZERO).frame;
    let _tiles: Vec<_> = (0..40)
        .map(|_| c.render_tile(&src, Rect::FULL, 8, 4).unwrap())
        .collect();
    let after = c.render(&program("a"), &layers, Duration::ZERO).frame;
    assert_eq!(&*before.data, &*after.data);
    assert_eq!(
        after.seq, 1,
        "tiles do not consume program sequence numbers"
    );
}

#[test]
fn a_large_multiview_held_by_its_viewers_is_recycled() {
    let src = pattern(32, 18);
    let mut c = compositor(32, 18);
    let shots = 30;
    c.reserve_tiles(shots);
    // Three generations held (engine, snapshot, window) while a fourth is
    // rendered: from the fourth refresh on, every tile reuses a buffer.
    let mut held: std::collections::VecDeque<Vec<VideoFrame>> = Default::default();
    let mut fresh = 0;
    for refresh in 0..12 {
        let known: Vec<*const u8> = c.tiles.buffers().map(|b| b.as_ptr()).collect();
        let set: Vec<VideoFrame> = (0..shots)
            .map(|_| c.render_tile(&src, Rect::FULL, 8, 4).unwrap())
            .collect();
        if refresh >= TILE_GENERATIONS {
            fresh += set
                .iter()
                .filter(|t| !known.contains(&t.data.as_ptr()))
                .count();
        }
        // Monitors do not evict tiles, and tiles do not evict monitors.
        let _monitor = c.render_monitor(&src, Rect::FULL, 16, 9).unwrap();
        held.push_back(set);
        if held.len() == TILE_GENERATIONS {
            held.pop_front();
        }
    }
    assert_eq!(fresh, 0, "tiles allocated after warm-up");
}

#[test]
fn monitors_and_tiles_render_the_same_picture() {
    let src = pattern(32, 18);
    let mut c = compositor(32, 18);
    let crop = Rect::new(0.25, 0.25, 0.5, 0.5);
    let tile = c.render_tile(&src, crop, 16, 9).unwrap();
    let monitor = c.render_monitor(&src, crop, 16, 9).unwrap();
    assert_eq!(&*tile.data, &*monitor.data);
    assert_eq!((monitor.seq, monitor.pts), (src.seq, src.pts));
    assert!(c.render_monitor(&src, crop, 0, 9).is_err());
}

#[test]
fn invalid_output_sizes_are_rejected() {
    assert!(Compositor::new(CompositorConfig::new(0, 1080)).is_err());
    assert!(Compositor::new(CompositorConfig::new(1920, 0)).is_err());
    assert!(Compositor::new(CompositorConfig::new(1920, 1080)).is_ok());
}
