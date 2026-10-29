//! Device inputs in the studio config (`kind = "device"`): how an entry
//! reads and writes, the empty UID a load refuses, and that a device has
//! no path to resolve.

use std::path::Path;

use super::*;

/// A studio with one synthetic source and the board as a device input
/// whose entry ends with `extra` lines.
fn studio(extra: &str) -> String {
    format!(
        r#"
schema = 1

[[sources]]
id = "stage"
kind = "synthetic"
width = 64
height = 36
fps = 30

[[audio_inputs]]
id = "board"
kind = "device"
{extra}

[output]
audio_source = "board"
"#
    )
}

#[test]
fn a_device_entry_reads_its_uid_flattened_and_writes_it_back() {
    let text = studio("uid = \"AppleUSBAudioEngine:Allen&Heath:SQ:1A2B3C:1\"\nchannels = [1, 2]");
    let config = StudioConfig::parse(&text, false).unwrap();
    let entry = &config.audio_inputs[0];
    assert_eq!(
        entry.input,
        AudioInputConfig::Device {
            uid: "AppleUSBAudioEngine:Allen&Heath:SQ:1A2B3C:1".into()
        }
    );
    assert_eq!(entry.pick(), Ok(Some(ChannelPick::Pair(0, 1))));
    assert_eq!(config.problems(), Vec::<String>::new());
    let written = toml::to_string(&config).unwrap();
    assert!(written.contains("kind = \"device\""), "{written}");
    assert_eq!(StudioConfig::parse(&written, false).unwrap(), config);
}

#[test]
fn a_device_entry_needs_a_uid_and_takes_no_path() {
    for extra in ["", "uid = \"x\"\npath = \"board.wav\"", "uid = 3"] {
        assert!(
            StudioConfig::parse(&studio(extra), false).is_err(),
            "{extra:?} was accepted"
        );
    }
    let blank = StudioConfig::parse(&studio("uid = \" \""), false).unwrap();
    assert_eq!(
        blank.problems(),
        ["audio input \"board\": uid must name a device; \
          `auto-camera --list-audio-devices` prints the UIDs"]
    );
}

#[test]
fn a_device_has_no_path_to_resolve() {
    let mut config =
        StudioConfig::parse(&studio("uid = \"BuiltInMicrophoneDevice\""), false).unwrap();
    let before = config.audio_inputs.clone();
    config.resolve_paths(Path::new("/studio"));
    assert_eq!(config.audio_inputs, before);
    assert_eq!(config.audio_inputs[0].input.path(), None);
}
