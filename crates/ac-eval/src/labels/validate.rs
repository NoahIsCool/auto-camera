//! The rules a label file keeps, all checked in one pass.
//!
//! [`LabelFile::validate`] walks the whole file and reports every rule it
//! breaks as a [`FieldError`] that names the field at fault by its path in
//! the JSON document, such as `keyframes[2].people[0].bbox.x`, in document
//! order, so one run lists everything to fix. The paths are those of the
//! shape errors found while reading ([`LabelFile::from_json`]), so a path
//! names the same field whichever step found the problem. A rule about
//! several fields (a box's right edge is `x + w`) names the object that
//! holds them.
//!
//! # Rules
//!
//! * `schema_version` is [`SCHEMA_VERSION`].
//! * `clip`: `id` and `camera` are identifiers (ASCII letters, digits, `-`
//!   and `_`); `sha256` is 64 lowercase hex digits; `width` and `height`
//!   are positive; `fps` has non-zero parts and is at most [`MAX_FPS`];
//!   `frame_count` is in `1..=`[`MAX_FRAMES`].
//! * `exclusions`: ids are identifiers, unique; a polygon has at least
//!   three vertices, all inside the picture, enclosing a non-zero area.
//!   Self-intersection is not checked.
//! * `tracks`: ids are unique; tags are [`RoleTag`]s, each at most once per
//!   track.
//! * `roles`: an interval names a declared track and a role tag that is
//!   not already one of that track's tags; `start_frame < end_frame <=
//!   frame_count`; two intervals of one track and role never overlap
//!   (touching, one's `end_frame` the other's `start_frame`, is fine).
//! * `keyframes`: frames strictly increase and lie in `0..frame_count`; the
//!   frame nearest each `pts_ns` at the clip's rate is the keyframe's
//!   frame; a person names a declared track, at most once per keyframe;
//!   boxes have a positive size and lie inside the picture; visibility is
//!   in `0..=1`, and below 1 when the box is truncated; body lines lie in
//!   `0..=1` and inside the box, each below the one before it.
//!
//! Bounds on normalized values allow [`SLACK`] for rounding, far below a
//! pixel, so a box converted from pixels that touches an edge passes.
//!
//! A rule that depends on another field is skipped while that field is
//! itself broken (no edge check for a box whose `x` is out of range, no PTS
//! check without a usable frame rate), so one mistake gives one error.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;

use ac_core::{Point, Rect};

use super::{
    BodyLines, ClipManifest, ExclusionZone, FieldError, FieldErrors, Keyframe, LabelFile, MAX_FPS,
    MAX_FRAMES, PersonBox, PersonTrack, RoleInterval, RoleTag, SCHEMA_VERSION, TrackId,
};

/// How far a normalized value may stray past a bound before it counts:
/// rounding in a box converted from pixels, far below a pixel.
pub const SLACK: f64 = 1e-9;

impl LabelFile {
    /// Checks every rule in the [module docs](crate::labels::validate),
    /// reporting all broken ones in document order.
    pub fn validate(&self) -> Result<(), FieldErrors> {
        let mut report = Report::default();
        let root = Path::root();
        if self.schema_version != SCHEMA_VERSION {
            report.push(
                &root.field("schema_version"),
                format!(
                    "version {} is not supported; this build reads version {SCHEMA_VERSION}",
                    self.schema_version
                ),
            );
        }
        check_clip(&self.clip, &root.field("clip"), &mut report);
        check_exclusions(&self.exclusions, &root.field("exclusions"), &mut report);
        let index = check_tracks(&self.tracks, &root.field("tracks"), &mut report);
        let declared = Declared {
            tracks: &self.tracks,
            index,
        };
        check_roles(
            &self.roles,
            &declared,
            &self.clip,
            &root.field("roles"),
            &mut report,
        );
        check_keyframes(
            &self.keyframes,
            &declared,
            &self.clip,
            &root.field("keyframes"),
            &mut report,
        );
        report.finish()
    }
}

/// A field path being built, such as `keyframes[2].people`; empty at the
/// document root.
#[derive(Debug, Clone)]
struct Path(String);

impl Path {
    /// The document root.
    fn root() -> Self {
        Self(String::new())
    }

    /// The field `name` of the object at this path.
    fn field(&self, name: &str) -> Self {
        if self.0.is_empty() {
            Self(name.to_owned())
        } else {
            Self(format!("{}.{name}", self.0))
        }
    }

    /// Element `index` of the array at this path.
    fn index(&self, index: usize) -> Self {
        Self(format!("{}[{index}]", self.0))
    }
}

