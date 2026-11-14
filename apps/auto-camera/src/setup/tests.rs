//! Tests of the source picker's studio file edits.

use super::*;
use crate::testing::temp_path;

/// A studio with two cameras, an audio input and comments to keep.
const TWO_CAMERAS: &str = r#"# My church.
schema = 1

# The stage camera.
[[sources]]
id = "stage"
name = "Stage"
kind = "capture"
unique_id = "0xAA"
width = 1920 # tuned
height = 1080
fps = 30
pixel_format = "nv12"

[[sources]]
id = "cam-1"
name = "Side"
kind = "capture"
unique_id = "0xBB"
width = 1280
height = 720
fps = 30

[[audio_inputs]]
id = "board"
kind = "file"
path = "board.wav"

# Keep this query.
[[queries]]
name = "Speaker"
prompt = "a person"

[detector]
kind = "none"

[output]
audio_source = "board"
"#;

/// A camera named `name` with unique ID `id`.
fn camera(name: &str, id: &str) -> Camera {
    Camera {
        name: name.into(),
        unique_id: id.into(),
    }
}

/// [`studio_text`] parsed as the engine would (paths resolved in /tmp).
fn config_of(text: &str) -> StudioConfig {
    check(text, Path::new("/tmp")).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

#[test]
fn a_video_file_replaces_the_sources_and_keeps_every_other_line() {
    let choice = Choice::File("/Users/me/Movies/Sermon 1.mp4".into());
    let text = studio_text(TWO_CAMERAS, &choice, false, Path::new("/s")).unwrap();
    assert!(text.starts_with("# My church.\nschema = 1\n"), "{text}");
    assert!(text.contains("# Keep this query.\n[[queries]]"), "{text}");
    assert!(!text.contains("0xAA"), "{text}");
    // The sources sit where the old ones were, before the audio input.
    assert!(text.find("[[sources]]") < text.find("[[audio_inputs]]"));
    let config = config_of(&text);
    assert_eq!(config.sources.len(), 1);
    let source = &config.sources[0];
    assert_eq!(source.id, "cam-1");
    assert_eq!(source.display_name(), "Sermon 1");
    match &source.input {
        ac_sources::SourceConfig::File { path, .. } => {
            assert_eq!(path, Path::new("/Users/me/Movies/Sermon 1.mp4"));
        }
        other => panic!("{other:?}"),
    }
    // Program audio from the board stays the board's.
    assert_eq!(config.output.audio_source.as_deref(), Some("board"));
    assert_eq!(config.queries.len(), 1);
}

#[test]
fn cameras_that_stay_keep_their_tables_and_new_ones_get_free_ids() {
    let choice = Choice::Cameras {
        cameras: vec![camera("New cam", "0xcc"), camera("Stage", "0xaa")],
        mode: MODES[0],
    };
    let text = studio_text(TWO_CAMERAS, &choice, false, Path::new("/s")).unwrap();
    let config = config_of(&text);
    let ids: Vec<&str> = config.sources.iter().map(|s| s.id.as_str()).collect();
    // "cam-1" was the dropped side camera's: free again, so taken.
    assert_eq!(ids, ["cam-1", "stage"]);
    assert_eq!(config.sources[0].display_name(), "New cam");
    // The kept table keeps its comments, name and pixel format; its mode
    // follows the picker, its trailing comment stays.
    assert!(text.contains("# The stage camera.\n[[sources]]\nid = \"stage\""));
    assert!(text.contains("width = 1280 # tuned"), "{text}");
    assert!(text.contains("pixel_format = \"nv12\""));
    assert!(!text.contains("0xBB"));
    match &config.sources[1].input {
        ac_sources::SourceConfig::Capture {
            width, height, fps, ..
        } => assert_eq!((*width, *height, *fps), (1280, 720, 30.0)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn audio_from_a_source_that_is_gone_follows_the_file_or_goes_for_cameras() {
    let studio = r#"
[[sources]]
id = "old"
kind = "file"
path = "a.mp4"

[output]
audio_source = "old" # the video's own track
"#;
    let file = Choice::File("/m/b.mp4".into());
    let text = studio_text(studio, &file, false, Path::new("/s")).unwrap();
    assert!(text.contains("audio_source = \"cam-1\" # the video's own track"));
    assert_eq!(
        config_of(&text).output.audio_source.as_deref(),
        Some("cam-1")
    );
    let cameras = Choice::Cameras {
        cameras: vec![camera("Cam", "0x1")],
        mode: MODES[1],
    };
    let text = studio_text(studio, &cameras, false, Path::new("/s")).unwrap();
    assert!(!text.contains("audio_source"), "{text}");
    assert_eq!(config_of(&text).output.audio_source, None);
    // The same file again, as written or resolved, keeps its source.
    for again in ["a.mp4", "/s/a.mp4"] {
        let same = studio_text(studio, &Choice::File(again.into()), false, Path::new("/s"));
        let same = same.unwrap();
        assert!(same.contains("id = \"old\""), "{same}");
        assert!(same.contains("audio_source = \"old\""), "{same}");
    }
}

#[test]
fn the_template_becomes_a_studio_file_with_its_own_header() {
    let choice = Choice::Cameras {
        cameras: vec![
            camera("FaceTime HD Camera", "0x1"),
            camera("Cam Link", "0x2"),
        ],
        mode: MODES[3],
    };
    let text = studio_text(TEMPLATE, &choice, true, Path::new("/s")).unwrap();
    assert!(text.starts_with(edit_header()), "{text}");
    assert!(!text.contains("Copy this file"));
    let config = config_of(&text);
    assert_eq!(config.sources.len(), 2);
    assert_eq!(config.sources[1].id, "cam-2");
    assert_eq!(config.output.audio_source, None);
    // The example's queries come along.
    assert_eq!(config.queries.len(), 3);
    // Without the detector the file still loads, saying why it is off.
    let off = edit::without_detector(&text).unwrap();
    assert!(off.contains("[detector]\nkind = \"none\""), "{off}");
    assert!(off.contains("turned off when this file was made"));
    assert!(matches!(config_of(&off).detector, DetectorSettings::None));
    assert!(!off.contains("owlv2-split-onnx/detector.onnx\""));
}

/// The header a studio file made from the template starts with.
fn edit_header() -> &'static str {
    "# Your studio: what Auto Camera opens"
}

#[test]
fn current_reads_the_file_or_the_cameras_and_their_mode() {
    let now = current(TWO_CAMERAS);
    assert_eq!(now.file, None);
    assert_eq!(
        now.cameras,
        [camera("Stage", "0xAA"), camera("Side", "0xBB")]
    );
    assert_eq!(now.mode, Some(MODES[1]));
    let now = current(TEMPLATE);
    assert_eq!(now.file, Some("../media/rehearsal.mp4".into()));
    assert!(now.cameras.is_empty());
    assert_eq!(current("not = [toml"), Current::default());
    assert_eq!(current(""), Current::default());
}

#[test]
fn invalid_input_is_refused_and_says_why() {
    let none = Choice::Cameras {
        cameras: Vec::new(),
        mode: MODES[0],
    };
    assert_eq!(
        studio_text(TWO_CAMERAS, &none, false, Path::new("/s")).unwrap_err(),
        "pick at least one camera"
    );
    let broken = studio_text(
        "[[sources]\n",
        &Choice::File("/a.mp4".into()),
        false,
        Path::new("/s"),
    );
    assert!(broken.unwrap_err().contains("not valid TOML"));
}

#[test]
fn save_writes_a_new_studio_and_then_edits_it_in_place() {
    let dir = temp_path("picker-save");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("show.toml");
    let video = dir.join("clip.mp4");
    save(&path, &Choice::File(video.clone())).unwrap();
    let made = std::fs::read_to_string(&path).unwrap();
    assert!(made.starts_with(edit_header()));
    let loaded = StudioConfig::load_with_warnings(&path).unwrap();
    assert_eq!(loaded.config.sources.len(), 1);
    // The example's model files are not beside this temp folder, so the
    // detector was turned off (and with no `onnx` it would be anyway).
    assert!(matches!(loaded.config.detector, DetectorSettings::None));
    // An edit keeps what the operator wrote.
    std::fs::write(&path, format!("# mine\n{made}")).unwrap();
    let cameras = Choice::Cameras {
        cameras: vec![camera("Cam", "0x1")],
        mode: MODES[0],
    };
    save(&path, &cameras).unwrap();
    let edited = std::fs::read_to_string(&path).unwrap();
    assert!(edited.starts_with("# mine\n"));
    assert!(edited.contains("unique_id = \"0x1\""));
    // A choice that would not load leaves the file as it was.
    let bad = Choice::Cameras {
        cameras: vec![camera("Cam", "0x1\u{7}")],
        mode: MODES[0],
    };
    let err = save(&path, &bad).unwrap_err();
    assert!(err.contains("was not changed"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn modes_have_labels_and_the_default_is_720p30() {
    assert_eq!(MODES[0].label(), "1280×720 at 30 fps");
    assert_eq!(MODES[3].label(), "1920×1080 at 60 fps");
}

#[test]
fn listing_cameras_off_macos_says_why() {
    if cfg!(target_os = "macos") {
        return;
    }
    let err = list_cameras().unwrap_err();
    assert!(err.contains("capture sources need macOS"), "{err}");
}
