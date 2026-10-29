//! Parser and resolver tests on canned avfoundation device lists and
//! system_profiler JSON (`tests/fixtures/capture`).

use super::*;

/// ffmpeg's list with a built-in camera, a webcam and a capture dongle.
const FFMPEG_THREE: &str = include_str!("../../../tests/fixtures/capture/ffmpeg-three-cameras.txt");
/// system_profiler's report of the same three cameras.
const PROFILER_THREE: &str =
    include_str!("../../../tests/fixtures/capture/system-profiler-three-cameras.json");
/// ffmpeg's list after the webcam was unplugged and plugged back in, now
/// after the dongle.
const FFMPEG_REPLUGGED: &str =
    include_str!("../../../tests/fixtures/capture/ffmpeg-brio-replugged.txt");
/// system_profiler's report after the same replug.
const PROFILER_REPLUGGED: &str =
    include_str!("../../../tests/fixtures/capture/system-profiler-brio-replugged.json");
/// ffmpeg's list with the webcam unplugged.
const FFMPEG_UNPLUGGED: &str =
    include_str!("../../../tests/fixtures/capture/ffmpeg-brio-unplugged.txt");
/// system_profiler's report with the webcam unplugged.
const PROFILER_UNPLUGGED: &str =
    include_str!("../../../tests/fixtures/capture/system-profiler-brio-unplugged.json");
/// ffmpeg's list with two webcams of one model.
const FFMPEG_TWINS: &str = include_str!("../../../tests/fixtures/capture/ffmpeg-twin-webcams.txt");
/// system_profiler's report of the same two webcams.
const PROFILER_TWINS: &str =
    include_str!("../../../tests/fixtures/capture/system-profiler-twin-webcams.json");
/// What an ffmpeg without avfoundation (Linux) says when asked.
const FFMPEG_NONE: &str =
    include_str!("../../../tests/fixtures/capture/ffmpeg-no-avfoundation.txt");
/// system_profiler's report with no camera connected.
const PROFILER_NONE: &str =
    include_str!("../../../tests/fixtures/capture/system-profiler-no-cameras.json");

/// The webcam's unique ID in every fixture.
const BRIO: &str = "0x14200000046d085e";

/// A device for comparisons.
fn device(index: u32, name: &str) -> AvDevice {
    AvDevice {
        index,
        name: name.into(),
    }
}

/// `uid` resolved against a pair of fixtures.
fn resolve_in(uid: &str, profiler: &str, ffmpeg: &str) -> Result<ResolvedDevice, MediaError> {
    resolve(
        uid,
        &parse_camera_report(profiler).unwrap(),
        &parse_device_list(ffmpeg).unwrap(),
    )
}

/// The message of an open error.
fn message(e: MediaError) -> String {
    match e {
        MediaError::Open(m) => m,
        other => panic!("not an open error: {other}"),
    }
}

#[test]
fn the_device_list_is_the_video_section_in_index_order() {
    let devices = parse_device_list(FFMPEG_THREE).unwrap();
    assert_eq!(
        devices,
        [
            device(0, "FaceTime HD Camera"),
            device(1, "Logitech BRIO"),
            device(2, "Cam Link 4K"),
            device(3, "Capture screen 0"),
        ],
        "the audio devices are not video devices"
    );
}

#[test]
fn the_device_list_tolerates_other_prefixes_and_noise() {
    // No log context (an ffmpeg that prints none), CRLF line ends, stray
    // lines, a duplicate index and an unsorted list.
    let text = "AVFoundation video devices:\r\n[2] Cam Link 4K\r\n\
                some warning\r\n[0] FaceTime HD Camera [front]\r\n[0] again\r\n\
                [x] not a device\r\n[1] \r\nAVFoundation audio devices:\r\n[5] Mic\r\n";
    assert_eq!(
        parse_device_list(text).unwrap(),
        [
            device(0, "FaceTime HD Camera [front]"),
            device(2, "Cam Link 4K")
        ]
    );
    // A list with no video device is empty, not an error.
    let empty = "[AVFoundation indev @ 0x1] AVFoundation video devices:\n\
                 [AVFoundation indev @ 0x1] AVFoundation audio devices:\n";
    assert_eq!(parse_device_list(empty).unwrap(), []);
}

