//! The scorers through the public API, from text as the runner will read
//! it: the synthetic label file in `tests/fixtures`, a trace written and
//! read back as JSON Lines, a scope in JSON and the thresholds in TOML,
//! scored into a JSON report.
//!
//! The run: the pastor (track 1) and the others stand where the keyframes
//! put them; subject 1 (query `pastor`) follows the pastor and subjects 2
//! and 3 (query `person`) the others. The wide is on program for the first
//! half second, then the pastor's medium shot, which holds still: 0.4 high
//! with its top at 0.218, so the pastor's crown at 0.25 sits 0.032 / 0.4 =
//! 0.08 below it on the keyframe at 0.5 s (inside the medium band, 0.07 to
//! 0.10), but at 0.26 on the keyframe at 1.5 s, 0.042 / 0.4 = 0.105 below
//! it, outside the band.
//!
//! The same run over a segment of the clip (a file source's `start_s`),
//! whose PTS start again at 0, scores as that part of the whole run does,
//! once the scope gives the same `start_s`.

use std::ops::Range;
use std::time::Duration;

use ac_core::{QueryId, Rect, ShotId, SourceId, SubjectId, TrackState};
use ac_eval::trace::{FrameTag, ShotTrace, SourceTrace, SubjectTrace, SwitcherTrace};
use ac_eval::{
    DEFAULT_THRESHOLDS, LabelFile, Report, Scope, TRACE_VERSION, Thresholds, TraceRecord, Verdict,
    parse_trace, score,
};

/// The synthetic label file: 60 frames at 30000/1001 fps, keyframes at
/// frames 0, 15, 30 and 45.
const LABELS: &str = include_str!("fixtures/synthetic.labels.json");

/// The scope: the clip played on `stage`; `pastor` looks for the pastor,
/// `person` for everyone.
const SCOPE: &str = r#"{
    "source": "stage",
    "queries": [{ "query": "pastor", "role": "pastor" }, { "query": "person" }]
}"#;

/// The pastor's medium shot, held still.
const MEDIUM: Rect = Rect::new(0.3, 0.218, 0.4, 0.4);

/// The trace of the run in the module docs, as JSON Lines.
fn trace_text(labels: &LabelFile) -> String {
    segment_text(labels, 0..labels.clip.frame_count, false)
}

/// The trace of the run in the module docs over the clip's `frames`, as
/// JSON Lines. When `rebased`, the trace is of a source playing just those
/// frames: ticks, sequence numbers and PTS count from the range's first
/// frame, as a file source playing a segment counts them; otherwise from
/// the clip's.
fn segment_text(labels: &LabelFile, frames: Range<u64>, rebased: bool) -> String {
    let stage = SourceId("stage".into());
    let first = frames.start;
    frames
        .map(|frame| {
            let seq = if rebased { frame - first } else { frame };
            let pts = Duration::from_nanos(labels.clip.fps.pts_ns(seq).unwrap());
            // Everyone where the latest keyframe put them.
            let keyframe = labels
                .keyframes
                .iter()
                .rev()
                .find(|k| k.frame <= frame)
                .unwrap();
            let subjects = keyframe
                .people
                .iter()
                .map(|p| SubjectTrace {
                    id: SubjectId(p.track.0),
                    query: QueryId(if p.track.0 == 1 { "pastor" } else { "person" }.into()),
                    bbox: p.bbox,
                    state: TrackState::Tracking,
                })
                .collect();
            let shot = |id: &str, subject, crop| ShotTrace {
                id: ShotId(id.into()),
                source: stage.clone(),
                subject,
                target: Some(crop),
                crop,
            };
            let program = if frame < 15 {
                "stage/wide"
            } else {
                "stage/1/medium"
            };
            TraceRecord {
                version: TRACE_VERSION,
                tick: seq + 1,
                pts,
                late: false,
                skipped: 0,
                sources: vec![SourceTrace {
                    id: stage.clone(),
                    frame: Some(FrameTag { seq, pts }),
                    subjects,
                }],
                shots: vec![
                    shot("stage/wide", None, Rect::FULL),
                    shot("stage/1/medium", Some(SubjectId(1)), MEDIUM),
                ],
                switcher: SwitcherTrace {
                    program: Some(ShotId(program.into())),
                    preview: None,
                    transition: None,
                },
            }
            .to_json_line()
        })
        .collect()
}

/// The report of the run against `thresholds` (TOML text).
fn run(thresholds: &str) -> Report {
    let labels = LabelFile::from_json(LABELS).expect("the fixture is valid");
    let trace = parse_trace(&trace_text(&labels)).expect("the trace reads back");
    let scope: Scope = serde_json::from_str(SCOPE).expect("the scope reads");
    let thresholds = Thresholds::from_toml(thresholds).expect("the thresholds read");
    score(&labels, &trace, &scope, &thresholds).expect("the run scores")
}

