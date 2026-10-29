//! Tests for the MOT importer: rows to keyframes and tracks, skipped rows,
//! boxes past the picture's edges, and every row error with its line.

use super::*;
use crate::labels::{BoxExtent, CameraId, ClipId, FrameRate};

/// The manifest of a 1000x500 clip at 10 fps with `frames` frames, so
/// pixel boxes divide into short decimals.
fn clip(frames: u64) -> ClipManifest {
    ClipManifest {
        id: ClipId("mot".into()),
        sha256: "0".repeat(64),
        camera: CameraId("cam".into()),
        width: 1000,
        height: 500,
        fps: FrameRate::new(10, 1),
        frame_count: frames,
    }
}

/// Imports `text` for a 5-frame clip.
fn import(text: &str) -> Result<LabelFile, MotError> {
    import_mot(text, clip(5))
}

/// Asserts that importing `text` fails at `line` with `words` in the
/// message.
#[track_caller]
fn assert_row_error(text: &str, line: usize, words: &str) {
    match import(text) {
        Err(MotError::Row { line: at, message }) => {
            assert_eq!(at, line, "{message}");
            assert!(message.contains(words), "{message}");
        }
        other => panic!("expected a row error, got {other:?}"),
    }
}

#[test]
fn rows_become_people_on_keyframes() {
    let labels = import("1,7,100,50,200,400,1,1,0.75\n3,2,0,0,100,100\n").unwrap();
    assert_eq!(labels.schema_version, SCHEMA_VERSION);
    assert_eq!(labels.clip, clip(5));
    assert_eq!(labels.box_extent, BoxExtent::Full);
    let ids: Vec<u32> = labels.tracks.iter().map(|t| t.id.0).collect();
    assert_eq!(ids, [2, 7]);
    assert!(labels.tracks.iter().all(|t| t.tags.is_empty()));
    // Every frame of the clip is a keyframe, at its time; frames no row
    // names are empty.
    let frames: Vec<(u64, u64, usize)> = labels
        .keyframes
        .iter()
        .map(|k| (k.frame, k.pts_ns, k.people.len()))
        .collect();
    assert_eq!(
        frames,
        [
            (0, 0, 1),
            (1, 100_000_000, 0),
            (2, 200_000_000, 1),
            (3, 300_000_000, 0),
            (4, 400_000_000, 0),
        ]
    );
    let person = &labels.keyframes[0].people[0];
    assert_eq!(person.track, TrackId(7));
    assert_eq!(person.bbox, Rect::new(0.1, 0.1, 0.2, 0.8));
    assert_eq!(person.visibility, 0.75);
    assert!(person.truncation.is_none());
    assert!(person.lines.is_empty());
    // Six columns: no flag, class or visibility, so fully visible.
    assert_eq!(labels.keyframes[2].people[0].visibility, 1.0);
    assert!(labels.roles.is_empty() && labels.exclusions.is_empty());
}

#[test]
fn people_on_a_keyframe_are_in_track_order() {
    let labels = import("1,9,0,0,10,10\n1,3,20,0,10,10\n1,5,40,0,10,10\n").unwrap();
    let order: Vec<u32> = labels.keyframes[0]
        .people
        .iter()
        .map(|p| p.track.0)
        .collect();
    assert_eq!(order, [3, 5, 9]);
}

#[test]
fn every_person_class_is_imported_whatever_its_flag() {
    let text = "\
# frame, id, left, top, width, height, consider, class, visibility

1,1,0,0,100,100,0,1,0.4
1,2,100,0,100,100,0,2,1
1,3,200,0,100,100,0,7,1
1,4,300,0,100,100,1,7,1
1,5,400,0,100,100,1,1,1
1,6,0,0,1000,500,0,3,1
1,8,500,0,100,100,0,8,1
1,9,0,200,1000,300,0,9,1
1,12,600,0,100,100,0,12,1
1,13,700,0,100,100,1
";
    let labels = import(text).unwrap();
    // Pedestrians, people on vehicles and static people are people, even
    // where MOT's own evaluation ignores them; vehicles, occluders,
    // distractors and reflections are not. A row without a class is a
    // pedestrian.
    let ids: Vec<u32> = labels.tracks.iter().map(|t| t.id.0).collect();
    assert_eq!(ids, [1, 2, 3, 4, 5, 13]);
    let people = &labels.keyframes[0].people;
    assert_eq!(people.len(), 6);
    assert_eq!(people[0].visibility, 0.4);
}