#[test]
fn an_ffmpeg_without_avfoundation_is_an_error_saying_why() {
    let e = message(parse_device_list(FFMPEG_NONE).unwrap_err());
    assert!(e.contains("needs macOS"), "{e}");
    assert!(e.contains("Unknown input format: 'avfoundation'"), "{e}");
    assert!(!e.contains("0x5579ee319ec0"), "no log context: {e}");
    let e = message(parse_device_list("").unwrap_err());
    assert!(e.contains("printed nothing"), "{e}");
}

#[test]
fn the_camera_report_keeps_names_ids_and_order() {
    let cameras = parse_camera_report(PROFILER_THREE).unwrap();
    let names: Vec<&str> = cameras.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["FaceTime HD Camera", "Logitech BRIO", "Cam Link 4K"]
    );
    assert_eq!(cameras[1].unique_id.as_deref(), Some(BRIO));
    assert_eq!(
        cameras[1].model_id.as_deref(),
        Some("UVC Camera VendorID_1133 ProductID_2142")
    );
    assert_eq!(parse_camera_report(PROFILER_NONE).unwrap(), []);
    assert_eq!(parse_camera_report("{}").unwrap(), [], "no key, no cameras");
}

#[test]
fn the_camera_report_skips_nameless_entries_and_blank_ids() {
    let json = r#"{"SPCameraDataType": [
        {"spcamera_unique-id": "0x1"},
        {"_name": "  Desk Cam  ", "spcamera_unique-id": " "},
        {"_name": "Other", "extra": [1, 2]}
    ]}"#;
    let cameras = parse_camera_report(json).unwrap();
    assert_eq!(
        cameras,
        [
            SystemCamera {
                name: "Desk Cam".into(),
                unique_id: None,
                model_id: None,
            },
            SystemCamera {
                name: "Other".into(),
                unique_id: None,
                model_id: None,
            },
        ]
    );
}

#[test]
fn an_unreadable_camera_report_is_an_error() {
    for bad in ["", "not json", r#"{"SPCameraDataType": {"_name": "x"}}"#] {
        let e = message(parse_camera_report(bad).unwrap_err());
        assert!(
            e.contains("unreadable system_profiler output"),
            "{bad}: {e}"
        );
    }
}

#[test]
fn a_unique_id_resolves_to_the_device_of_its_name() {
    let brio = resolve_in(BRIO, PROFILER_THREE, FFMPEG_THREE).unwrap();
    assert_eq!(
        brio,
        ResolvedDevice {
            unique_id: BRIO.into(),
            name: "Logitech BRIO".into(),
            index: 1,
            same_name: 1,
        }
    );
    assert_eq!(
        brio.to_string(),
        "\"Logitech BRIO\" (unique ID 0x14200000046d085e, ffmpeg device 1)"
    );
    // Case and surrounding space in a typed ID do not matter.
    let typed = resolve_in(" 0X14200000046D085E ", PROFILER_THREE, FFMPEG_THREE).unwrap();
    assert_eq!(typed, brio);
    let dongle = resolve_in("0x111000000fd90066", PROFILER_THREE, FFMPEG_THREE).unwrap();
    assert_eq!((dongle.index, dongle.name.as_str()), (2, "Cam Link 4K"));
}

#[test]
fn two_cameras_with_the_same_name_resolve_by_unique_id() {
    let first = resolve_in("0x141000000c456366", PROFILER_TWINS, FFMPEG_TWINS).unwrap();
    let second = resolve_in("0x142000000c456366", PROFILER_TWINS, FFMPEG_TWINS).unwrap();
    assert_eq!((first.index, first.same_name), (1, 2));
    assert_eq!((second.index, second.same_name), (2, 2));
    assert_eq!(first.name, second.name);
    // Swapped in system_profiler's order, they swap in ffmpeg's too (the
    // order assumption; see the module docs).
    let mut cameras = parse_camera_report(PROFILER_TWINS).unwrap();
    cameras.swap(1, 2);
    let devices = parse_device_list(FFMPEG_TWINS).unwrap();
    assert_eq!(
        resolve("0x141000000c456366", &cameras, &devices)
            .unwrap()
            .index,
        2
    );
}

