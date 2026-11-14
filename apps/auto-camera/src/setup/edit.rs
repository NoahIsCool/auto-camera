//! Editing a studio file's text: the picker's [`Choice`] written as its
//! `[[sources]]`, the rest of the document kept.
//!
//! # Theory
//!
//! The file is parsed as a TOML document (`toml_edit`), which keeps
//! comments, blank lines and the order of tables, so replacing one array
//! of tables leaves every other line byte for byte. The new source tables
//! are placed where the old ones were: `toml_edit` prints tables in the
//! order of their document positions, and a table without one follows the
//! table printed before it, so the first new table takes the old first
//! one's position and the others none.
//!
//! # Invariants
//!
//! * Pure: text in, text out; no file is read or written here.
//! * A source table that stays (the same video path, a camera with the
//!   same unique ID, ignoring ASCII case) is kept whole; a camera's
//!   `width`, `height` and `fps` are set to the chosen mode.
//! * Every source gets an id no other source has: kept ones keep theirs,
//!   new ones take the lowest free `cam-N`.

use std::collections::BTreeSet;
use std::path::Path;

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use super::{Camera, Choice, Current, MODES, Mode};

/// The comment a new studio file opens with, in place of the example's.
const HEADER: &str = "\
# Your studio: what Auto Camera opens when it starts without --config.
#
# The source picker (Sources in the window's header) writes the
# [[sources]] below and keeps everything else, comments included, so edit
# the rest by hand as you like. It started as
# config/file-source.example.toml; `--check` lists anything missing.
";

/// The comment above a `[detector]` the picker turned off.
const NO_DETECTOR: &str = "\
# The example's OWLv2 detector was turned off when this file was made:
# this build lacks the `onnx` feature or the model files were missing.
# Without a detector only the wide shots exist. To find subjects, build
# with `--features onnx`, put the model files in place (see
# crates/ac-vision/README.md) and copy the [detector] table from
# config/file-source.example.toml.
";

/// The studio `base` with its sources replaced by `choice` (see the module
/// docs), for a file in the folder `dir` (which its relative paths are
/// read against); `template` swaps the example's opening comment for a
/// studio file's. The error says why the text cannot be edited.
pub fn studio_text(
    base: &str,
    choice: &Choice,
    template: bool,
    dir: &Path,
) -> Result<String, String> {
    let text = if template {
        with_header(base)
    } else {
        base.to_owned()
    };
    let mut doc = parse(&text)?;
    let old: Vec<Table> = doc
        .get("sources")
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();
    let mut tables = match choice {
        Choice::File(path) => vec![file_source(&old, path, dir)?],
        Choice::Cameras { cameras, mode } => {
            if cameras.is_empty() {
                return Err("pick at least one camera".into());
            }
            cameras
                .iter()
                .map(|c| camera_source(&old, c, *mode))
                .collect()
        }
    };
    assign_ids(&mut tables);
    let first = old.first().and_then(Table::position);
    let mut sources = ArrayOfTables::new();
    for (i, mut table) in tables.into_iter().enumerate() {
        table.set_position(if i == 0 { first } else { None });
        sources.push(table);
    }
    let ids: Vec<String> = sources
        .iter()
        .filter_map(|t| t.get("id").and_then(Item::as_str).map(str::to_owned))
        .collect();
    doc.insert("sources", Item::ArrayOfTables(sources));
    fix_audio_source(&mut doc, choice, &ids);
    Ok(doc.to_string())
}

/// The studio `text` with its `[detector]` turned off (`kind = "none"`),
/// saying why above it.
pub fn without_detector(text: &str) -> Result<String, String> {
    let mut doc = parse(text)?;
    let position = doc
        .get("detector")
        .and_then(Item::as_table)
        .and_then(Table::position);
    let mut table = Table::new();
    table.insert("kind", value("none"));
    table.set_position(position);
    table.decor_mut().set_prefix(format!("\n{NO_DETECTOR}"));
    doc.insert("detector", Item::Table(table));
    Ok(doc.to_string())
}

/// What the studio `text` reads now, for the picker to open on; nothing
/// when the text is not TOML.
pub fn current(text: &str) -> Current {
    let Ok(doc) = parse(text) else {
        return Current::default();
    };
    let mut now = Current::default();
    let Some(sources) = doc.get("sources").and_then(Item::as_array_of_tables) else {
        return now;
    };
    for table in sources.iter() {
        let get = |key| table.get(key).and_then(Item::as_str);
        match get("kind") {
            Some("file") if now.file.is_none() => now.file = get("path").map(Into::into),
            Some("capture") => {
                let Some(unique_id) = get("unique_id") else {
                    continue;
                };
                let name = get("name").unwrap_or(unique_id);
                now.cameras.push(Camera {
                    name: name.to_owned(),
                    unique_id: unique_id.to_owned(),
                });
                if now.mode.is_none() {
                    now.mode = mode_of(table);
                }
            }
            _ => {}
        }
    }
    now
}

/// Parses `text` as a TOML document.
fn parse(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>()
        .map_err(|e| format!("the studio file is not valid TOML: {e}"))
}

