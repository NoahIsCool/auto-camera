//! Switching on hand-built timelines: takes through cuts and transitions,
//! stints, jump cuts at the threshold, dwell by content, long wides,
//! availability, losses on program and on-air swaps.

use ac_core::{Rect, SourceId};

use super::*;
use crate::report::Verdict;
use crate::scorers::testkit::*;
// The test kit's shot builder, not the join's lookup.
use crate::scorers::testkit::shot;
use crate::scorers::{Content, score};
use crate::thresholds::Thresholds;

/// Person 1's box.
const A: Rect = Rect::new(0.4, 0.2, 0.2, 0.7);
/// Person 2's box.
const B: Rect = Rect::new(0.05, 0.3, 0.15, 0.6);
/// A medium shot of person 1.
const MEDIUM: Rect = Rect::new(0.3, 0.168, 0.4, 0.4);

/// Record `f` with subject 1 on person 1, the wide and subject 1's
/// medium shot, and `program` on program.
fn at(f: u64, program: &str) -> TraceRecord {
    record(
        f,
        vec![subject(1, A)],
        vec![wide(), shot("cam/1/medium", Some(1), MEDIUM)],
        Some(program),
    )
}

/// Records `0..n` with `program(f)` on program.
fn timeline(n: u64, program: impl Fn(u64) -> &'static str) -> Vec<TraceRecord> {
    (0..n).map(|f| at(f, program(f))).collect()
}

/// The switching metrics of `trace` scored for `content`.
fn switching(trace: &[TraceRecord], content: Content) -> crate::report::Report {
    let mut s = scope();
    s.content = content;
    score(&labels(1, Vec::new()), trace, &s, &Thresholds::default()).unwrap()
}

#[test]
fn takes_follow_the_shot_program_is_headed_for() {
    // Wide 0-2, cut to the medium at 3, a mix to the close-up from 5 (the
    // medium stays program until the mix ends at 7), the close-up to 9.
    let close = shot("cam/1/close", Some(1), Rect::new(0.35, 0.185, 0.3, 0.3));
    let mut trace: Vec<TraceRecord> = (0..10)
        .map(|f| {
            let program = match f {
                0..3 => "cam/wide",
                3..7 => "cam/1/medium",
                _ => "cam/1/close",
            };
            let mut r = at(f, program);
            r.shots.push(close.clone());
            r
        })
        .collect();
    trace[5] = transition(trace[5].clone(), "cam/1/close", Some("cam/1/medium"), 0.5);
    trace[6] = transition(trace[6].clone(), "cam/1/close", Some("cam/1/medium"), 0.9);
    let heads: Vec<&str> = trace.iter().map(|r| head(r).unwrap().0.as_str()).collect();
    assert_eq!(heads[4..7], ["cam/1/medium", "cam/1/close", "cam/1/close"]);
    let found = takes(&trace);
    let summary: Vec<(usize, Option<&str>, &str)> = found
        .iter()
        .map(|t| {
            (
                t.record,
                t.from.as_ref().map(|f| f.0.as_str()),
                t.to.0.as_str(),
            )
        })
        .collect();
    // The mix's end, at 7, is not a take of its own.
    assert_eq!(
        summary,
        [
            (3, Some("cam/wide"), "cam/1/medium"),
            (5, Some("cam/1/medium"), "cam/1/close")
        ]
    );
    let stints = stints(&trace);
    let spans: Vec<(usize, usize, bool, bool)> = stints
        .iter()
        .map(|s| (s.first, s.last, s.taken, s.open))
        .collect();
    assert_eq!(
        spans,
        [
            (0, 2, false, false),
            (3, 4, true, false),
            (5, 9, true, true)
        ]
    );
    // 0 to 0.3 s, 0.3 to 0.5 s, and 0.5 s to the last tick at 0.9 s.
    let seconds: Vec<f64> = stints.iter().map(|s| s.seconds).collect();
    assert!(
        seconds
            .iter()
            .zip([0.3, 0.2, 0.4])
            .all(|(s, e)| (s - e).abs() < 1e-9),
        "{seconds:?}"
    );
    // On air during the mix: the medium under the close-up.
    let on: Vec<&str> = crate::scorers::on_air(&trace[5])
        .iter()
        .map(|s| s.0.as_str())
        .collect();
    assert_eq!(on, ["cam/1/medium", "cam/1/close"]);
}

#[test]
fn a_jump_cut_is_an_iou_above_the_threshold_on_one_source() {
    // A 0.5 by 0.5 crop inside a 0.625 by 1 one: IoU 0.25 / 0.625 = 0.4
    // exactly, which is not above 0.4.
    let small = shot("cam/1/close", Some(1), Rect::new(0.0, 0.0, 0.5, 0.5));
    let large = shot("cam/1/medium", Some(1), Rect::new(0.0, 0.0, 0.625, 1.0));
    assert_eq!(small.crop.iou(&large.crop), 0.4);
    assert!(!is_jump_cut(&small, &large, 0.4));
    assert!(is_jump_cut(&small, &large, 0.39));
    // The same crops on two sources are no jump cut.
    let mut elsewhere = large.clone();
    elsewhere.source = SourceId("side".into());
    assert!(!is_jump_cut(&small, &elsewhere, 0.0));
}

