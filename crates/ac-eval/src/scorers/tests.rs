//! End-to-end tests of [`score`]: defined results on empty and one-frame
//! traces, a perfect run with no violation, a run with one planted ID
//! swap, one jump cut and one dwell violation, and the inputs it refuses.
//! The metrics' own arithmetic is tested next to each scorer.

use ac_core::{QueryId, Rect, ShotSize, SourceId};

use super::testkit::*;
use super::*;
use crate::report::{Measure, Verdict};
use crate::trace::TraceRecord;

/// Person 1, standing still: crown 0.2, chin 0.3, shoulders 0.35, waist
/// 0.55, box centred at x = 0.5.
fn a() -> crate::labels::PersonBox {
    lined(
        person(1, Rect::new(0.4, 0.2, 0.2, 0.7)),
        0.2,
        0.3,
        0.35,
        0.55,
    )
}

/// Person 2, standing still at the left, with no lines.
fn b() -> crate::labels::PersonBox {
    person(2, Rect::new(0.05, 0.3, 0.15, 0.6))
}

/// A medium shot of person 1: 0.4 high with the crown 0.08 * 0.4 = 0.032
/// below the top (headroom 0.08, inside the medium band 0.07 to 0.10), so
/// the top edge is at 0.168 and the bottom at 0.568; centred on x = 0.5.
const MEDIUM: Rect = Rect::new(0.3, 0.168, 0.4, 0.4);

/// A close-up of person 1, inside the medium shot: IoU with it
/// 0.09 / 0.16 = 0.5625, above 0.4.
const CLOSE: Rect = Rect::new(0.35, 0.185, 0.3, 0.3);

/// Labels for frames `0..200` (20 s), both people still, a keyframe every
/// 10 frames (1 s).
fn still_labels() -> crate::labels::LabelFile {
    labels(
        2,
        (0..20).map(|i| keyframe(i * 10, vec![a(), b()])).collect(),
    )
}

/// 200 records, one per frame, with subject 1 on person 1 and subject 2
/// on person 2, the wide, subject 1's medium and close shots, and
/// `program(frame)` on program.
fn run(program: impl Fn(u64) -> &'static str) -> Vec<TraceRecord> {
    (0..200)
        .map(|f| {
            record(
                f,
                vec![subject(1, a().bbox), subject(2, b().bbox)],
                vec![
                    wide(),
                    shot("cam/1/medium", Some(1), MEDIUM),
                    shot("cam/1/close", Some(1), CLOSE),
                ],
                Some(program(f)),
            )
        })
        .collect()
}

/// Asserts that every number in `report` is finite and that its JSON
/// reads back to it.
#[track_caller]
fn assert_defined(report: &Report) {
    for m in &report.metrics {
        if let Some(v) = m.value() {
            assert!(v.is_finite(), "{}: {v}", m.name);
        }
        assert!(m.details.values().all(|v| v.is_finite()), "{}", m.name);
    }
    let json = report.to_json();
    assert!(!json.contains("NaN") && !json.contains("null"), "{json}");
    assert!(!json.contains("-0.0"), "a negative zero: {json}");
    assert_eq!(Report::from_json(&json).as_ref(), Ok(report));
}

#[test]
fn an_empty_trace_gives_defined_results_and_no_failure() {
    let mut s = scope();
    s.queries.push(QueryBinding {
        query: QueryId("person".into()),
        role: None,
    });
    let r = score(&still_labels(), &[], &s, &Thresholds::default()).unwrap();
    assert_defined(&r);
    assert!(r.pass);
    assert_eq!(r.summary.fail, 0);
    assert_eq!((r.coverage.ticks, r.coverage.scored_keyframes), (0, 0));
    // Shares and peaks over nothing are not measurable, with a reason ...
    for name in [
        "framing.head_torso_inside",
        "motion.peak_acceleration",
        "tracking.recall",
    ] {
        let m = r
            .metric(name, None)
            .or_else(|| r.metric(name, Some("person")))
            .unwrap();
        assert!(
            matches!(&m.measure, Measure::NotMeasurable(why) if !why.is_empty()),
            "{name}"
        );
        assert_eq!(m.verdict, Verdict::NotMeasurable);
    }
    // ... while counts and waits are zero.
    for name in [
        "switching.jump_cuts",
        "switching.dwell_violations",
        "switching.on_air_swaps",
        "switching.time_to_fall_back",
        "tracking.id_switches",
    ] {
        assert_eq!(value(&r, name), Some(0.0), "{name}");
    }
}

#[test]
fn an_empty_trace_over_empty_labels_scores_too() {
    let r = score(
        &labels(0, Vec::new()),
        &[],
        &scope(),
        &Thresholds::default(),
    )
    .unwrap();
    assert_defined(&r);
    assert!(r.pass);
    // No query is bound or seen, so there are no tracking metrics.
    assert!(r.metrics.iter().all(|m| !m.name.starts_with("tracking.")));
}

