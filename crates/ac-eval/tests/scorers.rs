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
    let stage = SourceId("stage".into());
    (0..labels.clip.frame_count)
        .map(|frame| {
            let pts = Duration::from_nanos(labels.clip.fps.pts_ns(frame).unwrap());
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
                tick: frame + 1,
                pts,
                late: false,
                skipped: 0,
                sources: vec![SourceTrace {
                    id: stage.clone(),
                    frame: Some(FrameTag { seq: frame, pts }),
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
