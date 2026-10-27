//! Tests for the label schema: every validator rule kept and broken, the
//! error paths it reports and frame-rate arithmetic. [`people`] covers the
//! rules for the people on a keyframe, [`json`] reading and writing.

use super::*;

mod json;
mod people;

/// A valid file: a 30 fps clip of 90 frames with one zone, two tracks
/// (the pastor tagged), two role intervals and two keyframes. Coordinates
/// are binary fractions, so sums in messages print exactly.
fn sample() -> LabelFile {
    let lectern = PersonBox {
        track: TrackId(1),
        bbox: Rect::new(0.375, 0.25, 0.125, 0.5),
        visibility: 0.6,
        truncation: Truncation {
            bottom: true,
            ..Truncation::NONE
        },
        lines: BodyLines {
            crown: Some(0.25),
            chin: Some(0.3125),
            shoulder: Some(0.375),
            waist: Some(0.5),
        },
    };
    let guitarist = PersonBox {
        track: TrackId(2),
        bbox: Rect::new(0.125, 0.25, 0.125, 0.625),
        visibility: 1.0,
        truncation: Truncation::NONE,
        lines: BodyLines::default(),
    };
    LabelFile {
        schema_version: SCHEMA_VERSION,
        clip: ClipManifest {
            id: ClipId("stage".into()),
            sha256: "ab".repeat(32),
            camera: CameraId("cam-a".into()),
            width: 1920,
            height: 1080,
            fps: FrameRate::new(30, 1),
            frame_count: 90,
        },
        exclusions: vec![ExclusionZone {
            id: "audience".into(),
            polygon: vec![point(0.0, 0.9), point(1.0, 0.9), point(1.0, 1.0)],
        }],
        tracks: vec![
            PersonTrack {
                id: TrackId(1),
                tags: vec![tag("pastor")],
            },
            PersonTrack {
                id: TrackId(2),
                tags: vec![],
            },
        ],
        roles: vec![interval(1, "speaker", 0, 30), interval(2, "singer", 30, 90)],
        keyframes: vec![
            Keyframe {
                frame: 0,
                pts_ns: 0,
                people: vec![lectern.clone(), guitarist.clone()],
            },
            Keyframe {
                frame: 30,
                pts_ns: 1_000_000_000,
                people: vec![lectern, guitarist],
            },
        ],
    }
}

/// A table row: a change to a `T` that breaks one rule, then the path of
/// the error and words from its message.
type Case<T> = (fn(&mut T), &'static str, &'static str);

/// A normalized point.
fn point(x: f64, y: f64) -> Point {
    Point { x, y }
}

/// A role tag.
fn tag(text: &str) -> RoleTag {
    RoleTag(text.into())
}

/// Track `track` holds `role` over frames `start..end`.
fn interval(track: u32, role: &str, start: u64, end: u64) -> RoleInterval {
    RoleInterval {
        track: TrackId(track),
        role: tag(role),
        start_frame: start,
        end_frame: end,
    }
}

/// The first keyframe's first person, the pastor behind the lectern.
fn pastor(labels: &mut LabelFile) -> &mut PersonBox {
    &mut labels.keyframes[0].people[0]
}

/// The paths of every error in `labels`, empty when it is valid.
fn error_paths(labels: &LabelFile) -> Vec<String> {
    match labels.validate() {
        Ok(()) => Vec::new(),
        Err(errors) => errors.0.into_iter().map(|e| e.path).collect(),
    }
}

/// Asserts that `labels` has exactly one error, at `path`, with `words`
/// in its message.
#[track_caller]
fn assert_rejected(labels: &LabelFile, path: &str, words: &str) {
    let errors = labels.validate().expect_err("the file breaks a rule");
    assert_eq!(errors.paths(), [path], "{errors}");
    assert!(errors.0[0].message.contains(words), "{errors}");
}

#[test]
fn the_sample_is_valid() {
    assert_eq!(sample().validate(), Ok(()));
}

