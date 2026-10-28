//! Switching on hand-built timelines: takes through cuts and completed or
//! abandoned transitions, stints, jump cuts at the threshold, dwell by
//! content, long wides, availability (stale sources included), losses on
//! program and on-air swaps.

use std::collections::BTreeSet;

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
    let heads: Vec<&str> = heads(&trace)
        .into_iter()
        .map(|h| h.unwrap().0.as_str())
        .collect();
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

/// `records` with `preview` coming in over `base` at `overlay`.
fn mixing(records: &mut [TraceRecord], preview: &str, base: &str, overlay: f64) {
    for r in records {
        *r = transition(r.clone(), preview, Some(base), overlay);
    }
}

#[test]
fn an_abandoned_transition_is_neither_a_take_nor_a_stint() {
    // The medium on program throughout. At 30-33 a T-bar move brings the
    // close-up in a little (overlay 0.05), then goes back to the start: at
    // 34 the medium is still program. The close-up never reached program.
    let close = shot("cam/1/close", Some(1), Rect::new(0.35, 0.185, 0.3, 0.3));
    let mut trace = timeline(60, |_| "cam/1/medium");
    for r in &mut trace {
        r.shots.push(close.clone());
    }
    mixing(&mut trace[30..34], "cam/1/close", "cam/1/medium", 0.05);
    trace[34].switcher.preview = Some(ShotId("cam/1/close".into()));
    assert!(heads(&trace).iter().all(|h| h.unwrap().0 == "cam/1/medium"));
    assert!(takes(&trace).is_empty());
    let all = stints(&trace);
    assert_eq!(
        all.iter().map(|s| (s.first, s.last)).collect::<Vec<_>>(),
        [(0, 59)]
    );
    // The medium and the close-up overlap with IoU 0.5625, so the take and
    // the take back would each have been a jump cut, and the 0.4 s of the
    // close-up a dwell violation.
    assert!(MEDIUM.iou(&close.crop) > 0.4);
    let r = switching(&trace, Content::Talk);
    for name in ["switching.jump_cuts", "switching.dwell_violations"] {
        let m = r.metric(name, None).unwrap();
        assert_eq!((m.value(), m.verdict), (Some(0.0), Verdict::Pass), "{name}");
    }
}

