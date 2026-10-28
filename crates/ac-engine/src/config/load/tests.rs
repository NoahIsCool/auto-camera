//! Tests of reading config files from disk: a studio file, and a show laid
//! over the studio file it names, through every loader and the readiness
//! check. Every file is written in a fresh temp folder.

use std::path::PathBuf;

use ac_core::QueryId;
use ac_director::{ContentProfile, DirectorMode};
use ac_sources::FfmpegTools;

use super::*;
use crate::config::DetectorSettings;
use crate::readiness::{self, Entry, Inspect, Level};

/// A fresh temp folder for one test, removed by the test at its end.
fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "ac-engine-load-{}-{nanos}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A studio file without a `schema` key (so it draws a warning), with one
/// synthetic source, the synthetic detector, one query and a director.
const STUDIO: &str = "\
[[sources]]
id = \"stage\"
kind = \"synthetic\"
width = 320
height = 180
fps = 30

[[queries]]
name = \"Pastor\"
prompt = \"person\"

[detector]
kind = \"synthetic\"

[director]
mode = \"suggest\"
profile = \"talk\"
";

/// A show over `studio` that adds a query and sets the director.
fn show_over(studio: &str) -> String {
    format!(
        "show = 1\nstudio = \"{studio}\"\n\n[[queries]]\nname = \"Pastor\"\nprompt = \"person\"\n\n\
         [[queries]]\nname = \"Choir\"\nprompt = \"choir wide\"\n\n[director]\nmode = \"auto\"\n"
    )
}

/// A machine on which every file is there and every tool runs.
struct AllThere;

impl Inspect for AllThere {
    fn entry(&self, _path: &Path) -> Entry {
        Entry::File {
            readable: true,
            executable: true,
        }
    }

    fn can_create_in(&self, _dir: &Path) -> Result<(), String> {
        Ok(())
    }

    fn version(&self, _tool: &Path) -> Result<String, String> {
        Ok("ffmpeg version test".into())
    }

    fn find_program(&self, _name: &str) -> Option<PathBuf> {
        None
    }

    fn read_text(&self, _path: &Path) -> Result<String, String> {
        Err("not asked".into())
    }

    fn decoder_tools(&self) -> FfmpegTools {
        FfmpegTools {
            ffmpeg: "/bin/ffmpeg".into(),
            ffprobe: "/bin/ffprobe".into(),
        }
    }
}