#[test]
fn dwell_violations_depend_on_the_content_and_spare_fallbacks() {
    // Wide 0-9, medium 10-29 (2 s), wide 30-69, medium 70-79 (1 s, its
    // subject lost on its last tick, so the take away is a fallback), wide
    // 80-99.
    let mut trace = timeline(100, |f| match f {
        10..30 | 70..80 => "cam/1/medium",
        _ => "cam/wide",
    });
    trace[79].sources[0].subjects[0].state = ac_core::TrackState::Lost;
    // Talk: 2 s is under 3 s. The first wide was not taken, the last one
    // is still on air, and the 1 s medium was left in a fallback.
    let talk = switching(&trace, Content::Talk);
    let m = talk.metric("switching.dwell_violations", None).unwrap();
    assert_eq!(
        (m.value(), m.samples, m.verdict),
        (Some(1.0), 2, Verdict::Fail)
    );
    assert_eq!(m.details["min_dwell_s"], 3.0);
    // Music: 2 s is the minimum itself, which is allowed.
    let music = switching(&trace, Content::Music);
    assert_eq!(
        music
            .metric("switching.dwell_violations", None)
            .unwrap()
            .value(),
        Some(0.0)
    );
}

#[test]
fn wides_held_past_the_limit_count_open_or_not() {
    // Wide 0-119 (12 s), medium 120-129, wide 130-229 (10 s, the limit
    // itself), medium 230-239, wide 240-399 (still on air at the end,
    // 15.9 s so far).
    let trace = timeline(400, |f| match f {
        120..130 | 230..240 => "cam/1/medium",
        _ => "cam/wide",
    });
    let r = switching(&trace, Content::Talk);
    let m = r.metric("switching.long_wides", None).unwrap();
    assert_eq!((m.value(), m.samples), (Some(2.0), 3));
}

#[test]
fn availability_needs_the_shot_a_frame_and_a_tracking_subject() {
    let mut r = at(0, "cam/1/medium");
    let medium = ShotId("cam/1/medium".into());
    let wide_id = ShotId("cam/wide".into());
    assert_eq!(availability(&r, &medium), Availability::Available);
    assert_eq!(
        availability(&r, &ShotId("cam/9/close".into())),
        Availability::Unavailable
    );
    r.sources[0].subjects[0].state = ac_core::TrackState::Lost;
    assert_eq!(availability(&r, &medium), Availability::Lost);
    r.sources[0].subjects.clear();
    assert_eq!(availability(&r, &medium), Availability::Unavailable);
    assert_eq!(availability(&r, &wide_id), Availability::Available);
    r.sources[0].frame = None;
    assert_eq!(availability(&r, &wide_id), Availability::Unavailable);
}

#[test]
fn losses_on_program_last_until_an_available_shot_is_on_air() {
    // The medium on program throughout but for a fallback cut to the wide
    // at 15. Its subject is lost at 10-14 (0.5 s, then the cut), at 30-39
    // (found again at 40: 1 s on air lost), and from 50 to the end (59:
    // 0.9 s, unresolved).
    let mut trace = timeline(60, |f| if f == 15 { "cam/wide" } else { "cam/1/medium" });
    for f in (10..15).chain(30..40).chain(50..60) {
        trace[f].sources[0].subjects[0].state = ac_core::TrackState::Lost;
    }
    let found = losses(&trace);
    let expected = [(0.5, true), (1.0, true), (0.9, false)];
    assert_eq!(found.len(), 3);
    assert!(
        found
            .iter()
            .zip(expected)
            .all(|(f, e)| (f.0 - e.0).abs() < 1e-9 && f.1 == e.1),
        "{found:?}"
    );
    let r = switching(&trace, Content::Talk);
    let fall_back = r.metric("switching.time_to_fall_back", None).unwrap();
    assert!((fall_back.value().unwrap() - 1.0).abs() < 1e-9);
    assert_eq!(fall_back.verdict, Verdict::Pass);
    assert_eq!(fall_back.details["unresolved"], 1.0);
    let total = r
        .metric("switching.time_unavailable_on_program", None)
        .unwrap();
    assert!((total.value().unwrap() - 2.4).abs() < 1e-9);
    assert_eq!(total.verdict, Verdict::Unchecked);
    // The take back to the medium at 16 finds its subject tracking again;
    // had it still been lost, the take would count.
    assert_eq!(
        r.metric("switching.takes_onto_unavailable", None)
            .unwrap()
            .value(),
        Some(0.0)
    );
    trace[16].sources[0].subjects[0].state = ac_core::TrackState::Lost;
    let r = switching(&trace, Content::Talk);
    assert_eq!(
        r.metric("switching.takes_onto_unavailable", None)
            .unwrap()
            .value(),
        Some(1.0)
    );
}

#[test]
fn on_air_swaps_count_identity_changes_while_on_air() {
    // Keyframes every 10 frames. Subject 1's box is on person 1, then on
    // person 2 from frame 20: with its medium shot on air throughout, one
    // swap. A keyframe where the box matches nobody (frame 10) keeps the
    // last identity rather than counting twice.
    let labels = labels(
        2,
        (0..4)
            .map(|i| keyframe(i * 10, vec![person(1, A), person(2, B)]))
            .collect(),
    );
    let boxes = [A, Rect::new(0.8, 0.0, 0.1, 0.1), B, B];
    let trace: Vec<TraceRecord> = (0..4)
        .map(|i| {
            record(
                i * 10,
                vec![subject(1, boxes[i as usize])],
                vec![wide(), shot("cam/1/medium", Some(1), MEDIUM)],
                Some("cam/1/medium"),
            )
        })
        .collect();
    let r = report(&labels, &trace);
    assert_eq!(
        r.metric("switching.on_air_swaps", None).unwrap().value(),
        Some(1.0)
    );
    // The same change with the wide on air at frame 10, between: the shot
    // left the air and came back with its new person, which no viewer saw
    // switch.
    let mut off = trace.clone();
    off[1].switcher.program = Some(ShotId("cam/wide".into()));
    let r = report(&labels, &off);
    assert_eq!(
        r.metric("switching.on_air_swaps", None).unwrap().value(),
        Some(0.0)
    );
}
