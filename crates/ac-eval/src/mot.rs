//! Person tracks from MOT Challenge ground truth.
//!
//! MOT ground truth (`gt/gt.txt` in MOT15 to MOT20, and the "MOT 1.1"
//! export of labelling tools such as CVAT) is one comma-separated row per
//! person per frame:
//!
//! ```text
//! frame, id, left, top, width, height[, consider[, class[, visibility[, z]]]]
//! ```
//!
//! [`import_mot`] turns that text into a [`LabelFile`] for the clip the
//! caller describes. It parses a string: reading `gt.txt`, and the
//! `seqinfo.ini` that gives the manifest's frame count, rate and size, is
//! the runner's job, so this crate does no I/O.
//!
//! # Mapping
//!
//! * MOT counts frames from 1, label files from 0: MOT frame `n` is frame
//!   `n - 1`. Every frame of the clip becomes a keyframe, its PTS that of
//!   the frame at the clip's rate, because MOT ground truth labels every
//!   frame: a frame no row names had nobody in it.
//! * A row is a person when its consider flag (column 7) is absent or 1 and
//!   its class (column 8) is absent or 1, MOT's pedestrian. Other rows are
//!   skipped: flag 0 marks entries the MOT evaluation ignores, and the
//!   other classes are vehicles, occluders, reflections and distractors.
//!   Those per-frame ignored boxes have no counterpart in a label file,
//!   whose exclusion zones are fixed areas of the camera's picture, so they
//!   are dropped. `-1` in columns 8 and 9 means absent, as in MOT15's
//!   unused world coordinates; column 10 is not read.
//! * Pixel boxes are divided by the manifest's width and height, the
//!   picture spanning `0..width` and `0..height`. MOT annotators draw the
//!   whole person even past the picture's edge, so such a box is cut at the
//!   edge and marked truncated there, and its visibility (1 when absent) is
//!   capped at the share of the box left inside.
//! * Track ids are kept as they are. MOT has no tags, roles, body lines or
//!   exclusion zones, so the result has none, ready for them to be added.
//!
//! Blank lines and lines starting with `#` are skipped.
//!
//! # Errors
//!
//! A row that cannot be read stops the import with its line number
//! ([`MotError::Row`]): too few or too many columns, a value that is not a
//! number, a frame outside the clip, a row without a track id (`-1`, as in
//! detection and result files, which are not ground truth), a confidence
//! where the consider flag belongs, a visibility outside `0..=1`, a box
//! with no size or entirely outside the picture, or a second box for one
//! track on one frame. The manifest is validated before any row is read and
//! the finished file after ([`MotError::Invalid`]).

use std::collections::{BTreeMap, BTreeSet};

use ac_core::Rect;
use thiserror::Error;

use crate::labels::validate::SLACK;
use crate::labels::{
    ClipManifest, FieldErrors, Keyframe, LabelFile, PersonBox, PersonTrack, SCHEMA_VERSION,
    TrackId, Truncation,
};

/// Why MOT ground truth could not be imported.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MotError {
    /// A row cannot be read.
    #[error("line {line}: {message}")]
    Row {
        /// The row's line in the text, counted from 1.
        line: usize,
        /// What is wrong with it.
        message: String,
    },
    /// The manifest, or the file built with it, breaks a label-file rule.
    #[error("{0}")]
    Invalid(FieldErrors),
}

/// Fewest columns in a row: frame, id and the box.
const MIN_COLUMNS: usize = 6;

/// Most columns in a row: MOT15's world coordinates end at column 10.
const MAX_COLUMNS: usize = 10;

/// MOT's pedestrian class, the only class imported.
const PEDESTRIAN: f64 = 1.0;

/// Builds the label file for `clip` from MOT ground truth `text`; see the
/// module docs for how rows map to keyframes and people.
pub fn import_mot(text: &str, clip: ClipManifest) -> Result<LabelFile, MotError> {
    let mut labels = LabelFile {
        schema_version: SCHEMA_VERSION,
        clip,
        exclusions: Vec::new(),
        tracks: Vec::new(),
        roles: Vec::new(),
        keyframes: Vec::new(),
    };
    // Rows are read against the manifest's size and length, and its frame
    // count sizes the keyframe list, so it is checked first.
    labels.validate().map_err(MotError::Invalid)?;
    let mut frames: BTreeMap<u64, BTreeMap<TrackId, PersonBox>> = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let row = line.trim();
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        let error = |message: String| MotError::Row {
            line: n + 1,
            message,
        };
        let Some((frame, person)) = read_row(row, &labels.clip).map_err(error)? else {
            continue;
        };
        let people = frames.entry(frame).or_default();
        if people.contains_key(&person.track) {
            return Err(error(format!(
                "track {} already has a box on MOT frame {}",
                person.track.0,
                frame + 1
            )));
        }
        people.insert(person.track, person);
    }
    let ids: BTreeSet<TrackId> = frames.values().flat_map(|p| p.keys().copied()).collect();
    labels.tracks = ids
        .into_iter()
        .map(|id| PersonTrack {
            id,
            tags: Vec::new(),
        })
        .collect();
    let rate = labels.clip.fps;
    labels.keyframes = (0..labels.clip.frame_count)
        .map(|frame| Keyframe {
            frame,
            pts_ns: rate.pts_ns(frame).expect("the manifest's rate is valid"),
            people: frames
                .remove(&frame)
                .map(|people| people.into_values().collect())
                .unwrap_or_default(),
        })
        .collect();
    labels.validate().map_err(MotError::Invalid)?;
    Ok(labels)
}