#[test]
fn another_schema_version_is_rejected() {
    let mut labels = sample();
    labels.schema_version = 2;
    assert_rejected(&labels, "schema_version", "version 2 is not supported");
}

#[test]
fn clip_ids_and_digest_are_checked() {
    let cases: [Case<ClipManifest>; 6] = [
        (|c| c.id.0.clear(), "clip.id", "is empty"),
        (
            |c| c.id.0 = "stage 1".into(),
            "clip.id",
            "other than letters",
        ),
        (
            |c| c.camera.0 = "cam/a".into(),
            "clip.camera",
            "other than letters",
        ),
        (
            |c| c.sha256 = "AB".repeat(32),
            "clip.sha256",
            "lowercase hex",
        ),
        (|c| c.sha256 = "ab".repeat(31), "clip.sha256", "64"),
        (|c| c.sha256 = "gh".repeat(32), "clip.sha256", "hex"),
    ];
    for (break_it, path, words) in cases {
        let mut labels = sample();
        break_it(&mut labels.clip);
        assert_rejected(&labels, path, words);
    }
    let mut labels = sample();
    labels.clip.id = ClipId("Service_2026-10-25".into());
    assert_eq!(labels.validate(), Ok(()));
}

#[test]
fn picture_size_rate_and_length_are_checked() {
    let cases: [Case<ClipManifest>; 7] = [
        (|c| c.width = 0, "clip.width", "0 is not a picture width"),
        (|c| c.height = 0, "clip.height", "0 is not a picture height"),
        (|c| c.fps.num = 0, "clip.fps.num", "not a rate"),
        (|c| c.fps.den = 0, "clip.fps.den", "not a rate"),
        (|c| c.fps = FrameRate::new(1001, 1), "clip.fps", "limit"),
        (|c| c.frame_count = 0, "clip.frame_count", "outside 1 to"),
        (
            |c| c.frame_count = MAX_FRAMES + 1,
            "clip.frame_count",
            "outside 1 to",
        ),
    ];
    for (break_it, path, words) in cases {
        let mut labels = sample();
        labels.keyframes.clear();
        labels.roles.clear();
        break_it(&mut labels.clip);
        assert_rejected(&labels, path, words);
    }
    // The limits themselves are allowed.
    let mut labels = sample();
    labels.clip.fps = FrameRate::new(2000, 2);
    labels.clip.frame_count = MAX_FRAMES;
    labels.keyframes.clear();
    assert_eq!(labels.validate(), Ok(()));
}

#[test]
fn exclusion_zones_need_an_area_inside_the_picture() {
    let mut labels = sample();
    labels.exclusions[0].polygon.pop();
    assert_rejected(&labels, "exclusions[0].polygon", "2 vertices are too few");

    let mut labels = sample();
    labels.exclusions[0].polygon[1].x = 1.5;
    assert_rejected(&labels, "exclusions[0].polygon[1].x", "outside 0 to 1");

    let mut labels = sample();
    labels.exclusions[0].polygon = vec![point(0.1, 0.1), point(0.2, 0.2), point(0.3, 0.3)];
    assert_rejected(&labels, "exclusions[0].polygon", "encloses no area");

    let mut labels = sample();
    labels.exclusions.push(labels.exclusions[0].clone());
    assert_rejected(&labels, "exclusions[1].id", "is already exclusions[0].id");

    let mut labels = sample();
    labels.exclusions[0].id = "back drop".into();
    assert_rejected(&labels, "exclusions[0].id", "other than letters");

    // No zones at all is fine.
    let mut labels = sample();
    labels.exclusions.clear();
    assert_eq!(labels.validate(), Ok(()));
}