#[test]
fn a_single_frame_trace_gives_defined_results() {
    let labels = labels(1, vec![keyframe(0, vec![a()])]);
    let trace = [record(
        0,
        vec![subject(1, a().bbox)],
        vec![wide(), shot("cam/1/medium", Some(1), MEDIUM)],
        Some("cam/1/medium"),
    )];
    let r = report(&labels, &trace);
    assert_defined(&r);
    assert_eq!(r.coverage.scored_keyframes, 1);
    // One on-air frame is enough for framing ...
    assert_eq!(value(&r, "framing.head_torso_inside"), Some(1.0));
    assert_eq!(value(&r, "framing.headroom.medium"), Some(1.0));
    assert_eq!(value(&r, "tracking.recall"), Some(1.0));
    assert_eq!(value(&r, "tracking.idf1"), Some(1.0));
    // ... but not for motion, which needs two ticks or more.
    for name in [
        "motion.peak_pan_speed",
        "motion.peak_acceleration",
        "motion.peak_jerk",
    ] {
        assert_eq!(value(&r, name), None, "{name}");
    }
    assert_eq!(value(&r, "switching.jump_cuts"), Some(0.0));
    assert!(r.pass);
}

#[test]
fn a_single_frame_with_a_lost_shot_on_program_waits_nothing() {
    let labels = labels(1, vec![keyframe(0, vec![a()])]);
    let trace = [record(
        0,
        vec![lost(subject(1, a().bbox))],
        vec![shot("cam/1/medium", Some(1), MEDIUM)],
        Some("cam/1/medium"),
    )];
    let r = report(&labels, &trace);
    assert_defined(&r);
    let fall_back = r.metric("switching.time_to_fall_back", None).unwrap();
    assert_eq!(fall_back.value(), Some(0.0));
    assert_eq!(fall_back.details["unresolved"], 1.0);
    // A lost subject holds a box but has not found anyone.
    assert_eq!(value(&r, "tracking.recall"), Some(0.0));
}

#[test]
fn a_perfect_trace_scores_no_violation() {
    // Wide for 5 s, the medium shot for 10 s, the wide again for 5 s.
    let trace = run(|f| match f {
        50..150 => "cam/1/medium",
        _ => "cam/wide",
    });
    let r = report(&still_labels(), &trace);
    assert_defined(&r);
    assert!(r.pass, "{:?}", r.failures().collect::<Vec<_>>());
    assert_eq!(r.summary.fail, 0);
    assert_eq!(r.coverage.scored_keyframes, 20);
    for name in [
        "switching.jump_cuts",
        "switching.dwell_violations",
        "switching.long_wides",
        "switching.on_air_swaps",
        "switching.takes_onto_unavailable",
        "switching.time_unavailable_on_program",
        "switching.time_to_fall_back",
        "tracking.id_switches",
        "tracking.false_subjects_per_frame",
        "tracking.time_to_acquire",
        "tracking.time_to_reacquire",
        "motion.peak_acceleration",
        "motion.peak_jerk",
        "motion.peak_pan_speed",
        "motion.peak_zoom_rate",
        "motion.jitter_at_rest",
        "motion.settle_time",
    ] {
        assert_eq!(value(&r, name), Some(0.0), "{name}");
    }
    // The medium shot is on air on the keyframes of frames 50 to 140: ten
    // frames, every one keeping person 1's head and torso in band.
    let kept = r.metric("framing.head_torso_inside", None).unwrap();
    assert_eq!((kept.value(), kept.samples), (Some(1.0), 10));
    assert_eq!(value(&r, "framing.headroom.medium"), Some(1.0));
    assert_eq!(value(&r, "framing.subject_containment"), Some(1.0));
    assert_eq!(value(&r, "tracking.recall"), Some(1.0));
    assert_eq!(value(&r, "tracking.idf1"), Some(1.0));
}

