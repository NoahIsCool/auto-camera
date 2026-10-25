//! Tests for [`TemplateTracker`] on synthetic frames built in-test.
//!
//! Scenes are 640x360 RGBA frames with smooth value-noise textured squares on
//! a flat or noisy background; the texture is attached to the square, so it
//! moves and scales with it. The contrast check has its own tests in
//! `contrast`, on 1280x720 frames of a lit wall with a faint shadow and
//! textured performers built by `stage`.

use std::sync::Arc;
use std::time::Duration;

use super::*;

mod contrast;
mod stage;

const W: u32 = 640;
const H: u32 = 360;
const FRAME_DT: Duration = Duration::from_micros(33_333);

/// Hash of a lattice point to `0..1`.
fn lattice(ix: i64, iy: i64, seed: u64) -> f64 {
    let mut h = (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    (h % 10_000) as f64 / 10_000.0
}

/// Smooth value noise in `0..1` with lattice spacing 1.
fn value_noise(u: f64, v: f64, seed: u64) -> f64 {
    let (x0, y0) = (u.floor(), v.floor());
    let (tx, ty) = (u - x0, v - y0);
    let (ix, iy) = (x0 as i64, y0 as i64);
    let a = lattice(ix, iy, seed) + (lattice(ix + 1, iy, seed) - lattice(ix, iy, seed)) * tx;
    let b = lattice(ix, iy + 1, seed)
        + (lattice(ix + 1, iy + 1, seed) - lattice(ix, iy + 1, seed)) * tx;
    a + (b - a) * ty
}

/// A textured square: centre and side in full-resolution pixels.
#[derive(Clone, Copy)]
struct Square {
    cx: f64,
    cy: f64,
    side: f64,
    seed: u64,
}

impl Square {
    fn rect(&self) -> Rect {
        Rect::new(
            (self.cx - self.side / 2.0) / W as f64,
            (self.cy - self.side / 2.0) / H as f64,
            self.side / W as f64,
            self.side / H as f64,
        )
    }

    fn center(&self) -> Point {
        Point {
            x: self.cx / W as f64,
            y: self.cy / H as f64,
        }
    }

    fn sample(&self, x: f64, y: f64) -> Option<f64> {
        let (u, v) = (
            (x - self.cx) / self.side + 0.5,
            (y - self.cy) / self.side + 0.5,
        );
        ((0.0..1.0).contains(&u) && (0.0..1.0).contains(&v))
            .then(|| value_noise(u * 6.0, v * 6.0, self.seed))
    }
}

#[derive(Clone, Copy)]
enum Background {
    Flat,
    Noise,
}

fn render(seq: u64, background: Background, squares: &[Square]) -> VideoFrame {
    render_from(SourceId("cam".into()), seq, background, squares)
}

fn render_from(
    source: SourceId,
    seq: u64,
    background: Background,
    squares: &[Square],
) -> VideoFrame {
    let mut data = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let bg = match background {
                Background::Flat => 0.4,
                Background::Noise => 0.3 + 0.4 * value_noise(fx / 9.0, fy / 9.0, 99),
            };
            let v = squares
                .iter()
                .rev()
                .find_map(|s| s.sample(fx, fy))
                .unwrap_or(bg);
            let g = (v * 255.0).round() as u8;
            data.extend_from_slice(&[g, g, g, 255]);
        }
    }
    VideoFrame::new(source, seq, FRAME_DT * seq as u32, W, H, Arc::from(data)).unwrap()
}

fn square(cx: f64, cy: f64, side: f64) -> Square {
    Square {
        cx,
        cy,
        side,
        seed: 7,
    }
}