#[test]
fn classes_are_whole_numbers() {
    assert_row_error(
        "1,1,0,0,10,10,1,4.25,1",
        1,
        "column 8 (class) is 4.25, not a whole number",
    );
    assert_row_error("1,1,0,0,10,10,1,0,1", 1, "class 0 is not a MOT class");
}

#[test]
fn minus_one_marks_absent_columns() {
    let labels = import("2, 4, 10, 20, 30, 40, 1, -1, -1").unwrap();
    let person = &labels.keyframes[1].people[0];
    assert_eq!(person.track, TrackId(4));
    assert_eq!(person.visibility, 1.0);
}

#[test]
fn ten_columns_are_mot15_with_world_coordinates() {
    // MOT15: the consider flag, then world coordinates, unused or filled
    // in as in the 3D MOT 2015 sequences. There is no class or visibility.
    let text = "\
1,1,100,50,200,400,1,-1,-1,-1
2,1,110,50,200,400,1,4.3,3.5,0
3,1,120,50,200,400,0,7,0.25,1
";
    let labels = import(text).unwrap();
    let people: Vec<(u32, f64)> = labels
        .keyframes
        .iter()
        .flat_map(|k| k.people.iter().map(|p| (p.track.0, p.visibility)))
        .collect();
    assert_eq!(people, [(1, 1.0), (1, 1.0), (1, 1.0)]);
    assert_eq!(labels.keyframes[1].people[0].bbox.x, 0.11);
}

#[test]
fn fractional_pixel_boxes_survive_json_unchanged() {
    // Sub-pixel boxes on a 1920x1080 picture need every digit of their
    // normalized coordinates.
    let mut manifest = clip(400);
    (manifest.width, manifest.height) = (1920, 1080);
    let text: String = (1..=400)
        .map(|frame| {
            let x = 3.0 + f64::from(frame) * 1.37;
            let h = 100.0 + f64::from(frame) * 0.29;
            format!(
                "{frame},1,{x},17.5,{},{h},1,1,1\n",
                51.0 + f64::from(frame) / 7.0
            )
        })
        .collect();
    let labels = import_mot(&text, manifest).unwrap();
    let json = labels.to_json();
    let back = LabelFile::from_json(&json).unwrap();
    assert_eq!(back, labels);
    assert_eq!(back.to_json(), json);
}

#[test]
fn occluded_people_keep_their_whole_outline() {
    // MOT draws a person 70% hidden behind something at full size, and the
    // file says so instead of marking a hidden side.
    let labels = import("1,1,100,50,200,400,1,1,0.3").unwrap();
    assert_eq!(labels.box_extent, BoxExtent::Full);
    let person = &labels.keyframes[0].people[0];
    assert_eq!(person.bbox, Rect::new(0.1, 0.1, 0.2, 0.8));
    assert_eq!(person.visibility, 0.3);
    assert!(person.truncation.is_none());
}

#[test]
fn boxes_past_the_edges_are_cut_and_marked_truncated() {
    // 100 pixels of a 200-pixel-wide box hang past the right edge.
    let labels = import("1,1,900,100,200,200,1,1,0.9").unwrap();
    let person = &labels.keyframes[0].people[0];
    assert_eq!(person.bbox, Rect::new(0.9, 0.2, 0.1, 0.4));
    assert_eq!(person.truncation.sides(), ["right"]);
    assert_eq!(person.visibility, 0.5);

    // Past the top and left: a quarter of the box is inside. A lower
    // labelled visibility is kept.
    let labels = import("1,1,-50,-100,100,200,1,1,0.2").unwrap();
    let person = &labels.keyframes[0].people[0];
    assert_eq!(person.bbox, Rect::new(0.0, 0.0, 0.05, 0.2));
    assert_eq!(person.truncation.sides(), ["top", "left"]);
    assert_eq!(person.visibility, 0.2);

    // Past the bottom with no visibility given.
    let labels = import("1,1,0,400,100,200").unwrap();
    let person = &labels.keyframes[0].people[0];
    assert_eq!(person.truncation.sides(), ["bottom"]);
    assert_eq!(person.visibility, 0.5);

    // Touching an edge is not being cut by it.
    let labels = import("1,1,0,0,1000,500,1,1,1").unwrap();
    let person = &labels.keyframes[0].people[0];
    assert_eq!(person.bbox, Rect::FULL);
    assert!(person.truncation.is_none());
    assert_eq!(person.visibility, 1.0);
}

