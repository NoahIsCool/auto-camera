//! Framing on hand-computed crops: a clipped crown, headroom at both ends
//! of a band, cut-line errors, containment, and the scored metrics.

use ac_core::{Rect, ShotSize};

use super::*;
use crate::scorers::testkit::*;
use crate::thresholds::Thresholds;

/// A person 0.7 high at x 0.4 to 0.6: crown 0.2, chin 0.3 (a head 0.1
/// high), shoulders 0.35, waist 0.6, feet at the box's bottom, 0.9.
fn p() -> PersonBox {
    lined(
        person(1, Rect::new(0.4, 0.2, 0.2, 0.7)),
        0.2,
        0.3,
        0.35,
        0.6,
    )
}

/// The default framing limits.
fn limits() -> FramingLimits {
    Thresholds::default().framing
}

#[test]
fn a_crop_that_clips_the_crown_loses_the_head() {
    // Top edge 0.22, below the crown at 0.2: headroom (0.2 - 0.22) / 0.4
    // = -0.05, outside every band, and the head is cut.
    let clipped = Rect::new(0.3, 0.22, 0.4, 0.4);
    assert!((headroom(&clipped, 0.2).unwrap() + 0.05).abs() < 1e-12);
    assert_eq!(
        keeps_head_and_torso(&clipped, &p(), Some(ShotSize::Close)),
        Some(false)
    );
    for size in [
        ShotSize::Close,
        ShotSize::MediumClose,
        ShotSize::Medium,
        ShotSize::Full,
    ] {
        assert!(!in_band(-0.05, band(&limits(), size).unwrap()), "{size:?}");
    }
    // Raised to 0.18 the crown is 0.02 / 0.4 = 0.05 below the top: the
    // head (0.2 to 0.3) and the shoulders (0.35) are inside 0.18 to 0.58,
    // in the medium close-up's band 0.05 to 0.08.
    let kept = Rect::new(0.3, 0.18, 0.4, 0.4);
    assert_eq!(
        keeps_head_and_torso(&kept, &p(), Some(ShotSize::MediumClose)),
        Some(true)
    );
    assert!(in_band(
        headroom(&kept, 0.2).unwrap(),
        limits().headroom_medium_close
    ));
}

#[test]
fn headroom_at_both_band_edges_is_in_band_and_just_past_them_is_not() {
    let close = limits().headroom_close;
    // Crown on the top edge: headroom 0, the close-up band's low end.
    let at_low = headroom(&Rect::new(0.3, 0.2, 0.25, 0.25), 0.2).unwrap();
    assert_eq!(at_low, 0.0);
    assert!(in_band(at_low, close));
    // Crown 0.01 below a 0.25-high frame's top: 0.01 / 0.25 = 0.04, the
    // high end (0.04000000000000004 in floating point, inside the slack).
    let at_high = headroom(&Rect::new(0.3, 0.19, 0.25, 0.25), 0.2).unwrap();
    assert!((at_high - 0.04).abs() < 1e-12);
    assert!(in_band(at_high, close));
    // 0.0101 / 0.25 = 0.0404 and -0.0001 / 0.25 = -0.0004: both out.
    assert!(!in_band(
        headroom(&Rect::new(0.3, 0.1899, 0.25, 0.25), 0.2).unwrap(),
        close
    ));
    assert!(!in_band(
        headroom(&Rect::new(0.3, 0.2001, 0.25, 0.25), 0.2).unwrap(),
        close
    ));
    // The medium band, 0.07 to 0.10, on a 0.5-high frame: crowns 0.035
    // and 0.05 below the top.
    let medium = limits().headroom_medium;
    assert!(in_band(
        headroom(&Rect::new(0.2, 0.165, 0.5, 0.5), 0.2).unwrap(),
        medium
    ));
    assert!(in_band(
        headroom(&Rect::new(0.2, 0.15, 0.5, 0.5), 0.2).unwrap(),
        medium
    ));
    assert!(!in_band(
        headroom(&Rect::new(0.2, 0.17, 0.5, 0.5), 0.2).unwrap(),
        medium
    ));
    // A frame with no height has no headroom.
    assert_eq!(headroom(&Rect::new(0.2, 0.1, 0.5, 0.0), 0.2), None);
}

#[test]
fn the_torso_check_follows_the_shot_size() {
    // Bottom edge 0.34, above the shoulders at 0.35: a close-up (cut at
    // the shoulders) keeps what it should, a medium shot does not.
    let crop = Rect::new(0.3, 0.18, 0.4, 0.16);
    assert_eq!(
        keeps_head_and_torso(&crop, &p(), Some(ShotSize::Close)),
        Some(true)
    );
    assert_eq!(
        keeps_head_and_torso(&crop, &p(), Some(ShotSize::Medium)),
        Some(false)
    );
    // An unknown size is checked for the head alone.
    assert_eq!(keeps_head_and_torso(&crop, &p(), None), Some(true));
    // A full shot needs the waist (0.6) inside too: 0.18 to 0.58 is not.
    let full = Rect::new(0.3, 0.18, 0.4, 0.4);
    assert_eq!(
        keeps_head_and_torso(&full, &p(), Some(ShotSize::Full)),
        Some(false)
    );
    // The box's centre line, x = 0.5, must be inside: 0.55 to 0.95 is not.
    let aside = Rect::new(0.55, 0.18, 0.4, 0.4);
    assert_eq!(
        keeps_head_and_torso(&aside, &p(), Some(ShotSize::Close)),
        Some(false)
    );
    // A line the size needs, unlabelled: not measured.
    let mut no_chin = p();
    no_chin.lines.chin = None;
    assert_eq!(
        keeps_head_and_torso(&full, &no_chin, Some(ShotSize::Close)),
        None
    );
    let mut no_shoulder = p();
    no_shoulder.lines.shoulder = None;
    assert_eq!(
        keeps_head_and_torso(&full, &no_shoulder, Some(ShotSize::Close)),
        Some(true)
    );
    assert_eq!(
        keeps_head_and_torso(&full, &no_shoulder, Some(ShotSize::Medium)),
        None
    );
}

