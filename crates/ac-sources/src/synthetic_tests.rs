//! Tests for the synthetic pattern and source.

use super::*;
use crate::clock::VirtualClock;
use std::time::Instant;

/// Polls `cond` until it holds or `timeout` passes.
fn eventually(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

/// RGBA of pixel `(x, y)` of a frame.
fn px(frame: &VideoFrame, x: usize, y: usize) -> [u8; 4] {
    frame.pixel(x as u32, y as u32).unwrap()
}

#[test]
fn identical_parameters_give_identical_frames_and_truth() {
    let a = SyntheticPattern::new(96, 54, 30.0, 3).unwrap();
    let b = SyntheticPattern::new(96, 54, 30.0, 3).unwrap();
    let id = SourceId("syn".into());
    for seq in [0, 1, 17, 300, 100_000] {
        let (fa, fb) = (a.frame(&id, seq), b.frame(&id, seq));
        assert_eq!(fa.data, fb.data, "frame {seq}");
        assert_eq!(fa.pts, a.frame_rate().pts(seq));
        assert_eq!(a.ground_truth(fa.pts), b.ground_truth(fb.pts));
    }
}

#[test]
fn boxes_stay_inside_the_frame_and_move_smoothly() {
    let p = SyntheticPattern::new(320, 180, 30.0, 6).unwrap();
    let mut prev = p.ground_truth(Duration::ZERO);
    for seq in 1..3000 {
        let now = p.ground_truth(p.frame_rate().pts(seq));
        for (a, b) in prev.iter().zip(&now) {
            let r = b.bbox;
            assert!(r.x >= 0.0 && r.y >= 0.0, "{r:?}");
            assert!(
                r.right() <= 1.0 + 1e-12 && r.bottom() <= 1.0 + 1e-12,
                "{r:?}"
            );
            let step = (a.bbox.center().x - b.bbox.center().x).abs()
                + (a.bbox.center().y - b.bbox.center().y).abs();
            assert!(step < 0.01, "jump of {step} at frame {seq}");
            assert_eq!((a.bbox.w, a.bbox.h), (b.bbox.w, b.bbox.h));
        }
        prev = now;
    }
}

#[test]
fn subjects_start_spread_out_and_actually_move() {
    let p = SyntheticPattern::new(320, 180, 30.0, 4).unwrap();
    let start = p.ground_truth(Duration::ZERO);
    let later = p.ground_truth(Duration::from_secs(2));
    for (i, gt) in start.iter().enumerate() {
        assert_eq!(gt.id, SubjectId(i as u32));
        assert!((gt.bbox.x - (1.0 - gt.bbox.w) * i as f64 / 4.0).abs() < 1e-12);
        assert!((gt.bbox.x - later[i].bbox.x).abs() > 0.05);
    }
}

#[test]
fn drawn_boxes_match_ground_truth() {
    let (w, h) = (320usize, 180usize);
    let p = SyntheticPattern::new(w as u32, h as u32, 30.0, 5).unwrap();
    let id = SourceId("syn".into());
    for seq in [0, 45, 451] {
        let frame = p.frame(&id, seq);
        let truth = p.ground_truth(frame.pts);
        for (i, gt) in truth.iter().enumerate() {
            let covered_later = |x: f64, y: f64| {
                truth[i + 1..].iter().any(|o| {
                    x >= o.bbox.x && x < o.bbox.right() && y >= o.bbox.y && y < o.bbox.bottom()
                })
            };
            let c = gt.bbox.center();
            if !covered_later(c.x, c.y) {
                let (cx, cy) = ((c.x * w as f64) as usize, (c.y * h as f64) as usize);
                assert_eq!(px(&frame, cx, cy), gt.color, "subject {i} at frame {seq}");
            }
            // One pixel beyond the right edge is not this subject's colour
            // unless another subject is there.
            let right = (gt.bbox.right() * w as f64 + 1.0) as usize;
            let row = (c.y * h as f64) as usize;
            let rx = (right as f64 + 0.5) / w as f64;
            let ry = (row as f64 + 0.5) / h as f64;
            let other_there = truth.iter().enumerate().any(|(j, o)| {
                j != i
                    && rx >= o.bbox.x
                    && rx < o.bbox.right()
                    && ry >= o.bbox.y
                    && ry < o.bbox.bottom()
            });
            if right < w && !other_there {
                assert_ne!(
                    px(&frame, right, row),
                    gt.color,
                    "subject {i} at frame {seq}"
                );
            }
        }
    }
}

#[test]
fn drawn_area_matches_box_area() {
    let (w, h) = (400u32, 300u32);
    let p = SyntheticPattern::new(w, h, 25.0, 1).unwrap();
    let frame = p.frame(&SourceId("s".into()), 10);
    let gt = p.ground_truth(frame.pts)[0];
    let painted = frame
        .data
        .chunks_exact(4)
        .filter(|c| *c == gt.color)
        .count() as f64;
    let expected = gt.bbox.w * w as f64 * gt.bbox.h * h as f64;
    let edge = 2.0 * (gt.bbox.w * w as f64 + gt.bbox.h * h as f64);
    assert!(
        (painted - expected).abs() <= edge / 2.0 + 1.0,
        "{painted} vs {expected}"
    );
}

#[test]
fn colours_are_distinct_from_each_other_and_the_background() {
    let p = SyntheticPattern::new(64, 64, 30.0, SyntheticPattern::MAX_SUBJECTS).unwrap();
    let colors: Vec<[u8; 4]> = p
        .ground_truth(Duration::ZERO)
        .iter()
        .map(|g| g.color)
        .collect();
    for (i, a) in colors.iter().enumerate() {
        assert_ne!(*a, SyntheticPattern::BACKGROUND);
        assert_ne!(*a, SyntheticPattern::GRID);
        assert_eq!(a[3], 255);
        for b in &colors[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn no_subjects_leaves_only_background_and_grid() {
    let p = SyntheticPattern::new(64, 36, 30.0, 0).unwrap();
    assert_eq!(p.subject_count(), 0);
    assert!(p.ground_truth(Duration::from_secs(3)).is_empty());
    let f = p.frame(&SourceId("s".into()), 5);
    assert!(
        f.data
            .chunks_exact(4)
            .all(|c| c == SyntheticPattern::BACKGROUND || c == SyntheticPattern::GRID)
    );
    assert_eq!(px(&f, 8, 0), SyntheticPattern::GRID);
    assert_eq!(px(&f, 0, 0), SyntheticPattern::BACKGROUND);
}

#[test]
fn tiny_frames_work_without_grid() {
    let p = SyntheticPattern::new(1, 1, 30.0, 2).unwrap();
    let f = p.frame(&SourceId("s".into()), 0);
    assert_eq!((f.width, f.height, f.data.len()), (1, 1, 4));
    let p = SyntheticPattern::new(15, 15, 30.0, 0).unwrap();
    let f = p.frame(&SourceId("s".into()), 0);
    assert!(
        f.data
            .chunks_exact(4)
            .all(|c| c == SyntheticPattern::BACKGROUND)
    );
}

#[test]
fn invalid_parameters_are_rejected() {
    let cases = [
        (0, 10, 30.0, 1),
        (10, 0, 30.0, 1),
        (SyntheticPattern::MAX_SIDE + 1, 10, 30.0, 1),
        (10, 10, 0.0, 1),
        (10, 10, f64::NAN, 1),
        (10, 10, -5.0, 1),
        (10, 10, 30.0, SyntheticPattern::MAX_SUBJECTS + 1),
    ];
    for (w, h, fps, n) in cases {
        assert!(
            matches!(
                SyntheticPattern::new(w, h, fps, n),
                Err(MediaError::Open(_))
            ),
            "{w}x{h}@{fps} n={n}"
        );
    }
}

#[test]
fn render_into_checks_the_buffer_size() {
    let p = SyntheticPattern::new(4, 4, 30.0, 1).unwrap();
    let mut short = vec![0u8; 63];
    assert!(matches!(
        p.render_into(Duration::ZERO, &mut short),
        Err(MediaError::BadFrameSize { len: 63, .. })
    ));
}

#[test]
fn unpaced_source_delivers_the_pattern_with_increasing_seq() {
    let pattern = SyntheticPattern::new(64, 36, 30.0, 2).unwrap();
    let expected = pattern.clone();
    let id = SourceId("syn".into());
    let mut src =
        SyntheticSource::start_with_clock(id.clone(), pattern, Arc::new(VirtualClock::new()))
            .unwrap();
    assert_eq!(src.info().audio, None);
    assert_eq!((src.info().width, src.info().height), (64, 36));
    let mut seen = Vec::new();
    assert!(eventually(Duration::from_secs(10), || {
        if let Some(f) = src.latest() {
            seen.push(f);
        }
        seen.last().is_some_and(|f| f.seq >= 50)
    }));
    assert_eq!(src.state(), SourceState::Running);
    assert!(seen.windows(2).all(|w| w[0].seq <= w[1].seq));
    let last = seen.last().unwrap();
    assert_eq!(last.source, id);
    assert_eq!(last.pts, expected.frame_rate().pts(last.seq));
    assert_eq!(last.data, expected.frame(&id, last.seq).data);
    assert_eq!(src.ground_truth(last.pts), expected.ground_truth(last.pts));
    assert!(src.drain_audio().is_empty());
    src.stop();
    src.stop();
    assert_eq!(src.state(), SourceState::Stopped);
}

#[test]
fn real_time_source_is_paced() {
    let pattern = SyntheticPattern::new(32, 18, 50.0, 1).unwrap();
    let started = Instant::now();
    let src = SyntheticSource::start(SourceId("syn".into()), pattern).unwrap();
    assert!(eventually(Duration::from_secs(5), || src
        .latest()
        .is_some()));
    std::thread::sleep(Duration::from_millis(200));
    let seq = src.latest().unwrap().seq;
    let allowed = (started.elapsed().as_secs_f64() * 50.0).ceil() as u64 + 1;
    assert!(seq <= allowed, "seq {seq} ahead of real time ({allowed})");
    assert!(seq >= 1);
    drop(src);
}