impl fmt::Display for Path {
    /// The path, or `.` at the root.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            f.write_str(".")
        } else {
            f.write_str(&self.0)
        }
    }
}

/// The errors found so far, in the order found (document order).
#[derive(Debug, Default)]
struct Report(Vec<FieldError>);

impl Report {
    /// Records that the field at `at` breaks a rule.
    fn push(&mut self, at: &Path, message: impl Into<String>) {
        self.0.push(FieldError::new(at.to_string(), message));
    }

    /// `Ok` when nothing was recorded.
    fn finish(self) -> Result<(), FieldErrors> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(FieldErrors(self.0))
        }
    }
}

/// The file's tracks and where each id is first declared, for resolving
/// references.
struct Declared<'a> {
    /// The tracks as declared.
    tracks: &'a [PersonTrack],
    /// Index in `tracks` of each id's first declaration.
    index: HashMap<TrackId, usize>,
}

impl Declared<'_> {
    /// The track declared as `id`, if any.
    fn get(&self, id: TrackId) -> Option<&PersonTrack> {
        self.index.get(&id).map(|&i| &self.tracks[i])
    }
}

/// Checks the manifest's ids, digest, picture size, rate and length.
fn check_clip(clip: &ClipManifest, at: &Path, report: &mut Report) {
    check_identifier(&clip.id.0, &at.field("id"), report);
    if !is_sha256(&clip.sha256) {
        report.push(
            &at.field("sha256"),
            format!("{:?} is not 64 lowercase hex digits", clip.sha256),
        );
    }
    check_identifier(&clip.camera.0, &at.field("camera"), report);
    for (name, size) in [("width", clip.width), ("height", clip.height)] {
        if size == 0 {
            report.push(&at.field(name), format!("0 is not a picture {name}"));
        }
    }
    let fps = at.field("fps");
    if clip.fps.num == 0 {
        report.push(&fps.field("num"), "0 frames is not a rate");
    }
    if clip.fps.den == 0 {
        report.push(&fps.field("den"), "0 seconds is not a rate");
    }
    if clip.fps.num > 0 && clip.fps.den > 0 && !clip.fps.is_valid() {
        report.push(
            &fps,
            format!("{} fps is above the {MAX_FPS} fps limit", clip.fps),
        );
    }
    if !(1..=MAX_FRAMES).contains(&clip.frame_count) {
        report.push(
            &at.field("frame_count"),
            format!("{} is outside 1 to {MAX_FRAMES}", clip.frame_count),
        );
    }
}

/// Checks zone ids and polygons.
fn check_exclusions(zones: &[ExclusionZone], at: &Path, report: &mut Report) {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (i, zone) in zones.iter().enumerate() {
        let at = at.index(i);
        let id = at.field("id");
        check_identifier(&zone.id, &id, report);
        match seen.entry(&zone.id) {
            Entry::Occupied(first) => report.push(
                &id,
                format!("{:?} is already exclusions[{}].id", zone.id, first.get()),
            ),
            Entry::Vacant(slot) => {
                slot.insert(i);
            }
        }
        let polygon = at.field("polygon");
        let mut inside = true;
        for (j, vertex) in zone.polygon.iter().enumerate() {
            let at = polygon.index(j);
            inside &= check_unit(vertex.x, &at.field("x"), report);
            inside &= check_unit(vertex.y, &at.field("y"), report);
        }
        if zone.polygon.len() < 3 {
            report.push(
                &polygon,
                format!(
                    "{} vertices are too few; a zone needs at least 3",
                    zone.polygon.len()
                ),
            );
        } else if inside && polygon_area(&zone.polygon) < SLACK {
            report.push(&polygon, "encloses no area: its vertices lie on one line");
        }
    }
}

/// Checks track ids and tags, returning where each id is first declared.
fn check_tracks(tracks: &[PersonTrack], at: &Path, report: &mut Report) -> HashMap<TrackId, usize> {
    let mut index = HashMap::new();
    for (i, track) in tracks.iter().enumerate() {
        let at = at.index(i);
        match index.entry(track.id) {
            Entry::Occupied(first) => report.push(
                &at.field("id"),
                format!("track {} is already tracks[{}]", track.id.0, first.get()),
            ),
            Entry::Vacant(slot) => {
                slot.insert(i);
            }
        }
        let tags = at.field("tags");
        let mut seen: HashMap<&RoleTag, usize> = HashMap::new();
        for (j, tag) in track.tags.iter().enumerate() {
            let at = tags.index(j);
            check_role_tag(tag, &at, report);
            match seen.entry(tag) {
                Entry::Occupied(first) => {
                    report.push(&at, format!("{:?} is already tags[{}]", tag.0, first.get()))
                }
                Entry::Vacant(slot) => {
                    slot.insert(j);
                }
            }
        }
    }
    index
}

