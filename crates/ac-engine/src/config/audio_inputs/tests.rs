//! Tests of the `[[audio_inputs]]` table: parsing, the config check
//! (acceptance: a pair outside the channel count is refused), the check of
//! an opened connection, and path resolution.

use std::path::PathBuf;

use ac_sources::SourceConfig;

use super::*;

/// A studio with one synthetic source "stage".
fn studio() -> StudioConfig {
    StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 64,
            height: 36,
            fps: 30.0,
            subjects: 1,
        },
    )
}

/// An entry "board" playing `board.wav`, picking `channels` of a
/// `channel_count`-channel input.
fn board(channels: Option<Vec<u16>>, channel_count: Option<u16>) -> AudioInputEntry {
    AudioInputEntry {
        id: "board".into(),
        name: None,
        channels,
        channel_count,
        input: AudioInputConfig::File {
            path: "board.wav".into(),
            loop_playback: false,
        },
    }
}

/// The problems of `studio()` with `entries` and program audio from
/// `audio_source`.
fn problems_of(entries: Vec<AudioInputEntry>, audio_source: Option<&str>) -> Vec<String> {
    let mut config = studio();
    config.audio_inputs = entries;
    config.output.audio_source = audio_source.map(Into::into);
    config.problems()
}

/// A description of an opened input with `channels` channels at 48 kHz.
fn opened(channels: u16) -> AudioInputInfo {
    AudioInputInfo {
        id: SourceId("board".into()),
        format: Some((48_000, channels)),
    }
}

#[test]
fn an_entry_reads_from_toml_with_its_input_flattened() {
    let text = r#"
schema = 1

[[sources]]
id = "stage"
kind = "synthetic"
width = 64
height = 36
fps = 30

[[audio_inputs]]
id = "board"
name = "Soundboard"
kind = "file"
path = "board.wav"
loop_playback = true
channels = [3, 4]
channel_count = 16

[output]
audio_source = "board"
"#;
    let config = StudioConfig::parse(text, false).unwrap();
    let entry = &config.audio_inputs[0];
    assert_eq!(entry.display_name(), "Soundboard");
    assert_eq!(entry.input_id(), SourceId("board".into()));
    assert_eq!(entry.pick(), Ok(Some(ChannelPick::Pair(2, 3))));
    assert_eq!(entry.channel_count, Some(16));
    assert_eq!(
        entry.input,
        AudioInputConfig::File {
            path: "board.wav".into(),
            loop_playback: true,
        }
    );
    assert_eq!(config.problems(), Vec::<String>::new());
    // Defaults: no name, the first two channels, no count, no loop.
    let minimal = "[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 64\n\
                   height = 36\nfps = 30\n\n[[audio_inputs]]\nid = \"board\"\n\
                   kind = \"file\"\npath = \"board.wav\"\n";
    let config = StudioConfig::parse(minimal, false).unwrap();
    assert_eq!(config.audio_inputs, vec![board(None, None)]);
    assert_eq!(config.audio_inputs[0].display_name(), "board");
    assert_eq!(config.audio_inputs[0].pick(), Ok(None));
}

#[test]
fn a_config_without_audio_inputs_loads_and_writes_as_before() {
    let config = studio();
    assert!(config.audio_inputs.is_empty());
    let text = toml::to_string(&config).unwrap();
    assert!(!text.contains("audio_inputs"), "{text}");
    let json = serde_json::to_string(&config).unwrap();
    assert!(!json.contains("audio_inputs"), "{json}");
    assert_eq!(StudioConfig::parse(&text, false).unwrap(), config);
}

#[test]
fn a_typo_or_an_unknown_kind_is_refused() {
    let base = "[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 64\n\
                height = 36\nfps = 30\n\n[[audio_inputs]]\nid = \"board\"\n";
    for (extra, what) in [
        (
            "kind = \"file\"\npath = \"b.wav\"\nloop = true\n",
            "a misspelt key",
        ),
        ("kind = \"device\"\npath = \"b.wav\"\n", "an unknown kind"),
        ("path = \"b.wav\"\n", "no kind"),
        (
            "kind = \"file\"\npath = \"b.wav\"\nchannels = [0, -1]\n",
            "a negative channel",
        ),
    ] {
        let text = format!("{base}{extra}");
        assert!(
            StudioConfig::parse(&text, false).is_err(),
            "{what} was accepted"
        );
    }
}