/// `template` with its opening comment (the lines before the first line
/// that is neither a comment nor blank) replaced by [`HEADER`].
fn with_header(template: &str) -> String {
    let body = template
        .lines()
        .position(|l| !(l.trim().is_empty() || l.trim_start().starts_with('#')))
        .map_or("", |n| {
            let start: usize = template.lines().take(n).map(|l| l.len() + 1).sum();
            &template[start.min(template.len())..]
        });
    format!("{HEADER}\n{body}")
}

/// The kept table of the old video file source at `path` (its own path
/// read against `dir`), or a new one: the file in a loop, decoded at most
/// 1280 wide, with its audio.
fn file_source(old: &[Table], path: &Path, dir: &Path) -> Result<Table, String> {
    let wanted = path;
    let path = path
        .to_str()
        .ok_or_else(|| format!("{} is not a UTF-8 path", path.display()))?;
    let kept = old.iter().find(|t| {
        t.get("kind").and_then(Item::as_str) == Some("file")
            && t.get("path")
                .and_then(Item::as_str)
                .is_some_and(|p| p == path || dir.join(p) == wanted)
    });
    if let Some(table) = kept {
        return Ok(table.clone());
    }
    let name = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Video");
    let mut table = Table::new();
    table.insert("name", value(name));
    table.insert("kind", value("file"));
    table.insert("path", value(path));
    table.insert("loop_playback", value(true));
    table.insert("max_width", value(1280));
    table.insert("audio", value(true));
    Ok(table)
}

/// The kept table of the old capture source of `camera`, or a new one,
/// asking for `mode`.
fn camera_source(old: &[Table], camera: &Camera, mode: Mode) -> Table {
    let kept = old.iter().find(|t| {
        t.get("kind").and_then(Item::as_str) == Some("capture")
            && t.get("unique_id")
                .and_then(Item::as_str)
                .is_some_and(|id| id.trim().eq_ignore_ascii_case(camera.unique_id.trim()))
    });
    let mut table = kept.cloned().unwrap_or_else(|| {
        let mut table = Table::new();
        table.insert("name", value(camera.name.as_str()));
        table.insert("kind", value("capture"));
        table.insert("unique_id", value(camera.unique_id.as_str()));
        table
    });
    set_value(&mut table, "width", value(i64::from(mode.width)));
    set_value(&mut table, "height", value(i64::from(mode.height)));
    let fps = if mode.fps.fract() == 0.0 {
        // Whole rates as integers, as the examples write them.
        value(mode.fps as i64)
    } else {
        value(mode.fps)
    };
    set_value(&mut table, "fps", fps);
    table
}

/// Sets `key` of `table` to `item`, keeping the key's comments and place
/// when it is there.
fn set_value(table: &mut Table, key: &str, item: Item) {
    match table.get_mut(key) {
        Some(slot) => match (slot.as_value_mut(), item) {
            (Some(old), Item::Value(mut new)) => {
                *new.decor_mut() = old.decor().clone();
                *old = new;
            }
            (_, item) => *slot = item,
        },
        None => {
            table.insert(key, item);
        }
    }
}

/// Gives every table without an id the lowest free `cam-N`, and puts the
/// id first in a new table.
fn assign_ids(tables: &mut [Table]) {
    let mut used: BTreeSet<String> = tables
        .iter()
        .filter_map(|t| t.get("id").and_then(Item::as_str).map(str::to_owned))
        .collect();
    for table in tables.iter_mut().filter(|t| !t.contains_key("id")) {
        let id = (1..)
            .map(|n| format!("cam-{n}"))
            .find(|id| !used.contains(id))
            .unwrap_or_default();
        used.insert(id.clone());
        let mut with_id = Table::new();
        with_id.insert("id", value(id));
        for (key, item) in table.iter() {
            with_id.insert(key, item.clone());
        }
        *table = with_id;
    }
}

/// Points `[output] audio_source` at what is left (see the parent module's
/// invariants): kept when it names an audio input, else the new video
/// file's source, or removed for cameras. Unset stays unset.
fn fix_audio_source(doc: &mut DocumentMut, choice: &Choice, ids: &[String]) {
    let inputs: BTreeSet<String> = doc
        .get("audio_inputs")
        .and_then(Item::as_array_of_tables)
        .map(|a| {
            a.iter()
                .filter_map(|t| t.get("id").and_then(Item::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let Some(output) = doc.get_mut("output").and_then(Item::as_table_like_mut) else {
        return;
    };
    let Some(source) = output.get("audio_source").and_then(Item::as_str) else {
        return;
    };
    if inputs.contains(source) {
        return;
    }
    match (choice, ids.first()) {
        (Choice::File(_), Some(id)) => {
            if let Some(slot) = output.get_mut("audio_source")
                && let Some(old) = slot.as_value_mut()
            {
                let mut new = toml_edit::Value::from(id.as_str());
                *new.decor_mut() = old.decor().clone();
                *old = new;
            }
        }
        _ => {
            output.remove("audio_source");
        }
    }
}

/// The capture mode a source table asks for, when it is one of [`MODES`].
fn mode_of(table: &Table) -> Option<Mode> {
    let int = |key| table.get(key).and_then(Item::as_integer);
    let width = u32::try_from(int("width")?).ok()?;
    let height = u32::try_from(int("height")?).ok()?;
    let fps = match table.get("fps")? {
        item if item.is_integer() => item.as_integer()? as f64,
        item => item.as_float()?,
    };
    MODES
        .into_iter()
        .find(|m| m.width == width && m.height == height && m.fps == fps)
}