/// Checks role intervals: references, spans and overlaps.
fn check_roles(
    roles: &[RoleInterval],
    declared: &Declared<'_>,
    clip: &ClipManifest,
    at: &Path,
    report: &mut Report,
) {
    // Earlier non-empty intervals of each track and role, by index.
    let mut held: HashMap<(TrackId, &RoleTag), Vec<usize>> = HashMap::new();
    for (i, interval) in roles.iter().enumerate() {
        let at = at.index(i);
        let track = declared.get(interval.track);
        if track.is_none() {
            report.push(&at.field("track"), unknown_track(interval.track));
        }
        let role = at.field("role");
        check_role_tag(&interval.role, &role, report);
        if let Some(track) = track
            && track.tags.contains(&interval.role)
        {
            report.push(
                &role,
                format!(
                    "track {} already has {:?} among its tags, which hold for the whole clip",
                    track.id.0, interval.role.0
                ),
            );
        }
        let end = at.field("end_frame");
        if interval.end_frame <= interval.start_frame {
            report.push(
                &end,
                format!(
                    "{} is not after start_frame {}",
                    interval.end_frame, interval.start_frame
                ),
            );
            continue;
        }
        if interval.end_frame > clip.frame_count {
            report.push(
                &end,
                format!(
                    "{} is past the clip's {} frames",
                    interval.end_frame, clip.frame_count
                ),
            );
        }
        let earlier = held.entry((interval.track, &interval.role)).or_default();
        if let Some(&j) = earlier.iter().find(|&&j| overlaps(&roles[j], interval)) {
            let other = &roles[j];
            report.push(
                &at,
                format!(
                    "overlaps roles[{j}]: track {} is already {:?} over frames {}..{}",
                    other.track.0, other.role.0, other.start_frame, other.end_frame
                ),
            );
        }
        earlier.push(i);
    }
}

/// Whether two non-empty half-open frame spans share a frame.
fn overlaps(a: &RoleInterval, b: &RoleInterval) -> bool {
    a.start_frame < b.end_frame && b.start_frame < a.end_frame
}

/// Checks keyframe order, timing and the people on each.
fn check_keyframes(
    keyframes: &[Keyframe],
    declared: &Declared<'_>,
    clip: &ClipManifest,
    at: &Path,
    report: &mut Report,
) {
    let rate = clip.fps.is_valid().then_some(clip.fps);
    let mut previous: Option<u64> = None;
    for (i, keyframe) in keyframes.iter().enumerate() {
        let at = at.index(i);
        let frame = at.field("frame");
        if keyframe.frame >= clip.frame_count {
            report.push(
                &frame,
                format!(
                    "{} is not inside the clip's {} frames (counted from 0)",
                    keyframe.frame, clip.frame_count
                ),
            );
        }
        if let Some(previous) = previous
            && keyframe.frame <= previous
        {
            report.push(
                &frame,
                format!(
                    "{} is not after the previous keyframe's {previous}; keyframes go in \
                     frame order, one per frame",
                    keyframe.frame
                ),
            );
        }
        previous = Some(keyframe.frame);
        if let Some(rate) = rate
            && let Some(nearest) = rate.nearest_frame(keyframe.pts_ns)
            && nearest != keyframe.frame
        {
            report.push(
                &at.field("pts_ns"),
                format!(
                    "{} ns is nearest frame {nearest} at {rate} fps, not frame {}",
                    keyframe.pts_ns, keyframe.frame
                ),
            );
        }
        let people = at.field("people");
        let mut seen: HashMap<TrackId, usize> = HashMap::new();
        for (j, person) in keyframe.people.iter().enumerate() {
            let at = people.index(j);
            let track = at.field("track");
            if declared.get(person.track).is_none() {
                report.push(&track, unknown_track(person.track));
            }
            match seen.entry(person.track) {
                Entry::Occupied(first) => report.push(
                    &track,
                    format!(
                        "track {} already has a box on this keyframe, people[{}]",
                        person.track.0,
                        first.get()
                    ),
                ),
                Entry::Vacant(slot) => {
                    slot.insert(j);
                }
            }
            check_person(person, &at, report);
        }
    }
}