/// Acceptance: config rejects a pair outside the channel count.
#[test]
fn a_pair_outside_the_channel_count_is_refused() {
    let problems = problems_of(vec![board(Some(vec![17, 18]), Some(16))], Some("board"));
    assert_eq!(
        problems,
        vec![
            "audio input \"board\": channels 17 and 18 need an input of at least 18 channels; \
             it has 16 (channel_count)"
        ]
    );
    let mono = problems_of(vec![board(Some(vec![3]), Some(2))], None);
    assert_eq!(
        mono,
        vec![
            "audio input \"board\": channel 3 on both sides needs an input of at least 3 \
             channels; it has 2 (channel_count)"
        ]
    );
    // The last channel there is, and any pick when the count is not given.
    assert!(problems_of(vec![board(Some(vec![15, 16]), Some(16))], None).is_empty());
    assert!(problems_of(vec![board(Some(vec![31, 32]), None)], None).is_empty());
}

#[test]
fn bad_channel_lists_and_counts_are_refused() {
    let cases = [
        (board(Some(vec![]), None), "channels: list one channel"),
        (board(Some(vec![1, 2, 3]), None), "not 3"),
        (
            board(Some(vec![0, 1]), None),
            "channels are numbered from 1",
        ),
        (board(None, Some(0)), "channel_count must be at least 1"),
    ];
    for (entry, needle) in cases {
        let problems = problems_of(vec![entry.clone()], None);
        assert_eq!(problems.len(), 1, "{entry:?}: {problems:?}");
        assert!(problems[0].contains(needle), "{problems:?}");
        assert!(problems[0].starts_with("audio input \"board\": "));
    }
}

#[test]
fn ids_are_valid_unique_and_not_a_sources() {
    let mut bad = board(None, None);
    bad.id = "the board".into();
    let mut stage = board(None, None);
    stage.id = "stage".into();
    let problems = problems_of(vec![board(None, None), board(None, None), bad, stage], None);
    assert_eq!(
        problems,
        vec![
            "duplicate audio input id \"board\"".to_string(),
            "audio input \"the board\": its id must be non-empty letters, digits, '-' or '_'"
                .into(),
            "audio input \"stage\": a source has the same id, so output.audio_source could \
             mean either"
                .into(),
        ]
    );
}

#[test]
fn the_audio_source_names_a_source_or_an_audio_input() {
    assert!(problems_of(vec![board(None, None)], Some("stage")).is_empty());
    assert!(problems_of(vec![board(None, None)], Some("board")).is_empty());
    assert!(problems_of(vec![], None).is_empty());
    for entries in [vec![], vec![board(None, None)]] {
        assert_eq!(
            problems_of(entries, Some("nope")),
            vec!["output.audio_source \"nope\" is not a source or audio input id"]
        );
    }
}

#[test]
fn a_connection_is_checked_against_the_count_and_the_pick() {
    let pair = board(Some(vec![3, 4]), None).check_opened();
    assert_eq!(pair(&opened(16)), Ok(()));
    assert_eq!(pair(&opened(4)), Ok(()));
    assert_eq!(
        pair(&opened(2)),
        Err("channels 3 and 4 need an input of at least 4 channels; it has 2".into())
    );
    let counted = board(Some(vec![3, 4]), Some(16)).check_opened();
    assert_eq!(counted(&opened(16)), Ok(()));
    assert_eq!(
        counted(&opened(18)),
        Err("the input has 18 channels, not the 16 of channel_count".into())
    );
    let default = board(None, None).check_opened();
    for channels in [1, 2, 64] {
        assert_eq!(default(&opened(channels)), Ok(()), "{channels}");
    }
    assert_eq!(default(&opened(0)), Err("the input has no channels".into()));
    // A description without a format is not refused: there is nothing to
    // check yet.
    assert_eq!(default(&board(None, None).placeholder_info()), Ok(()));
}

#[test]
fn a_relative_path_is_resolved_against_the_config_folder() {
    let mut config = studio();
    let mut absolute = board(None, None);
    *absolute.input.path_mut().unwrap() = PathBuf::from("/media/board.wav");
    config.audio_inputs = vec![board(None, None), absolute];
    config.resolve_paths(Path::new("/studio"));
    let paths: Vec<_> = config
        .audio_inputs
        .iter()
        .map(|a| a.input.path().unwrap().to_path_buf())
        .collect();
    assert_eq!(
        paths,
        [
            PathBuf::from("/studio/board.wav"),
            PathBuf::from("/media/board.wav")
        ]
    );
}