#[test]
fn rows_with_unreadable_columns_are_rejected_by_line() {
    assert_row_error("1,1,0,0,10", 1, "5 columns; a MOT row has 6 to 10");
    assert_row_error("1,1,0,0,10,10,1,1,1,1,1", 1, "11 columns");
    assert_row_error(
        "1,1,abc,0,10,10",
        1,
        "column 3 (left) is \"abc\", not a number",
    );
    assert_row_error("1,1,0,0,NaN,10", 1, "column 5 (width) is \"NaN\"");
    assert_row_error(
        "1.5,1,0,0,10,10",
        1,
        "column 1 (frame) is 1.5, not a whole number",
    );
    assert_row_error("1,2.5,0,0,10,10", 1, "column 2 (id) is 2.5");
    assert_row_error("1,1,0,0,10,10,1,x", 1, "column 8 (class)");
    // Blank and comment lines count towards line numbers.
    assert_row_error("\n# header\n1,1,0,0,10,10\n2,1,0,0,10", 4, "5 columns");
}

#[test]
fn frames_must_lie_inside_the_clip_counted_from_one() {
    assert_row_error("0,1,0,0,10,10", 1, "frame 0: MOT counts frames from 1");
    assert_row_error("6,1,0,0,10,10", 1, "frame 6 is past the clip's 5 frames");
    assert!(import("5,1,0,0,10,10").is_ok());
}

#[test]
fn rows_need_a_track_id() {
    assert_row_error("1,-1,0,0,10,10", 1, "detection without identity");
    assert_row_error("1,-4,0,0,10,10", 1, "track id -4 is not in 0 to");
    assert_row_error("1,4294967296,0,0,10,10", 1, "track id 4294967296");
    assert!(import("1,0,0,0,10,10").is_ok());
}

#[test]
fn result_files_with_confidences_are_not_ground_truth() {
    assert_row_error(
        "1,1,0,0,10,10,0.87,-1,-1,-1",
        1,
        "consider flag, 0 or 1, but holds 0.87",
    );
}

#[test]
fn boxes_need_a_size_and_a_part_inside_the_picture() {
    assert_row_error("1,1,0,0,0,10", 1, "0 by 10 pixels");
    assert_row_error("1,1,0,0,10,-5", 1, "10 by -5 pixels");
    assert_row_error(
        "1,1,1000,0,10,10",
        1,
        "entirely outside the 1000x500 picture",
    );
    assert_row_error("1,1,-20,0,10,10", 1, "entirely outside");
    assert_row_error(
        "1,1,0,0,10,10,1,1,1.5",
        1,
        "visibility 1.5 is outside 0 to 1",
    );
}

#[test]
fn one_track_has_one_box_per_frame() {
    assert_row_error(
        "1,1,0,0,10,10\n2,1,0,0,10,10\n1,1,5,5,10,10",
        3,
        "track 1 already has a box on MOT frame 1",
    );
}

#[test]
fn an_invalid_manifest_is_reported_by_field() {
    let mut manifest = clip(5);
    manifest.width = 0;
    match import_mot("1,1,0,0,10,10", manifest) {
        Err(MotError::Invalid(errors)) => assert_eq!(errors.paths(), ["clip.width"]),
        other => panic!("expected an invalid manifest, got {other:?}"),
    }
    let mut manifest = clip(5);
    manifest.frame_count = MAX_FRAMES_PLUS_ONE;
    assert!(matches!(
        import_mot("", manifest),
        Err(MotError::Invalid(_))
    ));
}

/// A frame count past the limit, which must fail before any keyframe is
/// built.
const MAX_FRAMES_PLUS_ONE: u64 = crate::labels::MAX_FRAMES + 1;

#[test]
fn empty_text_gives_empty_keyframes() {
    let labels = import("").unwrap();
    assert!(labels.tracks.is_empty());
    assert_eq!(labels.keyframes.len(), 5);
    assert!(labels.keyframes.iter().all(|k| k.people.is_empty()));
}

#[test]
fn errors_read_as_text() {
    let error = import("0,1,0,0,10,10").unwrap_err();
    assert_eq!(
        error.to_string(),
        "line 1: frame 0: MOT counts frames from 1"
    );
}
