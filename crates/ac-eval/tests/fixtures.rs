//! Acceptance tests on the fixture files in `tests/fixtures`: the
//! synthetic label file round-trips unchanged, the validator names the
//! field of each broken rule, and a three-frame MOT ground truth imports.
//!
//! The fixtures are compiled in with `include_str!`, so the tests read no
//! files at run time.

use ac_core::Rect;
use ac_eval::labels::{CameraId, ClipId, ClipManifest, FrameRate, RoleInterval, RoleTag, TrackId};
use ac_eval::{FieldErrors, LabelFile, import_mot};
use serde_json::{Value, json};

/// A synthetic label file in canonical form: a 60-frame NTSC-rate clip
/// with two exclusion zones, three tracks, three role intervals and four
/// keyframes.
const LABELS: &str = include_str!("fixtures/synthetic.labels.json");

/// MOT ground truth for three frames of a 1920x1080 clip, with one
/// ignored entry and one static person.
const MOT_GT: &str = include_str!("fixtures/three_frames.gt.txt");

/// The fixture, parsed.
fn fixture() -> LabelFile {
    LabelFile::from_json(LABELS).expect("the fixture is valid")
}

/// The fixture as a JSON value, for editing.
fn fixture_value() -> Value {
    serde_json::from_str(LABELS).unwrap()
}

/// Asserts that `errors` has exactly one error, at `path`, with `words` in
/// its message.
#[track_caller]
fn assert_only(errors: FieldErrors, path: &str, words: &str) {
    assert_eq!(errors.paths(), [path], "{errors}");
    assert!(errors.0[0].message.contains(words), "{errors}");
}

#[test]
fn the_fixture_round_trips_unchanged() {
    let labels = fixture();
    assert_eq!(labels.to_json(), LABELS);
    assert_eq!(LabelFile::from_json(&labels.to_json()), Ok(labels.clone()));
    // Spot checks that the parse kept what the file says.
    assert_eq!(labels.clip.fps, FrameRate::new(30000, 1001));
    assert_eq!(labels.keyframes.len(), 4);
    assert_eq!(labels.keyframes[1].pts_ns, 500_500_000);
    let pastor = &labels.keyframes[0].people[0];
    assert!(pastor.truncation.bottom && !pastor.truncation.top);
    assert_eq!(pastor.lines.shoulder, Some(0.37));
    assert_eq!(labels.tracks[1].tags[1], RoleTag("worship leader".into()));
}

#[test]
fn the_validator_rejects_boxes_outside_the_unit_square() {
    let mut labels = fixture();
    labels.keyframes[2].people[1].bbox.x = -0.05;
    assert_only(
        labels.validate().unwrap_err(),
        "keyframes[2].people[1].bbox.x",
        "-0.05 is outside 0 to 1",
    );

    let mut labels = fixture();
    labels.keyframes[1].people[2].bbox = Rect::new(0.95, 0.3, 0.1, 0.6);
    assert_only(
        labels.validate().unwrap_err(),
        "keyframes[1].people[2].bbox",
        "right edge x + w",
    );

    // Read from text, the same rule names the same field.
    let mut value = fixture_value();
    value["keyframes"][0]["people"][1]["bbox"]["y"] = json!(1.25);
    assert_only(
        LabelFile::from_json(&value.to_string()).unwrap_err(),
        "keyframes[0].people[1].bbox.y",
        "1.25 is outside 0 to 1",
    );
}

#[test]
fn the_validator_rejects_overlapping_role_intervals() {
    let mut labels = fixture();
    // Track 1 is "speaker" over frames 0..30 and 45..60.
    labels.roles.push(RoleInterval {
        track: TrackId(1),
        role: RoleTag("speaker".into()),
        start_frame: 20,
        end_frame: 50,
    });
    let errors = labels.validate().unwrap_err();
    assert_eq!(errors.paths(), ["roles[3]"], "{errors}");
    assert!(
        errors.0[0].message.contains("overlaps roles[0]"),
        "{errors}"
    );
}

#[test]
fn the_validator_rejects_unknown_track_ids() {
    let mut labels = fixture();
    labels.keyframes[3].people[0].track = TrackId(42);
    labels.roles[1].track = TrackId(43);
    let errors = labels.validate().unwrap_err();
    assert_eq!(
        errors.paths(),
        ["roles[1].track", "keyframes[3].people[0].track"],
        "{errors}"
    );
    assert!(errors.0[0].message.contains("unknown track id 43"));
    assert!(errors.0[1].message.contains("unknown track id 42"));
}

#[test]
fn the_validator_rejects_a_missing_schema_version() {
    let mut value = fixture_value();
    value.as_object_mut().unwrap().remove("schema_version");
    assert_only(
        LabelFile::from_json(&value.to_string()).unwrap_err(),
        "schema_version",
        "missing",
    );
}

#[test]
fn mot_ground_truth_imports_on_a_three_frame_fixture() {
    let clip = ClipManifest {
        id: ClipId("mot-fixture".into()),
        sha256: "1".repeat(64),
        camera: CameraId("cam-left".into()),
        width: 1920,
        height: 1080,
        fps: FrameRate::new(30, 1),
        frame_count: 3,
    };
    let labels = import_mot(MOT_GT, clip).unwrap();
    // Track 3 is an ignored entry and track 4 a static person: skipped.
    let ids: Vec<u32> = labels.tracks.iter().map(|t| t.id.0).collect();
    assert_eq!(ids, [1, 2]);
    let frames: Vec<(u64, u64, Vec<u32>)> = labels
        .keyframes
        .iter()
        .map(|k| {
            let tracks = k.people.iter().map(|p| p.track.0).collect();
            (k.frame, k.pts_ns, tracks)
        })
        .collect();
    assert_eq!(
        frames,
        [
            (0, 0, vec![1, 2]),
            (1, 33_333_333, vec![1, 2]),
            (2, 66_666_666, vec![1]),
        ]
    );

    // Pixels become fractions of the picture.
    let first = &labels.keyframes[0].people[0];
    assert_eq!(first.bbox, Rect::new(0.45, 0.25, 0.1, 0.7));
    assert_eq!(first.visibility, 1.0);
    assert!(first.truncation.is_none());

    // Track 2 hangs past the right edge: cut there, with half its box (on
    // frame 1) and a third (on frame 2) left inside.
    let walker = &labels.keyframes[0].people[1];
    assert_eq!(walker.bbox, Rect::new(0.925, 0.3, 0.075, 0.65));
    assert_eq!(walker.truncation.sides(), ["right"]);
    assert_eq!(walker.visibility, 0.5);
    let walker = &labels.keyframes[1].people[1];
    assert!((walker.bbox.right() - 1.0).abs() < 1e-12);
    assert_eq!(walker.visibility, 96.0 / 288.0);

    // The labelled visibility is kept when nothing caps it.
    assert_eq!(labels.keyframes[2].people[0].visibility, 0.75);

    // The import is a valid label file and survives JSON.
    assert_eq!(LabelFile::from_json(&labels.to_json()), Ok(labels));
}