#[test]
fn a_studio_file_reads_as_itself() {
    let dir = temp_dir("studio");
    let path = dir.join("studio.toml");
    std::fs::write(&path, STUDIO).unwrap();
    let loaded = StudioConfig::load_with_warnings(&path).unwrap();
    assert_eq!(
        (loaded.studio.as_path(), loaded.show),
        (path.as_path(), false)
    );
    assert_eq!(loaded.config.queries.len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_show_file_opens_as_its_studio_with_the_show_laid_over_it() {
    let dir = temp_dir("show");
    std::fs::write(dir.join("studio.toml"), STUDIO).unwrap();
    std::fs::create_dir(dir.join("shows")).unwrap();
    let path = dir.join("shows/sunday.show.toml");
    std::fs::write(&path, show_over("../studio.toml")).unwrap();
    for loaded in [
        StudioConfig::load_with_warnings(&path).unwrap(),
        StudioConfig::read_file(&path).unwrap(),
    ] {
        assert!(loaded.show);
        assert_eq!(loaded.studio, dir.join("shows/../studio.toml"));
        let c = &loaded.config;
        let ids: Vec<QueryId> = c.queries.iter().map(|q| q.query_id()).collect();
        assert_eq!(ids, [QueryId("pastor".into()), QueryId("choir".into())]);
        assert_eq!(c.director.mode, DirectorMode::Auto, "the show's");
        assert_eq!(c.director.profile, ContentProfile::Talk, "the studio's");
        assert!(matches!(c.detector, DetectorSettings::Synthetic { .. }));
        assert_eq!(c.sources[0].id, "stage");
        // The studio file's warning names the studio file.
        let [warning] = loaded.warnings.as_slice() else {
            panic!("{:?}", loaded.warnings);
        };
        assert!(
            warning.starts_with(&format!("{}: no `schema` key", loaded.studio.display())),
            "{warning}"
        );
    }
    // A JSON show over the same studio file opens too.
    let json = dir.join("shows/sunday.json");
    let studio = dir.join("studio.toml");
    std::fs::write(
        &json,
        format!(
            "{{\"show\": 1, \"studio\": {:?}, \"queries\": []}}",
            studio.display().to_string()
        ),
    )
    .unwrap();
    let loaded = StudioConfig::load(&json).unwrap();
    assert!(loaded.queries.is_empty(), "the show's empty list");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_show_needs_a_studio_file_that_is_there_and_not_a_show() {
    let dir = temp_dir("bad-studio");
    let path = dir.join("a.show.toml");
    std::fs::write(&path, "show = 1\n").unwrap();
    let err = StudioConfig::load(&path).unwrap_err().to_string();
    assert!(
        err.contains("a show names the studio file it lies over"),
        "{err}"
    );
    std::fs::write(&path, show_over("missing.toml")).unwrap();
    match StudioConfig::load(&path) {
        Err(ConfigError::Io { path: missing, .. }) => {
            assert_eq!(missing, dir.join("missing.toml"));
        }
        other => panic!("{other:?}"),
    }
    // A show over a show, or over itself, is refused rather than followed.
    std::fs::write(dir.join("b.show.toml"), show_over("a.show.toml")).unwrap();
    std::fs::write(&path, show_over("b.show.toml")).unwrap();
    let err = StudioConfig::load(&path).unwrap_err().to_string();
    assert!(
        err.contains("which is a show; it must name a studio file"),
        "{err}"
    );
    std::fs::write(&path, show_over("a.show.toml")).unwrap();
    assert!(StudioConfig::load(&path).is_err());
    // A show file that does not parse is reported as the show's.
    std::fs::write(&path, "show = 1\nstudio = = 2\n").unwrap();
    let err = StudioConfig::load(&path).unwrap_err().to_string();
    assert!(err.contains("a.show.toml"), "{err}");
    std::fs::write(&path, "show = 2\n").unwrap();
    match StudioConfig::load(&path) {
        Err(e @ ConfigError::Parse { .. }) => {
            assert!(e.to_string().contains("show schema 2, newer"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_show_meets_the_checks_a_studio_config_meets() {
    let dir = temp_dir("checked");
    std::fs::write(dir.join("studio.toml"), STUDIO).unwrap();
    let path = dir.join("sunday.show.toml");
    std::fs::write(
        &path,
        format!(
            "{}\n[switcher]\ntransition = \"mix\"\nduration = 99\n",
            show_over("studio.toml")
        ),
    )
    .unwrap();
    match StudioConfig::load(&path) {
        Err(ConfigError::Invalid(msg)) => assert!(msg.contains("switcher.duration 99"), "{msg}"),
        other => panic!("{other:?}"),
    }
    let found = readiness::check_file(&path, &AllThere);
    let problems: Vec<&str> = found.problems().collect();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("switcher.duration 99"), "{problems:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_readiness_check_accepts_a_show_file() {
    let dir = temp_dir("check");
    std::fs::write(dir.join("studio.toml"), STUDIO).unwrap();
    let path = dir.join("sunday.show.toml");
    std::fs::write(&path, show_over("studio.toml")).unwrap();
    let found = readiness::check_file(&path, &AllThere);
    assert!(found.is_ready(), "{:?}", found.findings);
    let config = found
        .findings
        .iter()
        .find(|f| f.message.starts_with("config:"))
        .unwrap();
    assert_eq!(config.level, Level::Ok);
    assert!(config.message.contains("2 queries"), "{}", config.message);
    assert_eq!(found.warnings().count(), 1, "the studio file's schema key");
    std::fs::remove_dir_all(&dir).unwrap();
}