#[test]
fn a_replug_that_reorders_indices_resolves_the_same_unique_id_anew() {
    let before = resolve_in(BRIO, PROFILER_THREE, FFMPEG_THREE).unwrap();
    let after = resolve_in(BRIO, PROFILER_REPLUGGED, FFMPEG_REPLUGGED).unwrap();
    assert_eq!((before.index, after.index), (1, 2));
    assert_eq!(before.name, after.name);
    // The device that took the old index is not the camera.
    let other = parse_device_list(FFMPEG_REPLUGGED).unwrap();
    assert_eq!(other[1].name, "Cam Link 4K");
}

#[test]
fn a_missing_unique_id_is_an_error_with_no_fallback() {
    // Unplugged: the old index now names another device, and nothing else
    // is taken in its place.
    let e = message(resolve_in(BRIO, PROFILER_UNPLUGGED, FFMPEG_UNPLUGGED).unwrap_err());
    assert!(
        e.contains(r#"no camera with unique ID "0x14200000046d085e" is connected"#),
        "{e}"
    );
    assert!(
        e.contains(r#""FaceTime HD Camera" 47B4B64B-7067-4B9C-AD2B-AE273A71F4B5"#),
        "lists what is connected: {e}"
    );
    // A camera of the same model on another port is another camera.
    let e = message(resolve_in("0x143000000c456366", PROFILER_TWINS, FFMPEG_TWINS).unwrap_err());
    assert!(e.contains("is connected (connected: "), "{e}");
    let e = message(resolve_in(BRIO, PROFILER_NONE, FFMPEG_UNPLUGGED).unwrap_err());
    assert!(e.contains("no camera is connected"), "{e}");
}

#[test]
fn a_camera_ffmpeg_does_not_list_is_an_error() {
    let cameras = parse_camera_report(PROFILER_THREE).unwrap();
    let devices = parse_device_list(FFMPEG_UNPLUGGED).unwrap();
    let e = message(resolve(BRIO, &cameras, &devices).unwrap_err());
    assert!(e.contains("does not list it"), "{e}");
    assert!(
        e.contains(r#"(connected: "FaceTime HD Camera" 47B4B64B-7067-4B9C-AD2B-AE273A71F4B5, "Logitech BRIO" 0x14200000046d085e, "#),
        "lists what is connected: {e}"
    );
}

#[test]
fn twins_that_the_lists_count_differently_cannot_be_told_apart() {
    // A third device of the same name that only ffmpeg lists (a virtual
    // camera, say) makes the order meaningless.
    let cameras = parse_camera_report(PROFILER_TWINS).unwrap();
    let mut devices = parse_device_list(FFMPEG_TWINS).unwrap();
    devices.push(device(9, "USB Camera"));
    for uid in ["0x141000000c456366", "0x142000000c456366"] {
        let e = message(resolve(uid, &cameras, &devices).unwrap_err());
        assert!(
            e.contains("2 camera(s) named \"USB Camera\" and ffmpeg 3"),
            "{e}"
        );
        assert!(e.contains("cannot be told apart"), "{e}");
        assert!(
            e.contains(r#""USB Camera" 0x141000000c456366, "USB Camera" 0x142000000c456366"#),
            "lists the twins' IDs: {e}"
        );
    }
    // One twin unplugged: the other still resolves, as a single camera.
    let one = &cameras[..2];
    let devices = parse_device_list(FFMPEG_TWINS).unwrap();
    let e = message(resolve("0x141000000c456366", one, &devices).unwrap_err());
    assert!(e.contains("1 camera(s)"), "{e}");
    let remaining = [device(0, "FaceTime HD Camera"), device(1, "USB Camera")];
    let got = resolve("0x141000000c456366", one, &remaining).unwrap();
    assert_eq!((got.index, got.same_name), (1, 1));
}

#[test]
fn a_camera_without_a_unique_id_cannot_be_asked_for() {
    let cameras = [SystemCamera {
        name: "Desk Cam".into(),
        unique_id: None,
        model_id: None,
    }];
    let e = message(resolve("", &cameras, &[device(0, "Desk Cam")]).unwrap_err());
    assert!(e.contains(r#""Desk Cam" without a unique ID"#), "{e}");
}