/// Checks one person's box, visibility and body lines.
fn check_person(person: &PersonBox, at: &Path, report: &mut Report) {
    let box_ok = check_box(&person.bbox, &at.field("bbox"), report);
    let visibility = at.field("visibility");
    if check_unit(person.visibility, &visibility, report)
        && person.visibility >= 1.0
        && !person.truncation.is_none()
    {
        report.push(
            &visibility,
            format!(
                "{} means fully visible, but the box is cut at the {}",
                person.visibility,
                person.truncation.sides().join(" and ")
            ),
        );
    }
    let bbox = box_ok.then_some(&person.bbox);
    check_lines(&person.lines, bbox, &at.field("lines"), report);
}

/// Checks that a box has a positive size and lies inside the picture;
/// returns whether it does.
fn check_box(bbox: &Rect, at: &Path, report: &mut Report) -> bool {
    let x = check_unit(bbox.x, &at.field("x"), report);
    let y = check_unit(bbox.y, &at.field("y"), report);
    let w = check_size(bbox.w, &at.field("w"), report);
    let h = check_size(bbox.h, &at.field("h"), report);
    let mut ok = x && y && w && h;
    if x && w && bbox.right() > 1.0 + SLACK {
        report.push(at, format!("right edge x + w = {} is past 1", bbox.right()));
        ok = false;
    }
    if y && h && bbox.bottom() > 1.0 + SLACK {
        report.push(
            at,
            format!("bottom edge y + h = {} is past 1", bbox.bottom()),
        );
        ok = false;
    }
    ok
}

/// Checks body lines against the unit range, the box (when it is valid)
/// and each other.
fn check_lines(lines: &BodyLines, bbox: Option<&Rect>, at: &Path, report: &mut Report) {
    let mut above: Option<(&str, f64)> = None;
    for (name, line) in lines.named() {
        let Some(y) = line else { continue };
        let at = at.field(name);
        if !check_unit(y, &at, report) {
            continue;
        }
        if let Some(bbox) = bbox
            && (y < bbox.y - SLACK || y > bbox.bottom() + SLACK)
        {
            report.push(
                &at,
                format!(
                    "{y} is outside the box, which spans {} to {}",
                    bbox.y,
                    bbox.bottom()
                ),
            );
        }
        if let Some((upper, upper_y)) = above
            && y <= upper_y
        {
            report.push(
                &at,
                format!(
                    "{y} is not below the {upper} at {upper_y}; lines go crown, chin, \
                     shoulder, waist from the top"
                ),
            );
        }
        above = Some((name, y));
    }
}

/// Checks that a normalized value is a number in `0..=1`; returns whether
/// it is.
fn check_unit(value: f64, at: &Path, report: &mut Report) -> bool {
    if !value.is_finite() {
        report.push(at, format!("{value} is not a finite number"));
        false
    } else if !(-SLACK..=1.0 + SLACK).contains(&value) {
        report.push(at, format!("{value} is outside 0 to 1"));
        false
    } else {
        true
    }
}

/// Checks that a box side is a positive number; returns whether it is.
fn check_size(value: f64, at: &Path, report: &mut Report) -> bool {
    if !value.is_finite() {
        report.push(at, format!("{value} is not a finite number"));
        false
    } else if value <= 0.0 {
        report.push(at, format!("{value} is not a positive size"));
        false
    } else {
        true
    }
}

/// Checks that `text` is an identifier: ASCII letters, digits, `-`, `_`.
fn check_identifier(text: &str, at: &Path, report: &mut Report) {
    let valid = text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if text.is_empty() {
        report.push(at, "is empty");
    } else if !valid {
        report.push(
            at,
            format!("{text:?} has characters other than letters, digits, '-' and '_'"),
        );
    }
}

/// Checks the form of a role tag (see [`RoleTag`]).
fn check_role_tag(tag: &RoleTag, at: &Path, report: &mut Report) {
    let valid = tag.0.split([' ', '-', '_']).all(|word| {
        !word.is_empty()
            && word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    if !valid {
        report.push(
            at,
            format!(
                "{:?} is not a role tag: lowercase letters and digits, in words joined by \
                 single spaces, '-' or '_'",
                tag.0
            ),
        );
    }
}

/// Whether `text` is a SHA-256 digest in lowercase hex.
fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The message for a reference to a track that is not declared.
fn unknown_track(id: TrackId) -> String {
    format!("unknown track id {}: no entry in tracks has it", id.0)
}

/// Area enclosed by a simple polygon (the shoelace formula).
fn polygon_area(vertices: &[Point]) -> f64 {
    let n = vertices.len();
    let twice: f64 = (0..n)
        .map(|i| {
            let (a, b) = (vertices[i], vertices[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    (twice / 2.0).abs()
}