/// The report of `trace` (JSON Lines) with the scope's `start_s` set to
/// `start_s`, against the default thresholds.
fn scored(labels: &LabelFile, trace: &str, start_s: f64) -> Report {
    let trace = parse_trace(trace).expect("the trace reads back");
    let mut scope: Scope = serde_json::from_str(SCOPE).expect("the scope reads");
    scope.start_s = start_s;
    let thresholds = Thresholds::from_toml(DEFAULT_THRESHOLDS).expect("the thresholds read");
    score(labels, &trace, &scope, &thresholds).expect("the run scores")
}

/// One metric's name, query, value (rounded to the nanosecond, as the two
/// clocks round frame times apart), samples and verdict.
type Line = (String, Option<String>, Option<i64>, u64, Verdict);

/// Each metric of `report`, as a [`Line`].
fn summary(report: &Report) -> Vec<Line> {
    report
        .metrics
        .iter()
        .map(|m| {
            (
                m.name.clone(),
                m.query.as_ref().map(|q| q.0.clone()),
                m.value().map(|v| (v * 1e9).round() as i64),
                m.samples,
                m.verdict,
            )
        })
        .collect()
}

#[test]
fn the_run_fails_on_the_headroom_alone_and_says_so_in_json() {
    let report = run(DEFAULT_THRESHOLDS);
    assert_eq!(report.coverage.ticks, 60);
    assert_eq!(report.coverage.scored_keyframes, 4);
    let failed: Vec<&str> = report.failures().map(|m| m.name.as_str()).collect();
    assert_eq!(failed, ["framing.headroom.medium"]);
    assert!(!report.pass);
    // One of the two keyframes with the crown labelled is in band.
    let medium = report.metric("framing.headroom.medium", None).unwrap();
    assert_eq!((medium.value(), medium.samples), (Some(0.5), 2));
    assert!((medium.details["highest"] - 0.105).abs() < 1e-9);
    // Only the keyframe at 0.5 s labels the crown, chin and shoulders the
    // head and torso check needs.
    let kept = report.metric("framing.head_torso_inside", None).unwrap();
    assert_eq!(
        (kept.value(), kept.samples, kept.verdict),
        (Some(1.0), 1, Verdict::Pass)
    );
    // `pastor` finds the pastor on all four keyframes. `person` looks for
    // everyone but has no subject on the pastor: 1 + 2 + 2 + 0 found of
    // 2 + 3 + 3 + 1 (the keyframes list 2, 3, 3 and 1 people).
    let recall = |q| {
        report
            .metric("tracking.recall", Some(q))
            .unwrap()
            .value()
            .unwrap()
    };
    assert_eq!(recall("pastor"), 1.0);
    assert!((recall("person") - 5.0 / 9.0).abs() < 1e-12);
    assert_eq!(
        report
            .metric("switching.on_air_swaps", None)
            .unwrap()
            .value(),
        Some(0.0)
    );
    // The JSON names the verdicts, and reads back to the same report.
    let json = report.to_json();
    assert!(json.contains(r#""name": "framing.headroom.medium""#));
    assert!(json.contains(r#""verdict": "fail""#));
    assert_eq!(Report::from_json(&json), Ok(report));
}

#[test]
fn a_segment_of_the_clip_scores_the_keyframes_it_showed() {
    let labels = LabelFile::from_json(LABELS).expect("the fixture is valid");
    // Frame 30 is the first at or after 1 s (it is presented at 1.001 s), so
    // a source with `start_s = 1` plays frames 30 to 59, from PTS 0.
    let segment = segment_text(&labels, 30..60, true);
    let report = scored(&labels, &segment, 1.0);
    assert_eq!(report.coverage.ticks, 30);
    assert_eq!(report.coverage.scored_keyframes, 2);
    // It scores as the same frames do in the whole run, on keyframes 30
    // and 45: `person` finds 2 + 0 of the 3 + 1 people listed there.
    let part = scored(&labels, &segment_text(&labels, 30..60, false), 0.0);
    assert_eq!(report.coverage, part.coverage);
    assert_eq!(summary(&report), summary(&part));
    let recall = report.metric("tracking.recall", Some("person")).unwrap();
    assert_eq!((recall.value(), recall.samples), (Some(0.5), 4));
    // Read on the clip's clock instead, the segment would show keyframes 0
    // and 15, which it never showed.
    let misread = scored(&labels, &segment, 0.0);
    assert_ne!(summary(&misread), summary(&report));
    // A scope file gives the start as the source config does.
    let text = SCOPE.replacen('{', r#"{ "start_s": 1.0,"#, 1);
    let scope: Scope = serde_json::from_str(&text).expect("the scope reads");
    assert_eq!(scope.start_s, 1.0);
}

#[test]
fn a_wider_band_in_the_thresholds_file_passes_the_run() {
    let wider = DEFAULT_THRESHOLDS.replace(
        "headroom_medium = [0.07, 0.10]",
        "headroom_medium = [0.07, 0.11]",
    );
    assert_ne!(wider, DEFAULT_THRESHOLDS);
    let report = run(&wider);
    assert!(report.pass, "{:?}", report.failures().collect::<Vec<_>>());
    assert_eq!(
        report
            .metric("framing.headroom.medium", None)
            .unwrap()
            .value(),
        Some(1.0)
    );
}