#[test]
fn track_ids_and_tags_are_unique_and_well_formed() {
    let mut labels = sample();
    labels.tracks[1].id = TrackId(1);
    // Every reference to track 2 is now unknown too.
    let paths = error_paths(&labels);
    assert_eq!(paths[0], "tracks[1].id");
    assert!(paths.contains(&"keyframes[0].people[1].track".to_owned()));

    for bad in ["Pastor", "worship  leader", "-lead", "lead-", "", "café"] {
        let mut labels = sample();
        labels.tracks[0].tags = vec![tag(bad)];
        assert_rejected(&labels, "tracks[0].tags[0]", "is not a role tag");
    }

    let mut labels = sample();
    labels.tracks[0].tags.push(tag("pastor"));
    assert_rejected(&labels, "tracks[0].tags[1]", "is already tags[0]");

    let mut labels = sample();
    labels.tracks[1].tags = vec![tag("worship leader"), tag("bass-player"), tag("drummer_2")];
    assert_eq!(labels.validate(), Ok(()));
}

#[test]
fn role_intervals_name_declared_tracks() {
    let mut labels = sample();
    labels.roles[1].track = TrackId(7);
    assert_rejected(&labels, "roles[1].track", "unknown track id 7");
}

#[test]
fn overlapping_intervals_of_one_track_and_role_are_rejected() {
    let mut labels = sample();
    labels.roles.push(interval(1, "speaker", 29, 40));
    assert_rejected(&labels, "roles[2]", "overlaps roles[0]");

    // An interval inside another overlaps it too, reported once.
    let mut labels = sample();
    labels.roles.push(interval(1, "speaker", 10, 20));
    labels.roles.push(interval(1, "speaker", 5, 25));
    assert_eq!(error_paths(&labels), ["roles[2]", "roles[3]"]);
}

#[test]
fn touching_or_different_intervals_do_not_overlap() {
    let mut labels = sample();
    // Starts where roles[0] ends.
    labels.roles.push(interval(1, "speaker", 30, 60));
    // Another role of the same track, at the same time.
    labels.roles.push(interval(1, "singer", 0, 30));
    // Track 2's role for track 1 as well: two people can sing at once.
    labels.roles.push(interval(1, "singer", 30, 90));
    assert_eq!(labels.validate(), Ok(()));
}

#[test]
fn interval_spans_lie_inside_the_clip() {
    let mut labels = sample();
    labels.roles[0].end_frame = 0;
    assert_rejected(&labels, "roles[0].end_frame", "not after start_frame 0");

    let mut labels = sample();
    labels.roles[0].start_frame = 30;
    assert_rejected(&labels, "roles[0].end_frame", "not after start_frame 30");

    let mut labels = sample();
    labels.roles[1].end_frame = 91;
    assert_rejected(&labels, "roles[1].end_frame", "past the clip's 90 frames");
}

#[test]
fn a_role_is_not_given_both_as_tag_and_interval() {
    let mut labels = sample();
    labels.roles.push(interval(1, "pastor", 0, 10));
    assert_rejected(
        &labels,
        "roles[2].role",
        "already has \"pastor\" among its tags",
    );

    let mut labels = sample();
    labels.roles[0].role = tag("Speaker");
    assert_rejected(&labels, "roles[0].role", "is not a role tag");
}

#[test]
fn keyframes_go_in_frame_order_inside_the_clip() {
    let mut labels = sample();
    labels.keyframes[1].frame = 0;
    labels.keyframes[1].pts_ns = 0;
    assert_rejected(
        &labels,
        "keyframes[1].frame",
        "not after the previous keyframe's 0",
    );

    let mut labels = sample();
    labels.keyframes.swap(0, 1);
    assert_rejected(
        &labels,
        "keyframes[1].frame",
        "not after the previous keyframe's 30",
    );

    let mut labels = sample();
    labels.keyframes[1].frame = 90;
    labels.keyframes[1].pts_ns = 3_000_000_000;
    assert_rejected(
        &labels,
        "keyframes[1].frame",
        "not inside the clip's 90 frames",
    );
}

