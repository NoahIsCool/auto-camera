//! Tests for the `schema` key: reading the current version, a file without
//! the key, versions this build does not know, the migration hook (with
//! steps of a pretend schema 3), and studio files on disk.

use std::path::PathBuf;

use serde::Deserialize;

use super::*;
use crate::config::{ConfigError, StudioConfig};

/// A document of the pretend schema 3: schema 1 called `name` `title`, and
/// schema 2 gave `size` in centimetres, schema 3 in millimetres.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    /// The version, as on the studio config.
    #[serde(default = "unversioned")]
    schema: u32,
    /// Renamed from `title` by step 1.
    name: String,
    /// Scaled by step 2.
    #[serde(default)]
    size: u64,
}

/// Step 1 of the pretend schema: `title` becomes `name`.
fn rename_title(doc: &mut Map<String, Value>) -> Result<(), String> {
    let title = doc.remove("title").ok_or("schema 1 needs a `title`")?;
    doc.insert("name".into(), title);
    Ok(())
}

/// Step 2 of the pretend schema: `size` goes from centimetres to
/// millimetres.
fn size_in_mm(doc: &mut Map<String, Value>) -> Result<(), String> {
    if let Some(size) = doc.get_mut("size") {
        let cm = size.as_u64().ok_or("`size` must be a whole number")?;
        *size = Value::from(cm * 10);
    }
    Ok(())
}

/// The pretend schema's steps.
const STEPS: [Migration; 2] = [rename_title, size_in_mm];

/// Reads TOML `text` as the pretend schema 3.
fn read3(text: &str) -> Result<(Doc, Vec<String>), String> {
    read_with(text, false, 3, &STEPS)
}

/// A one-source studio with `head` above its tables.
fn studio(head: &str) -> String {
    format!(
        "{head}\n[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 640\n\
         height = 360\nfps = 30\n"
    )
}

#[test]
fn every_version_below_this_one_has_a_step() {
    assert_eq!(MIGRATIONS.len(), SCHEMA_VERSION as usize - 1);
    assert_eq!(SCHEMA_VERSION, 1);
}

#[test]
fn the_current_version_reads_straight_from_the_text() {
    let (doc, warnings) = read3("schema = 3\nname = \"stage\"\nsize = 7\n").unwrap();
    let want = Doc {
        schema: 3,
        name: "stage".into(),
        size: 7,
    };
    assert_eq!((doc, warnings), (want, vec![]));
    let msg = read3("schema = 3\nname = \"stage\"\nsize = \"big\"\n").unwrap_err();
    assert!(
        msg.contains("line 3"),
        "a parse error keeps its place: {msg}"
    );
}

