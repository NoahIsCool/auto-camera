//! Builders for the scorers' tests: a small clip and trace records whose
//! numbers are easy to work out by hand.
//!
//! The clip is 1000 by 1000 pixels at 10 fps, so frame `n` is presented at
//! `n * 100` ms, one normalized unit is 1000 pixels on either axis, and a
//! trace ticking at the clip's rate shows frame `n` on the tick at the same
//! PTS. Every record has one source, `cam`, whose subjects and shots the
//! test gives; shots crop `cam`.

use std::time::Duration;

use ac_core::{QueryId, Rect, ShotId, SourceId, SubjectId, TrackState};

use super::{Scope, score};
use crate::labels::{
    BodyLines, BoxExtent, CameraId, ClipId, ClipManifest, FrameRate, Keyframe, LabelFile,
    PersonBox, PersonTrack, RoleTag, SCHEMA_VERSION, TrackId, Truncation,
};
use crate::report::Report;
use crate::thresholds::Thresholds;
use crate::trace::{
    FrameTag, ShotTrace, SourceTrace, SubjectTrace, SwitcherTrace, TRACE_VERSION, TraceRecord,
    TransitionTrace,
};

/// The source every record has.
pub const CAM: &str = "cam";

/// Frames in the clip: 60 s at 10 fps.
pub const FRAMES: u64 = 600;

/// The clip's PTS of `frame`.
pub fn pts(frame: u64) -> Duration {
    Duration::from_millis(frame * 100)
}

/// A whole person `track` at `bbox`, fully visible, with no lines.
pub fn person(track: u32, bbox: Rect) -> PersonBox {
    PersonBox {
        track: TrackId(track),
        bbox,
        visibility: 1.0,
        truncation: Truncation::NONE,
        lines: BodyLines::default(),
    }
}

/// `p` with its crown, chin, shoulder and waist lines.
pub fn lined(mut p: PersonBox, crown: f64, chin: f64, shoulder: f64, waist: f64) -> PersonBox {
    p.lines = BodyLines {
        crown: Some(crown),
        chin: Some(chin),
        shoulder: Some(shoulder),
        waist: Some(waist),
    };
    p
}

/// The keyframe of `frame` with `people`.
pub fn keyframe(frame: u64, people: Vec<PersonBox>) -> Keyframe {
    Keyframe {
        frame,
        pts_ns: frame * 100_000_000,
        people,
    }
}

/// A valid label file of the test clip, with tracks `1..=tracks` (none
/// tagged) and `keyframes`.
pub fn labels(tracks: u32, keyframes: Vec<Keyframe>) -> LabelFile {
    LabelFile {
        schema_version: SCHEMA_VERSION,
        clip: ClipManifest {
            id: ClipId("test".into()),
            sha256: "0".repeat(64),
            camera: CameraId("cam".into()),
            width: 1000,
            height: 1000,
            fps: FrameRate::new(10, 1),
            frame_count: FRAMES,
        },
        box_extent: BoxExtent::Visible,
        exclusions: Vec::new(),
        tracks: (1..=tracks)
            .map(|id| PersonTrack {
                id: TrackId(id),
                tags: Vec::new(),
            })
            .collect(),
        roles: Vec::new(),
        keyframes,
    }
}

/// `labels` with track `track` tagged `role` for the whole clip.
pub fn tagged(mut labels: LabelFile, track: u32, role: &str) -> LabelFile {
    let t = labels
        .tracks
        .iter_mut()
        .find(|t| t.id == TrackId(track))
        .expect("the track exists");
    t.tags.push(RoleTag(role.into()));
    labels
}

/// A tracking subject `id` of query `person` at `bbox`.
pub fn subject(id: u32, bbox: Rect) -> SubjectTrace {
    SubjectTrace {
        id: SubjectId(id),
        query: QueryId("person".into()),
        bbox,
        state: TrackState::Tracking,
    }
}

/// `s`, lost.
pub fn lost(mut s: SubjectTrace) -> SubjectTrace {
    s.state = TrackState::Lost;
    s
}

/// The shot `id` of subject `subject` (a wide for `None`), cropping
/// `crop` of `cam`, aiming at the same crop.
pub fn shot(id: &str, subject: Option<u32>, crop: Rect) -> ShotTrace {
    ShotTrace {
        id: ShotId(id.into()),
        source: SourceId(CAM.into()),
        subject: subject.map(SubjectId),
        target: Some(crop),
        crop,
    }
}

/// The wide of `cam`.
pub fn wide() -> ShotTrace {
    shot("cam/wide", None, Rect::FULL)
}

/// The record of tick `frame + 1`, showing `frame`, with `subjects`,
/// `shots` and `program` on program (no preview, no transition).
pub fn record(
    frame: u64,
    subjects: Vec<SubjectTrace>,
    shots: Vec<ShotTrace>,
    program: Option<&str>,
) -> TraceRecord {
    TraceRecord {
        version: TRACE_VERSION,
        tick: frame + 1,
        pts: pts(frame),
        late: false,
        skipped: 0,
        sources: vec![SourceTrace {
            id: SourceId(CAM.into()),
            frame: Some(FrameTag {
                seq: frame,
                pts: pts(frame),
            }),
            subjects,
        }],
        shots,
        switcher: SwitcherTrace {
            program: program.map(|p| ShotId(p.into())),
            preview: None,
            transition: None,
        },
    }
}

/// `r` with `preview` on preview and, when given, a transition drawing
/// `base` with the preview over it at `overlay`.
pub fn transition(
    mut r: TraceRecord,
    preview: &str,
    base: Option<&str>,
    overlay: f64,
) -> TraceRecord {
    r.switcher.preview = Some(ShotId(preview.into()));
    r.switcher.transition = Some(TransitionTrace {
        base: base.map(|b| ShotId(b.into())),
        overlay,
        dip: 0.0,
    });
    r
}

/// The scope of the test clip: `cam`, talk, every query for everyone.
pub fn scope() -> Scope {
    Scope::new(SourceId(CAM.into()))
}

/// The report of `trace` over `labels` with the default thresholds.
pub fn report(labels: &LabelFile, trace: &[TraceRecord]) -> Report {
    score(labels, trace, &scope(), &Thresholds::default()).expect("the run scores")
}

/// The value of metric `name` (for query `person` when `per_query`).
pub fn value(report: &Report, name: &str) -> Option<f64> {
    report
        .metric(name, None)
        .or_else(|| report.metric(name, Some("person")))
        .unwrap_or_else(|| panic!("{name} is in the report"))
        .value()
}
