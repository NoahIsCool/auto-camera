//! Micro benchmark of the CPU compositor: milliseconds per 1080p program
//! frame rendered from a 4K (3840x2160) source.
//!
//! Run with `cargo run --release -p ac-compositor --example bench`, optionally
//! followed by the number of frames per case (default 60). Each case renders a
//! few warm-up frames first so that scratch memory and the buffer pool are
//! settled, then reports the mean over the timed frames. Frames are dropped as
//! soon as they are rendered, as a fast consumer would, so buffers recycle.
//!
//! This is a measurement tool, not a test: it uses the wall clock, which the
//! library itself never does.

use std::time::{Duration, Instant};

use ac_compositor::{Compositor, CompositorConfig, Layer, Layers};
use ac_core::{Rect, ShotId, SourceId};
use ac_media::VideoFrame;
use ac_switcher::Output;

/// Frames rendered before timing starts.
const WARM_UP: u32 = 5;

/// A 4K frame with detail in every channel, so no case is trivially cached.
fn source_4k(name: &str) -> VideoFrame {
    let (w, h) = (3840u32, 2160u32);
    let mut data = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            data.extend([(x ^ y) as u8, (x / 15) as u8, (y / 8) as u8, 255]);
        }
    }
    VideoFrame::new(SourceId(name.into()), 0, Duration::ZERO, w, h, data.into())
        .expect("dimensions match")
}

/// Mean milliseconds of `frames` calls to `render`, after a warm-up.
fn time_ms(frames: u32, mut render: impl FnMut()) -> f64 {
    for _ in 0..WARM_UP {
        render();
    }
    let start = Instant::now();
    for _ in 0..frames {
        render();
    }
    start.elapsed().as_secs_f64() * 1000.0 / f64::from(frames.max(1))
}

fn main() {
    let frames: u32 = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(60);
    let cam_a = source_4k("cam-a");
    let cam_b = source_4k("cam-b");
    let wide = ShotId("wide".into());
    let close = ShotId("close".into());
    let layers = Layers::from([
        (wide.clone(), Layer::new(&cam_a, Rect::FULL)),
        // A 2.5x zoom: magnified from 1536x864 source pixels.
        (
            close.clone(),
            Layer::new(&cam_b, Rect::new(0.31, 0.12, 0.4, 0.4)),
        ),
    ]);
    let mut compositor =
        Compositor::new(CompositorConfig::new(1920, 1080)).expect("valid output size");
    let mut run = |label: &str, output: Output| {
        let ms = time_ms(frames, || {
            let rendered = compositor.render(&output, &layers, Duration::ZERO);
            assert!(rendered.report.is_clean());
        });
        println!("{label:<34} {ms:8.2} ms/frame  ({:6.1} fps)", 1000.0 / ms);
    };

    println!(
        "4K source -> 1080p program, {frames} frames per case, {} rayon threads",
        rayon::current_num_threads()
    );
    run(
        "wide (4K -> 1080p area average)",
        Output {
            base: Some(wide.clone()),
            overlay: None,
            dip: 0.0,
        },
    );
    run(
        "close-up (2.5x bilinear zoom)",
        Output {
            base: Some(close.clone()),
            overlay: None,
            dip: 0.0,
        },
    );
    run(
        "mix wide -> close at 50%",
        Output {
            base: Some(wide.clone()),
            overlay: Some((close.clone(), 0.5)),
            dip: 0.0,
        },
    );
    run(
        "dip at 100% (black, no sampling)",
        Output {
            base: Some(wide.clone()),
            overlay: None,
            dip: 1.0,
        },
    );

    let ms = time_ms(frames, || {
        for crop in [Rect::FULL, Rect::new(0.31, 0.12, 0.4, 0.4)] {
            compositor
                .render_tile(&cam_a, crop, 480, 270)
                .expect("valid tile");
        }
    });
    println!("{:<34} {:8.2} ms/frame", "two 480x270 multiview tiles", ms);
}