#[test]
fn a_missing_key_reads_as_schema_1_with_a_warning() {
    let text = studio("");
    let (c, warnings) = StudioConfig::parse_with_warnings(&text, false).unwrap();
    assert_eq!(c.schema, 1);
    c.validate().unwrap();
    let [warning] = warnings.as_slice() else {
        panic!("{warnings:?}");
    };
    assert!(warning.contains("no `schema` key"), "{warning}");
    assert!(warning.contains("Add `schema = 1`"), "{warning}");
    assert!(warning.contains("before any [table]"), "{warning}");
    let json =
        r#"{"sources": [{"id": "s", "kind": "synthetic", "width": 64, "height": 36, "fps": 30}]}"#;
    let (c, warnings) = StudioConfig::parse_with_warnings(json, true).unwrap();
    assert_eq!(c.schema, 1);
    assert!(warnings[0].contains(r#"Add `"schema": 1`"#), "{warnings:?}");
    let (c, warnings) = StudioConfig::parse_with_warnings(&studio("schema = 1\n"), false).unwrap();
    assert_eq!((c.schema, warnings.len()), (1, 0));
}

#[test]
fn unknown_versions_are_errors_that_say_what_this_build_reads() {
    let newer = StudioConfig::parse(&studio("schema = 2\n"), false).unwrap_err();
    assert!(newer.contains("this file is schema 2, newer"), "{newer}");
    assert!(newer.contains("(schema 1)"), "{newer}");
    for v in ["0", "-3"] {
        let msg = StudioConfig::parse(&studio(&format!("schema = {v}\n")), false).unwrap_err();
        assert!(
            msg.contains(&format!("schema {v} is not a config version")),
            "{msg}"
        );
    }
    for v in ["\"1\"", "1.0", "true", "[1]"] {
        let msg = StudioConfig::parse(&studio(&format!("schema = {v}\n")), false).unwrap_err();
        assert!(msg.contains("schema must be a whole number"), "{v}: {msg}");
    }
    let msg = StudioConfig::parse(r#"{"schema": 9, "sources": []}"#, true).unwrap_err();
    assert!(msg.contains("schema 9"), "{msg}");
    let msg = StudioConfig::parse("schema = = 1\n", false).unwrap_err();
    assert!(
        msg.contains("line 1"),
        "syntax errors are the parser's: {msg}"
    );
}

#[test]
fn a_key_inside_a_table_is_not_the_schema() {
    let text = format!("{}\n[output]\nschema = 1\n", studio("schema = 1\n"));
    let msg = StudioConfig::parse(&text, false).unwrap_err();
    assert!(msg.contains("unknown field `schema`"), "{msg}");
}

#[test]
fn older_files_are_upgraded_one_step_at_a_time() {
    let (doc, warnings) = read3("schema = 1\ntitle = \"stage\"\nsize = 7\n").unwrap();
    let want = Doc {
        schema: 3,
        name: "stage".into(),
        size: 70,
    };
    assert_eq!(doc, want);
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains("schema 1; it was read as schema 3"),
        "{warnings:?}"
    );
    let (doc, _) = read3("schema = 2\nname = \"stage\"\nsize = 7\n").unwrap();
    assert_eq!(doc.size, 70, "only step 2 runs on a schema 2 file");
    let (doc, warnings) = read3("title = \"stage\"\n").unwrap();
    assert_eq!((doc.name.as_str(), doc.schema), ("stage", 3));
    assert_eq!(warnings.len(), 2, "no key, then upgraded: {warnings:?}");
    let json = r#"{"schema": 1, "title": "stage", "size": 2}"#;
    let (doc, _) = read_with::<Doc>(json, true, 3, &STEPS).unwrap();
    assert_eq!(doc.size, 20);
}

#[test]
fn a_failed_or_missing_step_says_which_versions() {
    let msg = read3("schema = 1\nname = \"stage\"\n").unwrap_err();
    assert_eq!(
        msg,
        "cannot upgrade schema 1 to 2: schema 1 needs a `title`"
    );
    let msg = read3("schema = 2\nname = \"s\"\nsize = -1\n").unwrap_err();
    assert!(msg.starts_with("cannot upgrade schema 2 to 3"), "{msg}");
    let msg = read_with::<Doc>("schema = 2\nname = \"s\"\n", false, 3, &STEPS[..1]).unwrap_err();
    assert_eq!(msg, "no migration from schema 2 to 3");
    let msg = read_with::<Doc>("schema = 1\n[[x]]\n", false, 3, &STEPS).unwrap_err();
    assert!(msg.contains("needs a `title`"), "{msg}");
    let msg = read_with::<Doc>("[1]", true, 3, &STEPS).unwrap_err();
    assert!(!msg.is_empty());
}

#[test]
fn a_config_written_out_starts_with_its_schema() {
    let c = StudioConfig::parse(&studio(""), false).unwrap();
    let text = toml::to_string(&c).unwrap();
    assert!(text.starts_with("schema = 1\n"), "{text}");
    let (back, warnings) = StudioConfig::parse_with_warnings(&text, false).unwrap();
    assert_eq!((back, warnings.len()), (c, 0));
    let mut c = StudioConfig::parse(&studio(""), false).unwrap();
    c.schema = 2;
    match c.validate() {
        Err(ConfigError::Invalid(msg)) => assert!(msg.contains("schema 2"), "{msg}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn loading_a_file_names_it_in_warnings_and_errors() {
    let dir = std::env::temp_dir().join(format!("ac-engine-schema-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old-studio.toml");
    std::fs::write(&path, studio("")).unwrap();
    let loaded = StudioConfig::load_with_warnings(&path).unwrap();
    let [warning] = loaded.warnings.as_slice() else {
        panic!("{:?}", loaded.warnings);
    };
    assert!(
        warning.starts_with(&format!("{}: no `schema` key", path.display())),
        "{warning}"
    );
    std::fs::write(&path, studio("schema = 2\n")).unwrap();
    match StudioConfig::load(&path) {
        Err(e @ ConfigError::Parse { .. }) => {
            let msg = e.to_string();
            assert!(msg.contains("old-studio.toml"), "{msg}");
            assert!(msg.contains("schema 2"), "{msg}");
        }
        other => panic!("{other:?}"),
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn every_example_config_declares_this_schema() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let loaded = StudioConfig::load_with_warnings(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(loaded.config.schema, SCHEMA_VERSION, "{}", path.display());
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        seen += 1;
    }
    assert!(seen >= 3, "{seen} example configs");
}
