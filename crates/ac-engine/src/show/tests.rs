//! Tests of the show document: writing it out and reading it back, telling
//! it from a studio file, versions this build does not read, the migration
//! hook (with the step of a pretend show schema 2), and laying a show over
//! its studio key by key.

use std::path::{Path, PathBuf};

use ac_core::{Preset, ShotSize};
use ac_director::Takeover;
use serde::Deserialize;
use serde_json::Map;

use super::*;
use crate::config::PresetSpec;

/// A studio file's text with one synthetic source and `more` after it.
fn studio_text(more: &str) -> String {
    format!(
        "schema = 1\n\n[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 640\n\
         height = 360\nfps = 30\n{more}"
    )
}

/// A studio with one synthetic source and `more` after it.
fn studio(more: &str) -> StudioConfig {
    StudioConfig::parse(&studio_text(more), false).unwrap()
}

/// A show that gives every list and key, a full preset and a file source
/// included.
fn full_show() -> Show {
    let base = studio(
        "\n[[sources]]\nid = \"desk\"\nname = \"Desk\"\nkind = \"file\"\n\
         path = \"/media/desk.mp4\"\nloop_playback = true\n",
    );
    let mut tall = Preset::new("tall", "Tall", ShotSize::MediumClose);
    tall.headroom = 0.2;
    Show {
        show: SHOW_VERSION,
        studio: Some("/studios/sunday.toml".into()),
        sources: Some(base.sources),
        queries: Some(vec![
            QuerySettings {
                presets: Some(vec![
                    PresetSpec::Size(ShotSize::Medium),
                    PresetSpec::Full(tall),
                ]),
                ..QuerySettings::new("Pastor", "speaker upper body")
            },
            QuerySettings {
                id: Some("band".into()),
                literal: true,
                priority: Some(2),
                presets: Some(vec![PresetSpec::Size(ShotSize::Full)]),
                ..QuerySettings::new("Drums", "a drum kit")
            },
        ]),
        director: ShowDirector {
            mode: Some(DirectorMode::Auto),
            profile: Some(ContentProfile::Music),
        },
        switcher: ShowSwitcher {
            transition: Some(TransitionKind::Dip),
            duration: Some(1.5),
        },
        output: ShowOutput {
            rtmp_url: Some("rtmp://live.example.com/app/KEY".into()),
            record_path: Some("recordings/sunday.mp4".into()),
        },
    }
}

#[test]
fn a_show_written_out_says_what_it_is_first_and_reads_back() {
    let show = full_show();
    let text = show.to_text(false).unwrap();
    assert!(text.starts_with(HEADER), "{text}");
    let first = text[HEADER.len()..].lines().next().unwrap();
    assert_eq!(first, "show = 1");
    assert!(text.contains("studio = \"/studios/sunday.toml\""), "{text}");
    assert!(text.contains("presets = [\"full\"]"), "{text}");
    assert_eq!(
        Show::parse_with_warnings(&text, false),
        Ok((show.clone(), vec![]))
    );
    let json = show.to_text(true).unwrap();
    assert!(json.trim_start().starts_with("{\n  \"show\": 1,"), "{json}");
    assert_eq!(Show::parse_with_warnings(&json, true), Ok((show, vec![])));
}

#[test]
fn an_empty_list_and_missing_sections_are_written_as_given() {
    let show = Show {
        queries: Some(Vec::new()),
        sources: None,
        director: ShowDirector::default(),
        switcher: ShowSwitcher::default(),
        output: ShowOutput::default(),
        ..full_show()
    };
    let text = show.to_text(false).unwrap();
    assert!(text.contains("queries = []"), "{text}");
    for absent in ["[director]", "[switcher]", "[output]", "[[sources]]"] {
        assert!(!text.contains(absent), "{absent} in {text}");
    }
    assert_eq!(Show::parse_with_warnings(&text, false).unwrap().0, show);
}