#[test]
fn a_keyframes_pts_names_its_frame() {
    // Frames counted from 1: frame 30 labelled at frame 31's time.
    let mut labels = sample();
    labels.keyframes[1].pts_ns = 1_033_333_333;
    assert_rejected(
        &labels,
        "keyframes[1].pts_ns",
        "nearest frame 31 at 30 fps, not frame 30",
    );

    // Milliseconds written where nanoseconds belong.
    let mut labels = sample();
    labels.keyframes[1].pts_ns = 1_000;
    assert_rejected(&labels, "keyframes[1].pts_ns", "nearest frame 0");

    // Up to just under half a frame either way names the same frame.
    let mut labels = sample();
    labels.keyframes[1].pts_ns = 1_016_666_666;
    assert_eq!(labels.validate(), Ok(()));
    labels.keyframes[1].pts_ns = 983_333_334;
    assert_eq!(labels.validate(), Ok(()));

    // Without a usable rate only the rate is reported.
    let mut labels = sample();
    labels.clip.fps.den = 0;
    assert_rejected(&labels, "clip.fps.den", "not a rate");
}

#[test]
fn every_error_is_reported_in_document_order() {
    let mut labels = sample();
    labels.clip.width = 0;
    labels.roles[0].track = TrackId(5);
    pastor(&mut labels).bbox.x = -1.0;
    labels.keyframes[1].people[1].track = TrackId(6);
    let errors = labels.validate().unwrap_err();
    assert_eq!(
        errors.paths(),
        [
            "clip.width",
            "roles[0].track",
            "keyframes[0].people[0].bbox.x",
            "keyframes[1].people[1].track",
        ]
    );
    // One line each, path first.
    let text = errors.to_string();
    assert_eq!(text.lines().count(), 4);
    assert!(
        text.starts_with("clip.width: 0 is not a picture width\n"),
        "{text}"
    );
    assert_eq!(
        errors.at("roles[0].track").map(|e| e.message.as_str()),
        Some("unknown track id 5: no entry in tracks has it")
    );
    assert_eq!(errors.at("tracks"), None);
}

#[test]
fn frame_times_are_exact_at_ntsc_rates() {
    let ntsc = FrameRate::new(30000, 1001);
    assert_eq!(ntsc.pts_ns(0), Some(0));
    assert_eq!(ntsc.pts_ns(30), Some(1_001_000_000));
    assert_eq!(ntsc.pts_ns(15), Some(500_500_000));
    for frame in [0, 1, 2, 29, 30, 31, 1_000_000, MAX_FRAMES] {
        let pts = ntsc.pts_ns(frame).unwrap();
        assert_eq!(ntsc.nearest_frame(pts), Some(frame), "frame {frame}");
    }
    assert_eq!(ntsc.to_string(), "30000/1001");
    assert_eq!(FrameRate::new(25, 1).to_string(), "25");
}

#[test]
fn nearest_frame_rounds_half_up_and_rates_need_both_parts() {
    let rate = FrameRate::new(10, 1);
    assert_eq!(rate.nearest_frame(49_999_999), Some(0));
    assert_eq!(rate.nearest_frame(50_000_000), Some(1));
    for broken in [FrameRate::new(0, 1), FrameRate::new(30, 0)] {
        assert!(!broken.is_valid());
        assert_eq!(broken.pts_ns(5), None);
        assert_eq!(broken.nearest_frame(5), None);
    }
    assert!(FrameRate::new(1000, 1).is_valid());
    assert!(!FrameRate::new(2001, 2).is_valid());
    // Far past any clip, times saturate instead of wrapping.
    assert_eq!(FrameRate::new(1, 1000).pts_ns(u64::MAX), Some(u64::MAX));
}

#[test]
fn truncation_and_lines_describe_themselves() {
    assert!(Truncation::NONE.is_none());
    assert!(Truncation::NONE.sides().is_empty());
    let cut = Truncation {
        top: true,
        right: true,
        ..Truncation::NONE
    };
    assert!(!cut.is_none());
    assert_eq!(cut.sides(), ["top", "right"]);
    assert!(BodyLines::default().is_empty());
    let lines = BodyLines {
        chin: Some(0.4),
        ..BodyLines::default()
    };
    assert!(!lines.is_empty());
    assert_eq!(lines.named()[1], ("chin", Some(0.4)));
}
