//! Discovery and tracking through the core: result staleness, the
//! discovery rate, tracking quality on the synthetic pattern, identities of
//! crossing subjects, the rule that detections vouch for existence and one
//! subject for several prompts when results arrive late.

use std::collections::VecDeque;

use super::*;

/// Ticks from a discovery request to the tick that applies its result in
/// the late-result test: about the 200 ms a detector pass takes on a Mac.
const LATE_TICKS: usize = 6;

/// Runs `ticks` ticks, holding each discovery result back until `latency`
/// ticks after the tick that requested it (the rig alone answers on the
/// next tick, a latency of 1).
fn run_late(rig: &mut Rig, ticks: usize, latency: usize) {
    let mut late: VecDeque<(usize, DiscoveryResult)> = VecDeque::new();
    for tick in 0..ticks {
        while late.front().is_some_and(|(due, _)| *due <= tick) {
            let (_, result) = late.pop_front().expect("checked above");
            rig.pending.push(result);
        }
        rig.step();
        late.extend(rig.pending.drain(..).map(|r| (tick + latency, r)));
    }
}

#[test]
fn stale_discovery_results_are_dropped() {
    let mut rig = Rig::new(config(1));
    rig.run(90);
    rig.pending.clear();
    let applied = rig.snapshot().sources[0].discovery.applied;
    let queries = [PromptQuery::new("person", "person")];
    let old = rig.pattern.frame(&cam(), 0);
    let dets: Vec<Detection> = rig.detector.detect(&old, &queries).unwrap();
    // Frame 0 was applied long ago (superseded); a frame with a new sequence
    // number but a PTS three seconds old is too old.
    let mut late = old.clone();
    late.seq = 10_000;
    for frame in [old.clone(), late] {
        rig.pending.push(DiscoveryResult {
            source: 0,
            frame,
            outcome: Ok(dets.clone()),
            latency: Duration::ZERO,
        });
    }
    rig.pending.push(DiscoveryResult {
        source: 0,
        frame: old,
        outcome: Err("model crashed".into()),
        latency: Duration::ZERO,
    });
    rig.step();
    let d = &rig.snapshot().sources[0].discovery;
    assert_eq!(d.applied, applied);
    assert_eq!(d.stale, 2);
    assert_eq!(d.errors, 1);
    assert_eq!(d.last_error.as_deref(), Some("model crashed"));
}

#[test]
fn discovery_runs_at_the_configured_rate_and_only_with_queries() {
    let mut c = config(1);
    c.discovery.rate_hz = 2.0;
    let mut rig = Rig::new(c);
    rig.run_secs(3.0);
    let d = &rig.snapshot().sources[0].discovery;
    assert_eq!(d.requests, 6);
    let mut c = config(1);
    c.queries.clear();
    let mut rig = Rig::new(c);
    rig.run_secs(2.0);
    assert_eq!(rig.snapshot().sources[0].discovery.requests, 0);
}

#[test]
fn a_single_subject_is_tracked_and_kept_in_frame() {
    // The synthetic subject crosses the frame in four seconds, faster than
    // a medium shot's pan speed cap follows; a full shot keeps up.
    let mut c = config(1);
    c.queries[0].presets = vec![PresetSpec::Size(ShotSize::Full)];
    let mut rig = Rig::new(c);
    rig.run(30);
    let (mut tracked, mut centred) = (0, 0);
    let total = 300;
    for _ in 0..total {
        let frame = rig.frame();
        rig.step();
        let truth = rig.pattern.ground_truth(frame.pts)[0].bbox;
        let subject = rig.core.store(0).unwrap().get(SubjectId(1)).unwrap();
        if subject.bbox.iou(&truth) > 0.5 {
            tracked += 1;
        }
        let crop = rig.core.catalog().get(&shot("cam/1/full")).unwrap().crop;
        let c = truth.center();
        if crop.contains_rect(&Rect::new(c.x, c.y, 0.0, 0.0)) {
            centred += 1;
        }
    }
    let ratio = |n: i32| n as f64 / total as f64;
    assert!(ratio(tracked) > 0.9, "tracked {:.2}", ratio(tracked));
    assert!(ratio(centred) > 0.9, "centred {:.2}", ratio(centred));
    assert_eq!(rig.core.store(0).unwrap().len(), 1);
}