/// Reads one row: the frame (counted from 0) and the person on it, or
/// `None` for a row that is not an imported person.
fn read_row(row: &str, clip: &ClipManifest) -> Result<Option<(u64, PersonBox)>, String> {
    let columns: Vec<&str> = row.split(',').map(str::trim).collect();
    if !(MIN_COLUMNS..=MAX_COLUMNS).contains(&columns.len()) {
        return Err(format!(
            "{} columns; a MOT row has {MIN_COLUMNS} to {MAX_COLUMNS}",
            columns.len()
        ));
    }
    let frame = whole(&columns, 0, "frame")?;
    if frame < 1 {
        return Err(format!("frame {frame}: MOT counts frames from 1"));
    }
    let frame = frame.unsigned_abs() - 1;
    if frame >= clip.frame_count {
        return Err(format!(
            "frame {} is past the clip's {} frames",
            frame + 1,
            clip.frame_count
        ));
    }
    let id = whole(&columns, 1, "id")?;
    if id == -1 {
        return Err(String::from(
            "track id -1 marks a detection without identity; only ground truth \
             with ids can be imported",
        ));
    }
    let id = u32::try_from(id).map_err(|_| format!("track id {id} is not in 0 to {}", u32::MAX))?;
    let left = number(&columns, 2, "left")?;
    let top = number(&columns, 3, "top")?;
    let width = number(&columns, 4, "width")?;
    let height = number(&columns, 5, "height")?;
    if width <= 0.0 || height <= 0.0 {
        return Err(format!(
            "the box is {width} by {height} pixels; it needs a positive size"
        ));
    }
    if columns.len() > 6 {
        let flag = number(&columns, 6, "consider")?;
        if flag != 0.0 && flag != 1.0 {
            return Err(format!(
                "column 7 is the consider flag, 0 or 1, but holds {flag}; confidences \
                 belong to detection and result files, not ground truth"
            ));
        }
        if flag == 0.0 {
            return Ok(None);
        }
    }
    if let Some(class) = optional(&columns, 7, "class")?
        && class != PEDESTRIAN
    {
        return Ok(None);
    }
    let visibility = optional(&columns, 8, "visibility")?;
    if let Some(v) = visibility
        && !(0.0..=1.0).contains(&v)
    {
        return Err(format!("visibility {v} is outside 0 to 1"));
    }
    // The box is cut to the picture in pixels, where whole-pixel inputs
    // stay exact, and only then normalized.
    let (picture_w, picture_h) = (f64::from(clip.width), f64::from(clip.height));
    let (right, bottom) = (left + width, top + height);
    let (x0, y0) = (left.max(0.0), top.max(0.0));
    let (x1, y1) = (right.min(picture_w), bottom.min(picture_h));
    if x1 <= x0 || y1 <= y0 {
        return Err(format!(
            "the box lies entirely outside the {}x{} picture",
            clip.width, clip.height
        ));
    }
    let truncation = Truncation {
        top: -top / picture_h > SLACK,
        bottom: (bottom - picture_h) / picture_h > SLACK,
        left: -left / picture_w > SLACK,
        right: (right - picture_w) / picture_w > SLACK,
    };
    let bbox = Rect::new(
        x0 / picture_w,
        y0 / picture_h,
        (x1 - x0) / picture_w,
        (y1 - y0) / picture_h,
    );
    let mut visibility = visibility.unwrap_or(1.0);
    if !truncation.is_none() {
        visibility = visibility.min((x1 - x0) * (y1 - y0) / (width * height));
    }
    let person = PersonBox {
        track: TrackId(id),
        bbox,
        visibility,
        truncation,
        lines: Default::default(),
    };
    Ok(Some((frame, person)))
}

/// Column `index` (counted from 0) as a finite number.
fn number(columns: &[&str], index: usize, name: &str) -> Result<f64, String> {
    let text = columns[index];
    match text.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(value),
        _ => Err(format!(
            "column {} ({name}) is {text:?}, not a number",
            index + 1
        )),
    }
}

/// Column `index` as a whole number.
fn whole(columns: &[&str], index: usize, name: &str) -> Result<i64, String> {
    let value = number(columns, index, name)?;
    // Beyond 2^53 a float no longer holds every whole number.
    if value.fract() != 0.0 || value.abs() > 9_007_199_254_740_992.0 {
        return Err(format!(
            "column {} ({name}) is {value}, not a whole number",
            index + 1
        ));
    }
    Ok(value as i64)
}

/// Column `index` as a number, `None` when the row is shorter or the
/// column holds MOT's `-1` for absent.
fn optional(columns: &[&str], index: usize, name: &str) -> Result<Option<f64>, String> {
    if index >= columns.len() {
        return Ok(None);
    }
    let value = number(columns, index, name)?;
    Ok((value != -1.0).then_some(value))
}

#[cfg(test)]
mod tests;