#[test]
fn a_show_is_told_from_a_studio_file_by_its_key() {
    let show = full_show();
    assert!(Show::is_show(&show.to_text(false).unwrap(), false));
    assert!(Show::is_show(&show.to_text(true).unwrap(), true));
    assert!(Show::is_show("show = 7\n", false), "any version is a show");
    assert!(!Show::is_show(&studio_text(""), false));
    assert!(!Show::is_show(r#"{"schema": 1, "sources": []}"#, true));
    // A `show` key inside a table is not the file's.
    assert!(!Show::is_show(&studio_text("[output]\nshow = 1\n"), false));
    assert!(
        !Show::is_show("show = = 1\n", false),
        "text that does not parse"
    );
    assert!(!Show::is_show("[1]", true));
    // Read as a studio file, a show is refused by its key.
    let err = StudioConfig::parse(&show.to_text(false).unwrap(), false).unwrap_err();
    assert!(err.contains("unknown field `show`"), "{err}");
}

#[test]
fn versions_this_build_does_not_read_and_unknown_keys_are_errors() {
    let err = Show::parse_with_warnings("show = 2\n", false).unwrap_err();
    assert!(err.contains("this file is show schema 2, newer"), "{err}");
    assert!(err.contains("(show schema 1)"), "{err}");
    let err = Show::parse_with_warnings("show = 0\n", false).unwrap_err();
    assert!(err.contains("show schema 0 is not a show version"), "{err}");
    let err = Show::parse_with_warnings("show = \"1\"\n", false).unwrap_err();
    assert!(
        err.contains("show must be a whole number, as in `show = 1`"),
        "{err}"
    );
    let err = Show::parse_with_warnings("studio = \"s.toml\"\n", false).unwrap_err();
    assert!(err.contains("no `show` key"), "{err}");
    for bad in [
        "show = 1\ndetector = \"owl\"\n",
        "show = 1\n[director]\ntakeover = \"pause\"\n",
        "show = 1\n[switcher]\nspeed = 2\n",
        "show = 1\n[output]\nwidth = 640\n",
    ] {
        let err = Show::parse_with_warnings(bad, false).unwrap_err();
        assert!(err.contains("unknown field"), "{bad}: {err}");
    }
    let err = Show::parse_with_warnings("show = 1\n[director]\nmode = \"loud\"\n", false);
    assert!(err.is_err());
}

#[test]
fn every_show_version_below_this_one_has_a_step() {
    assert_eq!(MIGRATIONS.len(), SHOW_VERSION as usize - 1);
    assert_eq!(SHOW_VERSION, 1);
}

/// A show of the pretend schema 2, which adds the list `slots` (as a
/// later package may): the schema 1 show, flattened, and the new list.
#[derive(Debug, Deserialize)]
struct ShowV2 {
    /// Everything schema 1 had.
    #[serde(flatten)]
    show: Show,
    /// The pretend schema's new list, which a schema 1 file never has.
    slots: Vec<String>,
}

/// The pretend step from show schema 1 to 2: a schema 1 show has no
/// slots, so it gets an empty list; a file that has one is refused.
fn add_slots(doc: &mut Map<String, Value>) -> Result<(), String> {
    if doc.contains_key("slots") {
        return Err("a schema 1 show has no `slots`".into());
    }
    doc.insert("slots".into(), Value::Array(Vec::new()));
    Ok(())
}

/// The pretend build's steps.
const STEPS: [Migration; 1] = [add_slots];

/// Reads `text` as the pretend build, which reads show schema 2.
fn read2(text: &str, json: bool) -> Result<(ShowV2, Vec<String>), String> {
    let layout = Layout {
        current: 2,
        steps: &STEPS,
        ..LAYOUT
    };
    read_layout(text, json, &layout)
}

#[test]
fn a_schema_1_show_loads_through_a_migration_bump() {
    // A show as this build writes it, read by a build one show schema on.
    let written = full_show();
    for json in [false, true] {
        let text = written.to_text(json).unwrap();
        let (v2, warnings) = read2(&text, json).unwrap();
        assert_eq!(v2.slots, Vec::<String>::new());
        assert_eq!(v2.show.show, 2, "stamped with the version it was read as");
        assert_eq!(
            Show {
                show: SHOW_VERSION,
                ..v2.show
            },
            written,
            "nothing else changed"
        );
        let [warning] = warnings.as_slice() else {
            panic!("{warnings:?}");
        };
        assert!(
            warning.contains("this file is show schema 1; it was read as show schema 2"),
            "{warning}"
        );
    }
    // A schema 2 file reads straight, and a schema 3 one is too new.
    let (v2, warnings) = read2("show = 2\nslots = [\"pulpit\"]\n", false).unwrap();
    assert_eq!((v2.slots, warnings.len()), (vec!["pulpit".to_string()], 0));
    let err = read2("show = 3\n", false).unwrap_err();
    assert!(err.contains("newer than this build"), "{err}");
}

#[test]
fn a_failed_or_missing_show_step_says_which_versions() {
    let err = read2("show = 1\nslots = []\n", false).unwrap_err();
    assert_eq!(
        err,
        "cannot upgrade show schema 1 to 2: a schema 1 show has no `slots`"
    );
    let layout = Layout {
        current: 2,
        steps: &[],
        ..LAYOUT
    };
    let err = read_layout::<ShowV2>("show = 1\n", false, &layout).unwrap_err();
    assert_eq!(err, "no migration from show schema 1 to 2");
    let err = read_layout::<ShowV2>("[1]", true, &layout).unwrap_err();
    assert!(err.contains("no `show` key"), "{err}");
}

#[test]
fn a_show_lies_over_its_studio_key_by_key() {
    let base = studio(
        "\n[[queries]]\nname = \"Band\"\nprompt = \"person\"\n\n[director]\nmode = \"suggest\"\n\
         profile = \"music\"\ntakeover = \"pause\"\n\n[switcher]\ntransition = \"dip\"\n\
         duration = 2.0\n\n[output]\nrtmp_url = \"rtmp://a/b\"\nrecord_path = \"r.mp4\"\n",
    );
    // A show that gives nothing changes nothing.
    let mut c = base.clone();
    Show::parse_with_warnings("show = 1\n", false)
        .unwrap()
        .0
        .apply_to(&mut c);
    assert_eq!(c, base);
    // Each key given replaces that value only.
    let (show, _) = Show::parse_with_warnings(
        "show = 1\nqueries = []\n[director]\nmode = \"auto\"\n[switcher]\n\
         transition = \"mix\"\n[output]\nrtmp_url = \"\"\n",
        false,
    )
    .unwrap();
    let mut c = base.clone();
    show.apply_to(&mut c);
    assert!(c.queries.is_empty(), "an empty list leaves no queries");
    assert_eq!(c.sources, base.sources, "no sources given: the studio's");
    assert_eq!(c.director.mode, DirectorMode::Auto);
    assert_eq!(c.director.profile, ContentProfile::Music);
    assert_eq!(c.director.takeover, Takeover::Pause, "not a show key");
    assert_eq!(
        (c.switcher.transition, c.switcher.duration),
        (TransitionKind::Mix, 2.0)
    );
    assert_eq!(c.output.rtmp_url.as_deref(), Some(""), "empty means none");
    assert_eq!(c.output.record_path, Some(PathBuf::from("r.mp4")));
    c.validate().unwrap();
    // A full show replaces every list and key it holds.
    let mut c = base.clone();
    let show = full_show();
    show.apply_to(&mut c);
    assert_eq!(Some(&c.sources), show.sources.as_ref());
    assert_eq!(Some(&c.queries), show.queries.as_ref());
    assert_eq!(c.switcher.duration, 1.5);
    assert_eq!(c.output.rtmp_url, show.output.rtmp_url);
    assert_eq!(c.detector, base.detector, "the studio's tables stay");
}

#[test]
fn relative_paths_are_taken_from_the_show_file_folder() {
    let (mut show, _) = Show::parse_with_warnings(
        "show = 1\nstudio = \"../sunday.toml\"\n[[sources]]\nid = \"desk\"\nkind = \"file\"\n\
         path = \"media/desk.mp4\"\n[[sources]]\nid = \"abs\"\nkind = \"file\"\n\
         path = \"/media/abs.mp4\"\n[output]\nrecord_path = \"show.mp4\"\n",
        false,
    )
    .unwrap();
    show.resolve_paths(Path::new("/shows"));
    assert_eq!(show.studio, Some(PathBuf::from("/shows/../sunday.toml")));
    let paths: Vec<PathBuf> = show
        .sources
        .iter()
        .flatten()
        .map(|s| match &s.input {
            SourceConfig::File { path, .. } => path.clone(),
            SourceConfig::Synthetic { .. } => PathBuf::new(),
        })
        .collect();
    assert_eq!(
        paths,
        [
            PathBuf::from("/shows/media/desk.mp4"),
            PathBuf::from("/media/abs.mp4")
        ]
    );
    assert_eq!(
        show.output.record_path,
        Some(PathBuf::from("show.mp4")),
        "taken from the working folder, as in a studio file"
    );
}