fn dist(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// Tracks subject 1 started on `squares_at(0)` through `frames` frames.
fn run(
    tracker: &mut TemplateTracker,
    frames: u64,
    background: Background,
    squares_at: impl Fn(u64) -> Vec<Square>,
) -> Vec<TrackObservation> {
    let first = squares_at(0);
    tracker
        .start(
            &render(0, background, &first),
            SubjectId(1),
            first[0].rect(),
        )
        .unwrap();
    (1..frames)
        .map(|seq| tracker.update(&render(seq, background, &squares_at(seq)))[0])
        .collect()
}

#[test]
fn follows_a_moving_textured_square() {
    let mut tracker = TemplateTracker::default();
    let truth = |seq: u64| square(150.0 + 5.0 * seq as f64, 160.0 + 1.5 * seq as f64, 64.0);
    let obs = run(&mut tracker, 50, Background::Noise, |s| vec![truth(s)]);
    for (i, o) in obs.iter().enumerate() {
        let t = truth(i as u64 + 1);
        let err = dist(o.bbox.center(), t.center());
        assert!(err < 0.006, "frame {}: error {err}", i + 1);
        assert!(
            o.confidence > 0.8,
            "frame {}: confidence {}",
            i + 1,
            o.confidence
        );
    }
    let last = obs.last().unwrap();
    assert!((last.bbox.w / truth(49).rect().w - 1.0).abs() < 0.1);
}

#[test]
fn follows_on_a_flat_background_and_across_skipped_frames() {
    let mut tracker = TemplateTracker::default();
    let truth = |seq: u64| square(400.0 - 4.0 * seq as f64, 200.0, 80.0);
    let first = vec![truth(0)];
    tracker
        .start(
            &render(0, Background::Flat, &first),
            SubjectId(1),
            first[0].rect(),
        )
        .unwrap();
    // Every third frame only, as if a latest-frame mailbox dropped the rest.
    for seq in (3..60).step_by(3) {
        let o = tracker.update(&render(seq, Background::Flat, &[truth(seq)]))[0];
        let err = dist(o.bbox.center(), truth(seq).center());
        assert!(
            err < 0.006 && o.confidence > 0.8,
            "frame {seq}: error {err}, confidence {}",
            o.confidence
        );
    }
}

#[test]
fn follows_a_growing_square() {
    let mut tracker = TemplateTracker::default();
    let truth = |seq: u64| square(320.0, 180.0, 50.0 * 1.02f64.powi(seq as i32));
    let obs = run(&mut tracker, 25, Background::Noise, |s| vec![truth(s)]);
    let last = obs.last().unwrap();
    let ratio = last.bbox.w / truth(24).rect().w;
    assert!((ratio - 1.0).abs() < 0.1, "size ratio {ratio}");
    assert!(dist(last.bbox.center(), truth(24).center()) < 0.006);
}

#[test]
fn reports_low_confidence_when_the_target_disappears_and_recovers() {
    let mut tracker = TemplateTracker::default();
    let s = square(300.0, 180.0, 64.0);
    for background in [Background::Flat, Background::Noise] {
        tracker
            .start(&render(0, background, &[s]), SubjectId(1), s.rect())
            .unwrap();
        for seq in 1..5 {
            assert!(tracker.update(&render(seq, background, &[s]))[0].confidence > 0.8);
        }
        for seq in 5..10 {
            let o = tracker.update(&render(seq, background, &[]))[0];
            assert!(
                o.confidence < 0.5,
                "occluded frame {seq}: confidence {}",
                o.confidence
            );
            assert!(
                dist(o.bbox.center(), s.center()) < 0.01,
                "box must not jump"
            );
            assert!(!o.held, "occluded frame {seq}: held back by its score");
        }
        let back = tracker.update(&render(10, background, &[s]))[0];
        assert!(
            back.confidence > 0.8,
            "recovered confidence {}",
            back.confidence
        );
        assert!(dist(back.bbox.center(), s.center()) < 0.006);
    }
}

#[test]
fn coasts_on_velocity_during_a_short_occlusion() {
    let mut tracker = TemplateTracker::default();
    let truth = |seq: u64| square(200.0 + 4.0 * seq as f64, 180.0, 64.0);
    let first = vec![truth(0)];
    tracker
        .start(
            &render(0, Background::Noise, &first),
            SubjectId(1),
            first[0].rect(),
        )
        .unwrap();
    for seq in 1..15 {
        tracker.update(&render(seq, Background::Noise, &[truth(seq)]));
    }
    let before = tracker.observations()[0].bbox.center();
    let hidden = tracker.update(&render(15, Background::Noise, &[]))[0];
    assert!(hidden.confidence < 0.5);
    assert!(hidden.bbox.center().x > before.x, "coasts forward");
    for seq in 16..18 {
        tracker.update(&render(seq, Background::Noise, &[]));
    }
    let o = tracker.update(&render(18, Background::Noise, &[truth(18)]))[0];
    assert!(
        o.confidence > 0.8,
        "reacquired after occlusion: {}",
        o.confidence
    );
    assert!(dist(o.bbox.center(), truth(18).center()) < 0.006);
}

#[test]
fn ambiguous_peaks_give_low_confidence_instead_of_a_jump() {
    let config = TemplateTrackerConfig {
        search_fraction: 1.5,
        max_search_radius: 48,
        ..TemplateTrackerConfig::default()
    };
    let s = square(320.0, 180.0, 48.0);
    let twins = [square(280.0, 180.0, 48.0), square(360.0, 180.0, 48.0)];
    let mut tracker = TemplateTracker::new(config.clone()).unwrap();
    tracker
        .start(&render(0, Background::Noise, &[s]), SubjectId(1), s.rect())
        .unwrap();
    let o = tracker.update(&render(1, Background::Noise, &twins))[0];
    assert!(o.confidence <= 0.3, "ambiguous confidence {}", o.confidence);
    assert!(
        dist(o.bbox.center(), s.center()) < 0.01,
        "box must not jump"
    );
    // Not flagged as held back: a narrow occluder in front of a subject
    // makes ambiguous matches too, and the store must not restart on those.
    assert!(!o.held);

    // Control: a single displaced copy is unambiguous and is followed.
    let mut tracker = TemplateTracker::new(config).unwrap();
    tracker
        .start(&render(0, Background::Noise, &[s]), SubjectId(1), s.rect())
        .unwrap();
    let o = tracker.update(&render(1, Background::Noise, &twins[1..]))[0];
    assert!(o.confidence > 0.8);
    assert!(dist(o.bbox.center(), twins[1].center()) < 0.006);
}

#[test]
fn tracks_a_square_leaving_the_frame_edge() {
    let mut tracker = TemplateTracker::default();
    let truth = |seq: u64| square(560.0 + 4.0 * seq as f64, 180.0, 64.0);
    let obs = run(&mut tracker, 18, Background::Noise, |s| vec![truth(s)]);
    for (i, o) in obs.iter().enumerate() {
        let seq = i as u64 + 1;
        assert!(
            Rect::FULL.contains_rect(&o.bbox),
            "frame {seq}: {:?}",
            o.bbox
        );
        let visible = clamp_box(truth(seq).rect()).unwrap();
        assert!(
            o.confidence > 0.8,
            "frame {seq}: confidence {}",
            o.confidence
        );
        assert!(
            (o.bbox.x - visible.x).abs() < 0.008,
            "frame {seq}: {:?} vs {visible:?}",
            o.bbox
        );
    }
    // By the last frame the square is half outside and the box ends at the edge.
    assert!((obs.last().unwrap().bbox.right() - 1.0).abs() < 1e-9);
}

#[test]
fn starts_on_boxes_touching_or_crossing_the_edges() {
    let mut tracker = TemplateTracker::default();
    let corner = square(20.0, 20.0, 64.0);
    let frame = render(0, Background::Noise, &[corner]);
    tracker.start(&frame, SubjectId(3), corner.rect()).unwrap();
    let start_box = tracker.observations()[0].bbox;
    assert_eq!((start_box.x, start_box.y), (0.0, 0.0));
    let o = tracker.update(&render(1, Background::Noise, &[corner]))[0];
    assert!(o.confidence > 0.8, "corner confidence {}", o.confidence);
    assert!((o.bbox.x - start_box.x).abs() < 0.006 && (o.bbox.w - start_box.w).abs() < 0.01);
    let bottom = square(320.0, 345.0, 50.0);
    tracker
        .start(
            &render(2, Background::Noise, &[bottom]),
            SubjectId(4),
            bottom.rect(),
        )
        .unwrap();
    let o = tracker.update(&render(3, Background::Noise, &[corner, bottom]))[1];
    assert_eq!(o.id, SubjectId(4));
    assert!(o.confidence > 0.8 && (o.bbox.bottom() - 1.0).abs() < 1e-9);
}

#[test]
fn tracks_several_subjects_independently() {
    let mut tracker = TemplateTracker::default();
    let a = |seq: u64| Square {
        seed: 1,
        ..square(150.0 + 4.0 * seq as f64, 120.0, 60.0)
    };
    let b = |seq: u64| Square {
        seed: 2,
        ..square(500.0 - 4.0 * seq as f64, 240.0, 70.0)
    };
    let frame = render(0, Background::Noise, &[a(0), b(0)]);
    tracker.start(&frame, SubjectId(10), a(0).rect()).unwrap();
    tracker.start(&frame, SubjectId(2), b(0).rect()).unwrap();
    assert_eq!(tracker.tracked(), vec![SubjectId(2), SubjectId(10)]);
    for seq in 1..30 {
        let obs = tracker.update(&render(seq, Background::Noise, &[a(seq), b(seq)]));
        assert_eq!(
            obs.iter().map(|o| o.id).collect::<Vec<_>>(),
            vec![SubjectId(2), SubjectId(10)]
        );
        assert!(
            dist(obs[0].bbox.center(), b(seq).center()) < 0.006,
            "b at {seq}"
        );
        assert!(
            dist(obs[1].bbox.center(), a(seq).center()) < 0.006,
            "a at {seq}"
        );
    }
}

#[test]
fn one_pyramid_serves_every_start_and_update_on_a_frame() {
    let mut tracker = TemplateTracker::default();
    let squares = [
        Square {
            seed: 1,
            ..square(120.0, 120.0, 60.0)
        },
        Square {
            seed: 2,
            ..square(320.0, 180.0, 60.0)
        },
        Square {
            seed: 3,
            ..square(520.0, 240.0, 60.0)
        },
    ];
    let frame = render(0, Background::Noise, &squares);
    for (i, s) in squares.iter().enumerate() {
        tracker
            .start(&frame, SubjectId(i as u32 + 1), s.rect())
            .unwrap();
    }
    // A failed start on the same frame reuses it too.
    assert!(
        tracker
            .start(&frame, SubjectId(9), Rect::new(0.5, 0.5, 0.004, 0.004))
            .is_err()
    );
    assert_eq!(tracker.pyramids_built(), 1);
    // The next frame is converted once for the update and a late start.
    let next = render(1, Background::Noise, &squares);
    assert_eq!(tracker.update(&next).len(), 3);
    tracker
        .start(&next, SubjectId(4), squares[0].rect())
        .unwrap();
    assert_eq!(tracker.pyramids_built(), 2);
    // A frame with the same number from another source is not confused
    // with it.
    let other = render_from(SourceId("other".into()), 1, Background::Noise, &squares);
    let _ = tracker.start(&other, SubjectId(5), squares[0].rect());
    assert_eq!(
        tracker.pyramids_built(),
        2,
        "a foreign source is refused first"
    );
    for id in 1..=4 {
        tracker.stop(SubjectId(id));
    }
    tracker
        .start(&other, SubjectId(5), squares[0].rect())
        .unwrap();
    assert_eq!(tracker.pyramids_built(), 3);
}

#[test]
fn rejects_invalid_boxes_and_flat_regions() {
    let mut tracker = TemplateTracker::default();
    let frame = render(0, Background::Flat, &[square(320.0, 180.0, 64.0)]);
    for bbox in [
        Rect::new(f64::NAN, 0.1, 0.1, 0.1),
        Rect::new(0.1, 0.1, 0.0, 0.1),
        Rect::new(1.1, 0.1, 0.1, 0.1),
        Rect::new(0.5, 0.5, 0.004, 0.004),
    ] {
        let err = tracker.start(&frame, SubjectId(1), bbox).unwrap_err();
        assert!(
            matches!(err, VisionError::InvalidBox { .. }),
            "{bbox:?}: {err}"
        );
    }
    let flat = tracker.start(&frame, SubjectId(1), Rect::new(0.02, 0.02, 0.1, 0.1));
    assert!(matches!(flat, Err(VisionError::Untrackable(_))));
    assert!(tracker.tracked().is_empty());
    assert!(tracker.source().is_none());
}

#[test]
fn binds_to_one_source() {
    let mut tracker = TemplateTracker::default();
    let s = square(320.0, 180.0, 64.0);
    tracker
        .start(&render(0, Background::Noise, &[s]), SubjectId(1), s.rect())
        .unwrap();
    let other = render_from(SourceId("other".into()), 1, Background::Noise, &[s]);
    assert!(matches!(
        tracker.start(&other, SubjectId(2), s.rect()),
        Err(VisionError::SourceMismatch { .. })
    ));
    assert!(tracker.update(&other).is_empty());
    assert_eq!(tracker.tracked(), vec![SubjectId(1)]);
    tracker.stop(SubjectId(1));
    assert!(tracker.source().is_none());
    tracker.start(&other, SubjectId(2), s.rect()).unwrap();
    assert_eq!(tracker.source(), Some(&SourceId("other".into())));
}

#[test]
fn stale_and_repeated_frames_do_not_advance_subjects() {
    let mut tracker = TemplateTracker::default();
    let truth = |seq: u64| square(200.0 + 6.0 * seq as f64, 180.0, 64.0);
    tracker
        .start(
            &render(5, Background::Noise, &[truth(5)]),
            SubjectId(1),
            truth(5).rect(),
        )
        .unwrap();
    let start = tracker.observations();
    // Same frame as the start, and an older frame: nothing changes.
    assert_eq!(
        tracker.update(&render(5, Background::Noise, &[truth(5)])),
        start
    );
    assert_eq!(
        tracker.update(&render(3, Background::Noise, &[truth(3)])),
        start
    );
    let next = tracker.update(&render(6, Background::Noise, &[truth(6)]));
    assert!(dist(next[0].bbox.center(), truth(6).center()) < 0.006);
    assert_eq!(
        tracker.update(&render(6, Background::Noise, &[truth(6)])),
        next
    );
}

#[test]
fn restart_replaces_a_subject_and_unknown_stop_is_ignored() {
    let mut tracker = TemplateTracker::default();
    let (a, b) = (
        square(150.0, 180.0, 64.0),
        Square {
            seed: 3,
            ..square(450.0, 180.0, 64.0)
        },
    );
    let frame = render(0, Background::Noise, &[a, b]);
    tracker.start(&frame, SubjectId(1), a.rect()).unwrap();
    tracker.start(&frame, SubjectId(1), b.rect()).unwrap();
    tracker.stop(SubjectId(99));
    assert_eq!(tracker.tracked(), vec![SubjectId(1)]);
    let o = tracker.update(&render(1, Background::Noise, &[a, b]))[0];
    assert!(dist(o.bbox.center(), b.center()) < 0.006);
}

#[test]
fn update_without_subjects_is_empty() {
    let mut tracker = TemplateTracker::default();
    assert!(tracker.update(&render(0, Background::Flat, &[])).is_empty());
    assert!(tracker.tracked().is_empty());
}

#[test]
fn small_frames_are_not_enlarged() {
    let data: Arc<[u8]> = (0..64 * 48 * 4)
        .map(|i| ((i * 37) % 251) as u8)
        .collect::<Vec<_>>()
        .into();
    let frame = VideoFrame::new(SourceId("cam".into()), 0, Duration::ZERO, 64, 48, data).unwrap();
    let mut tracker = TemplateTracker::default();
    let p = tracker.pyramid(&frame);
    assert_eq!((p.levels[0].img.width, p.levels[0].img.height), (64, 48));
}

#[test]
fn config_validation_rejects_out_of_range_values() {
    assert!(TemplateTrackerConfig::default().validate().is_ok());
    let bad = [
        TemplateTrackerConfig {
            work_width: 8,
            ..Default::default()
        },
        TemplateTrackerConfig {
            template_size: 2,
            ..Default::default()
        },
        TemplateTrackerConfig {
            search_fraction: f64::NAN,
            ..Default::default()
        },
        TemplateTrackerConfig {
            min_search_radius: 30,
            ..Default::default()
        },
        TemplateTrackerConfig {
            scale_step: 0.6,
            ..Default::default()
        },
        TemplateTrackerConfig {
            accept_score: 1.5,
            ..Default::default()
        },
        TemplateTrackerConfig {
            learning_rate: -0.1,
            ..Default::default()
        },
        TemplateTrackerConfig {
            velocity_decay: 2.0,
            ..Default::default()
        },
        TemplateTrackerConfig {
            min_visible: 0.0,
            ..Default::default()
        },
        TemplateTrackerConfig {
            nominal_dt: 0.0,
            ..Default::default()
        },
        TemplateTrackerConfig {
            min_box_px: 1.0,
            ..Default::default()
        },
    ];
    for config in bad {
        assert!(matches!(
            TemplateTracker::new(config),
            Err(VisionError::InvalidConfig(_))
        ));
    }
}

#[test]
fn interval_falls_back_and_caps() {
    let c = TemplateTrackerConfig::default();
    assert_eq!(
        interval(&c, Duration::from_secs(1), Duration::from_secs(1)),
        c.nominal_dt
    );
    assert_eq!(
        interval(&c, Duration::from_secs(2), Duration::from_secs(1)),
        c.nominal_dt
    );
    assert_eq!(
        interval(&c, Duration::ZERO, Duration::from_secs(5)),
        c.max_dt
    );
    assert!((interval(&c, Duration::ZERO, Duration::from_millis(40)) - 0.04).abs() < 1e-12);
}

#[test]
fn canonical_size_keeps_aspect() {
    assert_eq!(canonical_size(64.0, 32.0, 32), (32, 16));
    assert_eq!(canonical_size(10.0, 40.0, 32), (8, 32));
    assert_eq!(canonical_size(100.0, 1.0, 32), (32, 4));
}
