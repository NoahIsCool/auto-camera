//! Discovery and tracking through the core: result staleness, the
//! discovery rate, tracking quality on the synthetic pattern, identities of
//! crossing subjects and the rule that detections vouch for existence.

use super::*;

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
    c.queries[0].presets = Some(vec![PresetSpec::Size(ShotSize::Full)]);
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