#[test]
fn crossing_subjects_keep_their_identities_with_fast_discovery() {
    let mut c = config(3);
    c.discovery.rate_hz = 10.0;
    let mut rig = Rig::new(c);
    let (mut covered, mut n) = (0, 0);
    for _ in 0..600 {
        let frame = rig.frame();
        rig.step();
        let store = rig.core.store(0).unwrap();
        for g in rig.pattern.ground_truth(frame.pts) {
            n += 1;
            if store
                .records()
                .any(|r| r.is_tracking() && r.subject.bbox.iou(&g.bbox) > 0.5)
            {
                covered += 1;
            }
        }
    }
    let ids: Vec<u32> = rig
        .core
        .store(0)
        .unwrap()
        .records()
        .map(|r| r.subject.id.0)
        .collect();
    assert_eq!(ids, [1, 2, 3]);
    let ratio = covered as f64 / n as f64;
    assert!(ratio > 0.85, "covered {ratio:.2}");
}

#[test]
fn subjects_the_detector_no_longer_sees_are_not_kept_alive_by_the_tracker() {
    let mut c = config(1);
    c.discovery.max_undetected_s = 1.0;
    c.subjects.retire_after_s = 1.0;
    let pattern = SyntheticPattern::new(W, H, FPS, 1).unwrap();
    let mut truth = synthetic_detector(pattern, 0.9);
    // After two seconds the detector stops seeing the subject, although it
    // is still in the picture and the tracker still follows it.
    let detector = ScriptedDetector::from_fn("blind", move |frame, queries| {
        if frame.pts > Duration::from_secs(2) {
            Ok(Vec::new())
        } else {
            truth.detect(frame, queries)
        }
    });
    let mut rig = Rig::new(c).with_detector(detector);
    rig.run_secs(2.0);
    assert_eq!(rig.core.store(0).unwrap().len(), 1);
    rig.run_secs(4.5);
    let snap = rig.snapshot();
    assert!(snap.subjects.is_empty(), "{:?}", snap.subjects);
    assert!(snap.sources[0].vision.unvouched_observations > 0);
}

/// Two prompts box one static subject identically in every pass, as
/// people-first matching does, and every pass after the first boxes it 30%
/// wider while the tracker keeps the first box's size. Applied 6 ticks late,
/// each pass agrees with the tracker's newer box without aliasing on it, so
/// only the box the first prompt just applied lets the second prompt reach
/// the subject: the run keeps one subject answering both.
#[test]
fn two_prompts_on_one_static_subject_stay_one_subject_with_late_results() {
    let mut c = config(1);
    c.discovery.rate_hz = 2.0;
    c.queries.push(QuerySettings {
        id: None,
        name: "Speaker".into(),
        prompt: "speaker".into(),
        presets: vec![PresetSpec::Size(ShotSize::Medium)],
    });
    let mut rig = Rig::new(c);
    let still = rig.pattern.frame(&cam(), 0);
    let body = rig.pattern.ground_truth(still.pts)[0].bbox;
    rig.still = Some(still);
    let mut passes = 0;
    let detector = ScriptedDetector::from_fn("people-first", move |_, queries| {
        passes += 1;
        let bbox = if passes == 1 {
            body
        } else {
            Rect {
                w: body.w * 1.3,
                ..body
            }
        };
        let one = |q: &PromptQuery| Detection {
            query: q.id.clone(),
            bbox,
            score: 0.9,
        };
        Ok(queries.iter().map(one).collect())
    });
    let mut rig = rig.with_detector(detector);
    run_late(&mut rig, (20.0 * FPS) as usize, LATE_TICKS);
    let snap = rig.snapshot();
    assert!(snap.sources[0].discovery.applied >= 30);
    assert_eq!(snap.sources[0].vision.created, 1);
    let store = rig.core.store(0).unwrap();
    assert_eq!(store.len(), 1);
    let rec = store.record(SubjectId(1)).unwrap();
    assert!(rec.is_tracking());
    assert!(
        (rec.subject.bbox.w - body.w).abs() < 0.01,
        "the tracker keeps the first box's size: {:?}",
        rec.subject.bbox
    );
    assert!(rec.answers(&QueryId("person".into())) && rec.answers(&QueryId("speaker".into())));
    assert!(snap.subject_shots().count() >= 2);
    assert!(
        snap.subject_shots()
            .all(|s| s.subject == Some(SubjectId(1)))
    );
}