#[test]
fn a_take_queued_behind_an_abandoned_transition_is_the_one_take() {
    // Wide on program. At 30-32 a T-bar move towards the close-up is
    // abandoned, and the take queued meanwhile runs at once: a mix to the
    // medium at 33-35, which completes (the medium is program at 36). The
    // trace ends during a second mix, to the close-up at 58-59, which is
    // taken to complete.
    let close = shot("cam/1/close", Some(1), Rect::new(0.35, 0.185, 0.3, 0.3));
    let mut trace = timeline(60, |f| if f < 36 { "cam/wide" } else { "cam/1/medium" });
    for r in &mut trace {
        r.shots.push(close.clone());
    }
    mixing(&mut trace[30..33], "cam/1/close", "cam/wide", 0.2);
    mixing(&mut trace[33..36], "cam/1/medium", "cam/wide", 0.5);
    mixing(&mut trace[58..60], "cam/1/close", "cam/1/medium", 0.5);
    let all = takes(&trace);
    let found: Vec<(usize, &str, &str)> = all
        .iter()
        .map(|t| {
            (
                t.record,
                t.from.as_ref().unwrap().0.as_str(),
                t.to.0.as_str(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (33, "cam/wide", "cam/1/medium"),
            (58, "cam/1/medium", "cam/1/close")
        ]
    );
}

#[test]
fn a_take_queued_behind_an_abandoned_transition_can_fire_as_a_cut() {
    // The medium on program. At 30-33 a T-bar move towards the close-up
    // ends, and the take of the wide queued meanwhile fires as a cut (the
    // selected transition): at 34 the wide is program. Abandoned, the move
    // leaves the medium on program until the cut sends it to preview;
    // completed, it puts the close-up there, and the cut sends that to
    // preview instead.
    let close = shot("cam/1/close", Some(1), Rect::new(0.35, 0.185, 0.3, 0.3));
    let run = |preview: &str| {
        let mut trace = timeline(60, |f| if f < 34 { "cam/1/medium" } else { "cam/wide" });
        for r in &mut trace {
            r.shots.push(close.clone());
        }
        mixing(&mut trace[30..34], "cam/1/close", "cam/1/medium", 0.5);
        for r in &mut trace[34..] {
            r.switcher.preview = Some(ShotId(preview.into()));
        }
        trace
    };
    let found = |trace: &[TraceRecord]| -> Vec<(usize, String)> {
        takes(trace)
            .into_iter()
            .map(|t| (t.record, t.to.0))
            .collect()
    };
    let abandoned = run("cam/1/medium");
    assert_eq!(found(&abandoned), [(34, "cam/wide".to_owned())]);
    // The close-up never reached program: no jump cut onto it, and no
    // 0.4 s stint of it.
    let r = switching(&abandoned, Content::Talk);
    for name in ["switching.jump_cuts", "switching.dwell_violations"] {
        let m = r.metric(name, None).unwrap();
        assert_eq!((m.value(), m.verdict), (Some(0.0), Verdict::Pass), "{name}");
    }
    let completed = run("cam/1/close");
    assert_eq!(
        found(&completed),
        [(30, "cam/1/close".to_owned()), (34, "cam/wide".to_owned())]
    );
}

#[test]
fn availability_needs_the_shot_a_fresh_frame_and_a_tracking_subject() {
    let mut r = at(0, "cam/1/medium");
    let medium = ShotId("cam/1/medium".into());
    let wide_id = ShotId("cam/wide".into());
    let fresh = BTreeSet::new();
    assert_eq!(availability(&r, &fresh, &medium), Availability::Available);
    assert_eq!(
        availability(&r, &fresh, &ShotId("cam/9/close".into())),
        Availability::Unavailable
    );
    // A stale source's shots are unavailable, wide and subject shot alike.
    let stale = BTreeSet::from([SourceId(CAM.into())]);
    assert_eq!(availability(&r, &stale, &medium), Availability::Unavailable);
    assert_eq!(
        availability(&r, &stale, &wide_id),
        Availability::Unavailable
    );
    r.sources[0].subjects[0].state = ac_core::TrackState::Lost;
    assert_eq!(availability(&r, &fresh, &medium), Availability::Lost);
    r.sources[0].subjects.clear();
    assert_eq!(availability(&r, &fresh, &medium), Availability::Unavailable);
    assert_eq!(availability(&r, &fresh, &wide_id), Availability::Available);
    r.sources[0].frame = None;
    assert_eq!(
        availability(&r, &fresh, &wide_id),
        Availability::Unavailable
    );
}

/// `trace` with `cam`'s frame frozen from record `from` to record `to`
/// (exclusive): the frame of record `from - 1` stays its newest.
fn frozen(mut trace: Vec<TraceRecord>, from: usize, to: usize) -> Vec<TraceRecord> {
    let held = trace[from - 1].sources[0].frame;
    for r in &mut trace[from..to] {
        r.sources[0].frame = held;
    }
    trace
}

/// `r` with a second source, `side`, delivering a new frame, and its wide.
fn with_side(mut r: TraceRecord) -> TraceRecord {
    let mut source = r.sources[0].clone();
    source.id = SourceId("side".into());
    source.subjects.clear();
    r.sources.push(source);
    let mut wide = wide();
    wide.id = ShotId("side/wide".into());
    wide.source = SourceId("side".into());
    r.shots.push(wide);
    r
}

#[test]
fn a_source_is_stale_once_no_new_frame_comes_for_the_window() {
    // `cam` delivers frames 0-9, then frame 9 stands still to 29, and new
    // frames come again from 30. Frame 9 came at 0.9 s, so the source is
    // stale from 2.0 s (1.1 s without a new frame), not at 1.9 s (1 s,
    // the window itself). `side` delivers all along.
    let both = timeline(40, |_| "cam/wide").into_iter().map(with_side);
    let trace = frozen(both.collect(), 10, 30);
    let stale = stale_sources(&trace, 1.0);
    let cam: BTreeSet<SourceId> = BTreeSet::from([SourceId(CAM.into())]);
    for (f, s) in stale.iter().enumerate() {
        let expected = if (20..30).contains(&f) {
            cam.clone()
        } else {
            BTreeSet::new()
        };
        assert_eq!(s, &expected, "record {f}");
    }
    // The trace cannot see before its first record: an old frame there
    // counts as new, and a source with no frame is not stale (it is
    // unavailable for having none).
    let mut late_start = timeline(15, |_| "cam/wide");
    for r in &mut late_start {
        r.sources[0].frame = Some(FrameTag {
            seq: 3,
            pts: pts(3),
        });
    }
    let stale = stale_sources(&late_start, 1.0);
    assert!(stale[..11].iter().all(BTreeSet::is_empty));
    assert!(stale[11..].iter().all(|s| s == &cam));
    let mut blind = timeline(30, |_| "cam/wide");
    for r in &mut blind {
        r.sources[0].frame = None;
    }
    assert!(stale_sources(&blind, 1.0).iter().all(BTreeSet::is_empty));
    assert!(stale_sources(&[], 1.0).is_empty());
}

/// Records 0-49 of `cam` and `side`, both delivering: `cam`'s wide on
/// program to 34, `side`'s wide (a fallback) at 35-39, then `cam`'s medium.
fn fallback_timeline() -> Vec<TraceRecord> {
    timeline(50, |f| if f < 40 { "cam/wide" } else { "cam/1/medium" })
        .into_iter()
        .enumerate()
        .map(|(f, r)| {
            let mut r = with_side(r);
            if (35..40).contains(&f) {
                r.switcher.program = Some(ShotId("side/wide".into()));
            }
            r
        })
        .collect()
}

#[test]
fn a_frozen_source_on_program_is_a_loss_until_the_fallback() {
    // `cam`'s frame stands still from 10 (frame 9, at 0.9 s), so its shots
    // are unavailable from 20. Its wide stays on program until a fallback
    // cut to `side`'s wide at 35: a loss of 1.5 s, longer than the 1 s
    // allowed. Then, at 40, a cut back to `cam`'s medium, still frozen.
    let trace = frozen(fallback_timeline(), 10, 50);
    let stale = stale_sources(&trace, 1.0);
    let found = losses(&trace, &stale);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!((found[0].0 - 1.5).abs() < 1e-9 && found[0].1, "{found:?}");
    assert!((found[1].0 - 0.9).abs() < 1e-9 && !found[1].1, "{found:?}");
    let r = switching(&trace, Content::Talk);
    let fall_back = r.metric("switching.time_to_fall_back", None).unwrap();
    assert!((fall_back.value().unwrap() - 1.5).abs() < 1e-9);
    assert_eq!(fall_back.verdict, Verdict::Fail);
    assert_eq!(fall_back.details["unresolved"], 1.0);
    let total = r
        .metric("switching.time_unavailable_on_program", None)
        .unwrap();
    assert!((total.value().unwrap() - 2.4).abs() < 1e-9);
    // The fallback is onto a live source; the take back is onto a frozen
    // one, which the engine would have refused.
    let onto = r.metric("switching.takes_onto_unavailable", None).unwrap();
    assert_eq!(
        (onto.value(), onto.samples, onto.verdict),
        (Some(1.0), 2, Verdict::Fail)
    );
    // With frames coming throughout, nothing was lost and both takes are
    // sound.
    let r = switching(&fallback_timeline(), Content::Talk);
    assert_eq!(
        value(&r, "switching.time_unavailable_on_program"),
        Some(0.0)
    );
    assert_eq!(value(&r, "switching.takes_onto_unavailable"), Some(0.0));
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
    let found = losses(&trace, &stale_sources(&trace, 1.0));
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