#[test]
fn one_planted_swap_jump_cut_and_dwell_violation_count_once_each() {
    // wide 0-49 | medium 50-69 (2 s: a dwell violation) | wide 70-109 |
    // medium 110-149 (subject 1 jumps to person 2 at frame 120: the swap) |
    // close 150-189 (medium to close of one source, IoU 0.5625: the jump
    // cut) | wide 190-199.
    let mut trace = run(|f| match f {
        50..70 | 110..150 => "cam/1/medium",
        150..190 => "cam/1/close",
        _ => "cam/wide",
    });
    for r in &mut trace[120..] {
        // Subject 1's box jumps to person 2, beside subject 2, and a new
        // subject 3 picks up person 1. CLEAR MOT counts one switch, for
        // person 1 (subject 1, then 3); person 2 keeps subject 2.
        r.sources[0].subjects = vec![
            subject(1, b().bbox),
            subject(2, b().bbox),
            subject(3, a().bbox),
        ];
    }
    let r = report(&still_labels(), &trace);
    assert_defined(&r);
    assert_eq!(value(&r, "tracking.id_switches"), Some(1.0));
    assert_eq!(value(&r, "switching.on_air_swaps"), Some(1.0));
    assert_eq!(value(&r, "switching.jump_cuts"), Some(1.0));
    assert_eq!(value(&r, "switching.dwell_violations"), Some(1.0));
    let failed: Vec<&str> = r.failures().map(|m| m.name.as_str()).collect();
    assert_eq!(
        failed,
        [
            "switching.jump_cuts",
            "switching.dwell_violations",
            "switching.on_air_swaps"
        ]
    );
    // Subject 1 matches nobody from frame 120 on (person 2 keeps subject
    // 2): eight false subjects over 20 keyframes.
    assert_eq!(value(&r, "tracking.false_subjects_per_frame"), Some(0.4));
    assert_eq!(value(&r, "tracking.recall"), Some(1.0));
    assert!(!r.pass);
}

#[test]
fn bound_queries_look_for_their_role_only() {
    let labels = tagged(still_labels(), 1, "pastor");
    let mut s = scope();
    s.queries.push(QueryBinding {
        query: QueryId("person".into()),
        role: Some(crate::labels::RoleTag("pastor".into())),
    });
    let trace = run(|_| "cam/wide");
    let r = score(&labels, &trace, &s, &Thresholds::default()).unwrap();
    // Only person 1 is a target: subject 2, on person 2, is false on all
    // 20 keyframes.
    assert_eq!(value(&r, "tracking.recall"), Some(1.0));
    assert_eq!(value(&r, "tracking.false_subjects_per_frame"), Some(1.0));
}

#[test]
fn shot_sizes_come_from_the_preset_name_or_the_scope() {
    let mut s = scope();
    let tight = shot("cam/1/tight", Some(1), MEDIUM);
    assert_eq!(s.shot_size(&wide()), Some(ShotSize::Wide));
    assert_eq!(
        s.shot_size(&shot("cam/1/medium_close", Some(1), MEDIUM)),
        Some(ShotSize::MediumClose)
    );
    assert_eq!(s.shot_size(&tight), None);
    s.preset_sizes.insert("tight".into(), ShotSize::Close);
    assert_eq!(s.shot_size(&tight), Some(ShotSize::Close));
    // A subject shot is never a wide, whatever its preset is called.
    assert_eq!(s.shot_size(&shot("cam/1/wide", Some(1), MEDIUM)), None);
}

#[test]
fn invalid_input_is_refused_with_its_reason() {
    let trace = run(|_| "cam/wide");
    let mut bad = still_labels();
    bad.keyframes[1].frame = 0;
    assert!(matches!(
        score(&bad, &trace, &scope(), &Thresholds::default()),
        Err(ScoreError::Labels(_))
    ));
    let mut t = Thresholds::default();
    t.tracking.match_iou = 0.0;
    let err = score(&still_labels(), &trace, &scope(), &t).unwrap_err();
    assert!(matches!(&err, ScoreError::Thresholds(e) if e.paths() == ["tracking.match_iou"]));
    let other = Scope::new(SourceId("side".into()));
    assert_eq!(
        score(&still_labels(), &trace, &other, &Thresholds::default()),
        Err(ScoreError::UnknownSource(SourceId("side".into())))
    );
    let mut twice = scope();
    let binding = QueryBinding {
        query: QueryId("person".into()),
        role: None,
    };
    twice.queries = vec![binding.clone(), binding];
    assert_eq!(
        score(&still_labels(), &trace, &twice, &Thresholds::default()),
        Err(ScoreError::DuplicateQuery(QueryId("person".into())))
    );
}

#[test]
fn keyframes_no_tick_shows_are_left_out() {
    // Ticks show only even frames, so the keyframes at odd frames (every
    // other one: frames 5, 15, ...) are not scored.
    let labels = labels(1, (0..10).map(|i| keyframe(i * 5, vec![a()])).collect());
    let trace: Vec<TraceRecord> = (0..25)
        .map(|i| {
            record(
                i * 2,
                vec![subject(1, a().bbox)],
                vec![wide()],
                Some("cam/wide"),
            )
        })
        .collect();
    let r = report(&labels, &trace);
    assert_eq!((r.coverage.keyframes, r.coverage.scored_keyframes), (10, 5));
    let recall = r.metric("tracking.recall", Some("person")).unwrap();
    assert_eq!((recall.value(), recall.samples), (Some(1.0), 5));
}
