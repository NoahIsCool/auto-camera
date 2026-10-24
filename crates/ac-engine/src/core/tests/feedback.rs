//! Prompt feedback (spec M2): every live query carries, per source, what
//! the newest applied discovery pass found for it, tagged with the frame;
//! queries a pass did not look for, stale results and errors change
//! nothing, and removing a query forgets its feedback.

use super::*;
use crate::snapshot::MatchView;

/// A detector that answers only queries whose prompt is "person", with
/// the pattern's ground truth at descending scores (0.9, 0.8, ...).
fn people_only(pattern: SyntheticPattern) -> ScriptedDetector {
    ScriptedDetector::from_fn("people-only", move |frame, queries| {
        let truth = pattern.ground_truth(frame.pts);
        Ok(queries
            .iter()
            .filter(|q| q.prompt == "person")
            .flat_map(|q| {
                truth.iter().zip(0..).map(|(g, i)| Detection {
                    query: q.id.clone(),
                    bbox: g.bbox,
                    score: 0.9 - 0.1 * i as f32,
                    extent: ac_core::Extent::Object,
                })
            })
            .collect())
    })
}

/// The feedback of query `id` in `snap`.
fn matches(snap: &EngineSnapshot, id: &str) -> Vec<MatchView> {
    snap.queries
        .iter()
        .find(|q| q.id.0 == id)
        .map(|q| q.matches.clone())
        .unwrap_or_default()
}

/// Adds a query named `name` for `prompt` with a medium framing.
fn add(rig: &mut Rig, name: &str, prompt: &str) {
    rig.apply(EngineCommand::AddQuery {
        name: name.into(),
        prompt: prompt.into(),
        presets: Some(vec![Preset::new("medium", "Medium", ShotSize::Medium)]),
        literal: false,
    })
    .unwrap();
}

/// Runs until a request is waiting to be applied on the next tick.
fn until_a_pass_is_pending(rig: &mut Rig) {
    for _ in 0..120 {
        if !rig.pending.is_empty() {
            return;
        }
        rig.step();
    }
    panic!("no discovery request in 4 s");
}

#[test]
fn each_query_reports_its_count_and_best_score_per_source() {
    let mut rig = Rig::new(config(2));
    let pattern = rig.pattern.clone();
    rig = rig.with_detector(people_only(pattern));
    assert!(matches(&rig.snapshot(), "person").is_empty(), "no pass yet");
    until_a_pass_is_pending(&mut rig);
    let looked_at = rig.pending[0].frame.clone();
    rig.step();
    let m = matches(&rig.snapshot(), "person");
    assert_eq!(
        m,
        [MatchView {
            source: cam(),
            seq: looked_at.seq,
            pts: looked_at.pts,
            count: 2,
            best: Some(0.9),
            kept: 2,
            floor: 0.35,
        }]
    );
}

/// A detector answering every query with the pattern's boxes at `scores`
/// (cycled).
fn scored(pattern: SyntheticPattern, scores: &'static [f32]) -> ScriptedDetector {
    ScriptedDetector::from_fn("scored", move |frame, queries| {
        let truth = pattern.ground_truth(frame.pts);
        Ok(queries
            .iter()
            .flat_map(|q| {
                truth
                    .iter()
                    .zip(scores.iter().cycle())
                    .map(|(g, s)| Detection {
                        query: q.id.clone(),
                        bbox: g.bbox,
                        score: *s,
                        extent: ac_core::Extent::Object,
                    })
            })
            .collect())
    })
}

#[test]
fn matches_below_the_store_floor_are_counted_apart() {
    let mut rig = Rig::new(config(2));
    let pattern = rig.pattern.clone();
    rig = rig.with_detector(scored(pattern, &[0.9, 0.2]));
    until_a_pass_is_pending(&mut rig);
    rig.step();
    let m = &matches(&rig.snapshot(), "person")[0];
    assert_eq!((m.count, m.kept, m.best, m.floor), (2, 1, Some(0.9), 0.35));
    // Nothing usable: every box is below the floor.
    let mut c = config(1);
    c.subjects.min_score = Some(0.5);
    let mut rig = Rig::new(c);
    let pattern = rig.pattern.clone();
    rig = rig.with_detector(scored(pattern, &[0.4]));
    until_a_pass_is_pending(&mut rig);
    rig.step();
    let snap = rig.snapshot();
    let m = &matches(&snap, "person")[0];
    assert_eq!((m.count, m.kept, m.floor), (1, 0, 0.5));
    assert!(
        snap.subjects.is_empty(),
        "nothing above the floor is framed"
    );
}

