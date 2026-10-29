//! Tests for the rules on the people of a keyframe: track references,
//! boxes inside the picture, visibility and body lines.

use super::*;

#[test]
fn people_name_declared_tracks_once_per_keyframe() {
    let mut labels = sample();
    labels.keyframes[1].people[1].track = TrackId(9);
    assert_rejected(
        &labels,
        "keyframes[1].people[1].track",
        "unknown track id 9",
    );

    let mut labels = sample();
    labels.keyframes[0].people[1].track = TrackId(1);
    assert_rejected(
        &labels,
        "keyframes[0].people[1].track",
        "already has a box on this keyframe, people[0]",
    );

    // A keyframe with nobody on it is fine.
    let mut labels = sample();
    labels.keyframes[1].people.clear();
    assert_eq!(labels.validate(), Ok(()));
}

#[test]
fn boxes_outside_the_picture_are_rejected() {
    let cases: [Case<Rect>; 8] = [
        (|b| b.x = -0.1, "bbox.x", "-0.1 is outside 0 to 1"),
        (|b| b.y = 1.2, "bbox.y", "1.2 is outside 0 to 1"),
        (|b| b.w = 0.75, "bbox", "right edge x + w = 1.125 is past 1"),
        (
            |b| b.h = 0.875,
            "bbox",
            "bottom edge y + h = 1.125 is past 1",
        ),
        (|b| b.w = 0.0, "bbox.w", "not a positive size"),
        (|b| b.h = -0.2, "bbox.h", "not a positive size"),
        (|b| b.x = f64::NAN, "bbox.x", "not a finite number"),
        (|b| b.w = f64::INFINITY, "bbox.w", "not a finite number"),
    ];
    for (break_it, field, words) in cases {
        let mut labels = sample();
        let person = pastor(&mut labels);
        person.lines = BodyLines::default();
        break_it(&mut person.bbox);
        assert_rejected(&labels, &format!("keyframes[0].people[0].{field}"), words);
    }
}

#[test]
fn boxes_touching_the_edges_pass_despite_rounding() {
    let mut labels = sample();
    let person = pastor(&mut labels);
    person.lines = BodyLines::default();
    // 0.9 + 0.1 and 0.3 + 0.7 need not be exactly 1 in binary.
    person.bbox = Rect::new(0.9, 0.3, 1.0 - 0.9 + 1e-12, 0.7);
    assert_eq!(labels.validate(), Ok(()));
    pastor(&mut labels).bbox = Rect::new(0.0, 0.0, 1.0, 1.0);
    assert_eq!(labels.validate(), Ok(()));
}

#[test]
fn visibility_is_a_fraction_below_one_when_truncated() {
    let mut labels = sample();
    pastor(&mut labels).visibility = 1.5;
    assert_rejected(
        &labels,
        "keyframes[0].people[0].visibility",
        "outside 0 to 1",
    );

    let mut labels = sample();
    pastor(&mut labels).visibility = 1.0;
    assert_rejected(
        &labels,
        "keyframes[0].people[0].visibility",
        "fully visible, but the box is cut at the bottom",
    );

    let mut labels = sample();
    pastor(&mut labels).truncation.left = true;
    pastor(&mut labels).visibility = 1.0;
    assert_rejected(
        &labels,
        "keyframes[0].people[0].visibility",
        "cut at the bottom and left",
    );

    // Fully hidden behind someone is a valid label.
    let mut labels = sample();
    pastor(&mut labels).visibility = 0.0;
    assert_eq!(labels.validate(), Ok(()));
}

#[test]
fn body_lines_lie_in_order_inside_the_box() {
    let mut labels = sample();
    pastor(&mut labels).lines.waist = Some(1.2);
    assert_rejected(
        &labels,
        "keyframes[0].people[0].lines.waist",
        "outside 0 to 1",
    );

    let mut labels = sample();
    pastor(&mut labels).lines.crown = Some(0.125);
    assert_rejected(
        &labels,
        "keyframes[0].people[0].lines.crown",
        "0.125 is outside the box, which spans 0.25 to 0.75",
    );

    let mut labels = sample();
    pastor(&mut labels).lines.chin = Some(0.4);
    assert_rejected(
        &labels,
        "keyframes[0].people[0].lines.shoulder",
        "0.375 is not below the chin at 0.4",
    );

    // Order skips unlabelled lines: the waist must still be below the crown.
    let mut labels = sample();
    pastor(&mut labels).lines = BodyLines {
        crown: Some(0.5),
        waist: Some(0.375),
        ..BodyLines::default()
    };
    assert_rejected(
        &labels,
        "keyframes[0].people[0].lines.waist",
        "not below the crown at 0.5",
    );

    // Lines on the box's own edges are inside it.
    let mut labels = sample();
    pastor(&mut labels).lines = BodyLines {
        crown: Some(0.25),
        waist: Some(0.75),
        ..BodyLines::default()
    };
    assert_eq!(labels.validate(), Ok(()));
}
