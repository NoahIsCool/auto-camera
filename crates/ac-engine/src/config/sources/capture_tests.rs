//! Capture sources in the studio config: how an entry reads, how it is
//! described before it opens, and the request mistakes a load catches.

use super::*;
use crate::config::StudioConfig;

/// A studio with one capture source whose entry ends with `extra` lines.
fn studio(extra: &str) -> String {
    format!(
        r#"
schema = 1

[[sources]]
id = "cam-1"
kind = "capture"
unique_id = "0x14200000046d085e"
width = 1920
height = 1080
fps = 29.97
{extra}
"#
    )
}

#[test]
fn a_capture_entry_reads_its_keys_flattened() {
    let c = StudioConfig::parse(&studio(""), false).unwrap();
    c.validate().unwrap();
    assert_eq!(
        c.sources[0].input,
        SourceConfig::Capture {
            unique_id: "0x14200000046d085e".into(),
            width: 1920,
            height: 1080,
            fps: 29.97,
            pixel_format: None,
        }
    );
    let c = StudioConfig::parse(&studio(r#"pixel_format = "nv12""#), false).unwrap();
    c.validate().unwrap();
    assert!(matches!(
        &c.sources[0].input,
        SourceConfig::Capture { pixel_format: Some(p), .. } if p == "nv12"
    ));
}

#[test]
fn a_capture_entry_rejects_unknown_and_missing_keys() {
    // A typo of a key is an error, not an ignored setting.
    assert!(StudioConfig::parse(&studio("framerate = 30"), false).is_err());
    let no_id = r#"
[[sources]]
id = "cam-1"
kind = "capture"
width = 1280
height = 720
fps = 30
"#;
    let e = StudioConfig::parse(no_id, false).unwrap_err();
    assert!(e.contains("unique_id"), "{e}");
    let no_rate = r#"
[[sources]]
id = "cam-1"
kind = "capture"
unique_id = "0x1"
width = 1280
height = 720
"#;
    let e = StudioConfig::parse(no_rate, false).unwrap_err();
    assert!(e.contains("fps"), "{e}");
}

#[test]
fn a_capture_placeholder_is_its_requested_mode() {
    let c = StudioConfig::parse(&studio(""), false).unwrap();
    let info = c.sources[0].placeholder_info((1280, 720));
    assert_eq!(info.id, SourceId("cam-1".into()));
    assert_eq!((info.width, info.height, info.fps), (1920, 1080, 29.97));
    assert_eq!(info.audio, None);
}

#[test]
fn every_bad_capture_request_is_a_load_problem() {
    let text = r#"
[[sources]]
id = "cam-1"
kind = "capture"
unique_id = ""
width = 0
height = 720
fps = 0
pixel_format = "rgba"
"#;
    let c = StudioConfig::parse(text, false).unwrap();
    let problems = c.problems();
    let capture: Vec<&String> = problems
        .iter()
        .filter(|p| p.starts_with("source \"cam-1\": capture"))
        .collect();
    assert_eq!(capture.len(), 4, "{problems:?}");
    assert!(capture[0].contains("unique_id is empty"));
    assert!(capture[1].contains("size 0x720"));
    assert!(capture[2].contains("fps 0"));
    assert!(capture[3].contains("pixel_format \"rgba\""));
    assert!(c.validate().is_err());
}

#[test]
fn the_synthetic_detector_refuses_a_capture_source() {
    let c = StudioConfig::parse(&studio("\n[detector]\nkind = \"synthetic\""), false).unwrap();
    let problems = c.problems();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("only works with synthetic sources")),
        "{problems:?}"
    );
    let detector = crate::config::DetectorSettings::Synthetic { score: 0.9 };
    let e = crate::discovery::build_detector(&detector, &c.sources[0].input)
        .err()
        .unwrap();
    assert!(e.contains("only works on synthetic sources"), "{e}");
}

#[test]
fn the_capture_example_config_loads() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/capture.example.toml");
    let c = StudioConfig::load(&path).unwrap();
    assert!(matches!(c.sources[0].input, SourceConfig::Capture { .. }));
    let info = c.sources[0].placeholder_info((c.output.width, c.output.height));
    assert_eq!((info.width, info.height, info.fps), (1280, 720, 30.0));
}