#[test]
fn the_newest_outcome_says_whether_the_feedback_is_current() {
    use crate::snapshot::PassOutcome;
    let mut rig = Rig::new(config(1));
    let outcome = |rig: &Rig| rig.snapshot().sources[0].discovery.last_outcome;
    assert_eq!(outcome(&rig), None);
    until_a_pass_is_pending(&mut rig);
    rig.step();
    assert_eq!(outcome(&rig), Some(PassOutcome::Applied));
    let latency = rig.snapshot().sources[0].discovery.last_latency;
    rig.pending.clear();
    rig.pending.push(DiscoveryResult {
        source: 0,
        frame: rig.frame(),
        queries: vec![QueryId("person".into())],
        outcome: Err("the sidecar is unavailable: crashed".into()),
        latency: Duration::from_secs(9),
    });
    rig.step();
    let d = rig.snapshot().sources[0].discovery.clone();
    assert_eq!(d.last_outcome, Some(PassOutcome::Failed));
    assert_eq!(d.last_latency, latency, "a failed pass is not timed");
    rig.pending.push(DiscoveryResult {
        source: 0,
        frame: rig.pattern.frame(&cam(), 0),
        queries: vec![QueryId("person".into())],
        outcome: Ok(Vec::new()),
        latency: Duration::ZERO,
    });
    rig.step();
    assert_eq!(outcome(&rig), Some(PassOutcome::Stale));
}

#[test]
fn a_prompt_with_nothing_found_reports_no_matches() {
    let mut rig = Rig::new(config(2));
    let pattern = rig.pattern.clone();
    rig = rig.with_detector(people_only(pattern));
    add(&mut rig, "Guitar", "a guitar");
    until_a_pass_is_pending(&mut rig);
    rig.step();
    let snap = rig.snapshot();
    let guitar = matches(&snap, "guitar");
    assert_eq!(guitar.len(), 1);
    assert_eq!((guitar[0].count, guitar[0].best), (0, None));
    assert_eq!(matches(&snap, "person")[0].count, 2);
}

#[test]
fn a_query_added_during_a_pass_waits_for_a_pass_that_looked_for_it() {
    let mut rig = Rig::new(config(1));
    until_a_pass_is_pending(&mut rig);
    add(&mut rig, "Late", "person");
    rig.step();
    let snap = rig.snapshot();
    assert_eq!(matches(&snap, "person").len(), 1);
    assert!(
        matches(&snap, "late").is_empty(),
        "the pass did not look for it"
    );
    until_a_pass_is_pending(&mut rig);
    rig.step();
    let late = matches(&rig.snapshot(), "late");
    assert_eq!((late.len(), late[0].count), (1, 1));
}

#[test]
fn stale_and_failed_results_keep_the_feedback() {
    let mut rig = Rig::new(config(1));
    until_a_pass_is_pending(&mut rig);
    rig.step();
    let before = matches(&rig.snapshot(), "person");
    assert_eq!(before.len(), 1);
    rig.pending.clear();
    let old = rig.pattern.frame(&cam(), 0);
    let asked = vec![QueryId("person".into())];
    rig.pending.push(DiscoveryResult {
        source: 0,
        frame: old.clone(),
        queries: asked.clone(),
        outcome: Ok(Vec::new()),
        latency: Duration::ZERO,
    });
    rig.pending.push(DiscoveryResult {
        source: 0,
        frame: rig.frame(),
        queries: asked,
        outcome: Err("model crashed".into()),
        latency: Duration::ZERO,
    });
    rig.step();
    let snap = rig.snapshot();
    assert_eq!(snap.sources[0].discovery.stale, 1);
    assert_eq!(snap.sources[0].discovery.errors, 1);
    assert_eq!(matches(&snap, "person"), before);
}

#[test]
fn removing_a_query_forgets_its_feedback() {
    let mut rig = Rig::new(config(1));
    until_a_pass_is_pending(&mut rig);
    rig.step();
    assert_eq!(matches(&rig.snapshot(), "person").len(), 1);
    rig.apply(EngineCommand::RemoveQuery {
        query: QueryId("person".into()),
    })
    .unwrap();
    add(&mut rig, "Person", "person");
    assert!(
        matches(&rig.snapshot(), "person").is_empty(),
        "the new query starts without feedback"
    );
}
