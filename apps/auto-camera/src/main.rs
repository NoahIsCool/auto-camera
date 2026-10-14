//! Auto Camera command-line entry point.
//!
//! Until capture, inference and output land, this runs a deterministic
//! synthetic rehearsal: a subject walks across a 16:9 source, a medium shot
//! follows it, and the switcher mixes from the wide shot to it.

use ac_core::{Point, Preset, Rect, ShotId, ShotSize};
use ac_framing::{FramerConfig, ShotFramer, compose};
use ac_switcher::Switcher;

/// Source and output aspect ratio of the synthetic rehearsal.
const ASPECT: f64 = 16.0 / 9.0;
/// Output frame interval.
const DT: f64 = 1.0 / 30.0;

fn main() {
    let preset = Preset::new("medium", "Medium", ShotSize::Medium);
    let subject_at = |t: f64| {
        Rect::from_center(
            Point {
                x: 0.3 + 0.1 * t,
                y: 0.45,
            },
            0.08,
            0.3,
        )
    };

    let initial = compose(&subject_at(0.0), &preset, ASPECT, ASPECT);
    let mut framer = ShotFramer::new(FramerConfig::default(), initial, ASPECT, ASPECT);
    let mut switcher = Switcher::new();
    let (wide, medium) = (ShotId("wide".into()), ShotId("medium".into()));
    switcher
        .set_preview(wide)
        .expect("idle switcher accepts preview");
    switcher.cut(|_| true).expect("wide shot is available");
    switcher
        .set_preview(medium)
        .expect("idle switcher accepts preview");

    for frame in 0..=120 {
        let t = frame as f64 * DT;
        if frame == 30 {
            switcher.auto(|_| true).expect("medium shot is available");
        }
        let crop = framer.step(Some(compose(&subject_at(t), &preset, ASPECT, ASPECT)), DT);
        switcher.tick(DT);
        if frame % 30 == 0 {
            let out = switcher.output();
            println!(
                "t={t:4.1}s crop centre=({:.3}, {:.3}) width={:.3} program={:?} overlay={:?}",
                crop.center.x, crop.center.y, crop.width, out.base, out.overlay
            );
        }
    }
}