#[test]
fn lower_cut_errors_are_in_head_heights_from_the_size_line() {
    // Head 0.3 - 0.2 = 0.1. Close-up cut at the shoulders (0.35), bottom
    // edge at 0.4: 0.05 / 0.1 = 0.5 head heights below.
    let close = Rect::new(0.3, 0.2, 0.4, 0.2);
    assert!((lower_cut_error(&close, &p(), ShotSize::Close).unwrap() - 0.5).abs() < 1e-12);
    // Medium shot cut at the waist (0.6), bottom edge at 0.55: -0.5.
    let medium = Rect::new(0.3, 0.15, 0.4, 0.4);
    assert!((lower_cut_error(&medium, &p(), ShotSize::Medium).unwrap() + 0.5).abs() < 1e-12);
    // Full shot cut at the feet (0.9), bottom edge at 0.9: 0.
    let full = Rect::new(0.2, 0.1, 0.8, 0.8);
    assert!(lower_cut_error(&full, &p(), ShotSize::Full).unwrap().abs() < 1e-12);
    // Feet out of the picture, no sternum line, no head: not measurable.
    let mut cut_off = p();
    cut_off.truncation.bottom = true;
    assert_eq!(lower_cut_error(&full, &cut_off, ShotSize::Full), None);
    assert_eq!(lower_cut_error(&full, &p(), ShotSize::MediumClose), None);
    let mut flat = p();
    flat.lines.chin = Some(0.2);
    assert_eq!(lower_cut_error(&close, &flat, ShotSize::Close), None);
}

#[test]
fn containment_cuts_the_box_at_the_frame_bottom() {
    // The box (0.4, 0.2, 0.2, 0.7) cut at a bottom edge of 0.58 keeps 0.38
    // of height, inside 0.3..0.7 by 0.18..0.58.
    let crop = Rect::new(0.3, 0.18, 0.4, 0.4);
    let cut = cut_at_frame_bottom(&p().bbox, &crop);
    assert!((cut.h - 0.38).abs() < 1e-12 && crop.contains_rect(&cut));
    // Moved right to 0.55..0.75 it leaves the crop at the side.
    let aside = cut_at_frame_bottom(&Rect::new(0.55, 0.2, 0.2, 0.7), &crop);
    assert!(!crop.contains_rect(&aside));
    // Wholly below the bottom edge: no height, still not inside.
    let below = cut_at_frame_bottom(&Rect::new(0.4, 0.7, 0.2, 0.2), &crop);
    assert_eq!(below.h, 0.0);
    assert!(!crop.contains_rect(&below));
}

#[test]
fn the_scored_metrics_count_on_air_frames() {
    // Three keyframes with the close-up on air at headroom 0, 0.04 and
    // 0.08 (crop tops 0.2, 0.19 and 0.18 on a 0.25-high frame): two of
    // three in the close-up band. A fourth with the wide on air adds
    // nothing; a fifth frames person 2, who has no lines.
    let other = person(2, Rect::new(0.05, 0.3, 0.15, 0.6));
    let labels = labels(
        2,
        (0..5)
            .map(|i| keyframe(i * 10, vec![p(), other.clone()]))
            .collect(),
    );
    let tops = [0.2, 0.19, 0.18];
    let mut trace: Vec<_> = tops
        .iter()
        .enumerate()
        .map(|(i, &top)| {
            let crop = Rect::new(0.375, top, 0.25, 0.25);
            record(
                i as u64 * 10,
                vec![subject(1, p().bbox)],
                vec![wide(), shot("cam/1/close", Some(1), crop)],
                Some("cam/1/close"),
            )
        })
        .collect();
    trace.push(record(
        30,
        vec![subject(1, p().bbox)],
        vec![wide()],
        Some("cam/wide"),
    ));
    trace.push(record(
        40,
        vec![subject(2, other.bbox)],
        vec![shot(
            "cam/2/close",
            Some(2),
            Rect::new(0.0, 0.25, 0.25, 0.25),
        )],
        Some("cam/2/close"),
    ));
    let r = report(&labels, &trace);
    let close = r.metric("framing.headroom.close", None).unwrap();
    assert_eq!(close.samples, 3);
    assert!((close.value().unwrap() - 2.0 / 3.0).abs() < 1e-12);
    assert_eq!(close.details["band_high"], 0.04);
    assert!((close.details["highest"] - 0.08).abs() < 1e-12);
    // The head (0.2 to 0.3) is inside each frame; person 2's frame has no
    // lines and is not counted.
    let kept = r.metric("framing.head_torso_inside", None).unwrap();
    assert_eq!((kept.value(), kept.samples), (Some(1.0), 3));
    assert_eq!(kept.details["unlabelled"], 0.0);
    // Close-up cut at the shoulders (0.35): bottom edges 0.45, 0.44 and
    // 0.43 lie 1.0, 0.9 and 0.8 head heights below; the worst is 1.0.
    let cut = r.metric("framing.lower_cut_error", None).unwrap();
    assert!((cut.value().unwrap() - 1.0).abs() < 1e-12);
    assert_eq!(
        r.metric("framing.joint_clearance", None).unwrap().value(),
        None
    );
    assert_eq!(
        r.metric("framing.headroom.medium", None).unwrap().value(),
        None
    );
}
