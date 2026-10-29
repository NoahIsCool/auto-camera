//! Scenario tests for prompt priority: with equal quality the shot whose
//! prompt the operator ranked higher is suggested and taken, a priority lead
//! obeys the same margin and hold as any other lead, and program keeps its
//! priority against challengers. With every candidate at priority 0 (one
//! priority everywhere) the other scenario tests run unchanged.

use std::time::Duration;

use super::test_support::{Sim, director, id, input, subject, tick_time};
use crate::{ContentProfile, DirectorMode, Reason, ShotCandidate};

/// [`subject`] with its prompt `levels` above the lowest priority.
fn ranked(name: &str, source: &str, quality: f32, levels: u8) -> ShotCandidate {
    ShotCandidate {
        priority: levels,
        ..subject(name, source, quality)
    }
}

#[test]
fn equal_quality_suggests_the_higher_priority_shot() {
    let on_air = id("x");
    let suggest = |cands: &[ShotCandidate]| {
        let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
        d.tick(&input(
            Duration::ZERO,
            Some(&on_air),
            None,
            Duration::ZERO,
            cands,
        ))
        .suggestion
    };
    let x = subject("x", "cam0", 0.9);
    // Equal quality and priority: the tie goes to the earlier candidate.
    let level = [
        x.clone(),
        ranked("a", "cam1", 0.9, 0),
        ranked("b", "cam2", 0.9, 0),
    ];
    assert_eq!(suggest(&level), Some(id("a")));
    // One level is enough to win a fresh choice, wherever b is listed.
    let b_up = [
        x.clone(),
        ranked("a", "cam1", 0.9, 0),
        ranked("b", "cam2", 0.9, 1),
    ];
    assert_eq!(suggest(&b_up), Some(id("b")));
    let b_first = [x, ranked("b", "cam2", 0.9, 1), ranked("a", "cam1", 0.9, 0)];
    assert_eq!(suggest(&b_first), Some(id("b")));
}

#[test]
fn quality_still_counts_against_priority() {
    let on_air = id("x");
    let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
    // b is one level up (0.05) but tracked 0.1 worse.
    let cands = [
        subject("x", "cam0", 0.9),
        ranked("a", "cam1", 0.9, 0),
        ranked("b", "cam2", 0.8, 1),
    ];
    let decision = d.tick(&input(
        Duration::ZERO,
        Some(&on_air),
        None,
        Duration::ZERO,
        &cands,
    ));
    assert_eq!(decision.suggestion, Some(id("a")));
}

#[test]
fn a_priority_lead_needs_the_margin_to_displace_the_incumbent() {
    let on_air = id("x");
    let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
    let mut tick = |k: u64, cands: &[ShotCandidate]| {
        d.tick(&input(
            tick_time(k),
            Some(&on_air),
            None,
            tick_time(k),
            cands,
        ))
    };
    let x = subject("x", "cam0", 0.9);
    let a = ranked("a", "cam1", 0.9, 0);
    assert_eq!(tick(0, &[x.clone(), a.clone()]).suggestion, Some(id("a")));
    // One level (0.05) is below the 0.1 margin: a stays, unchallenged.
    let one = [x.clone(), a.clone(), ranked("b", "cam2", 0.9, 1)];
    for k in 1..=50 {
        let decision = tick(k, &one);
        assert_eq!(decision.suggestion, Some(id("a")), "tick {k}");
        assert_eq!(decision.reason, Reason::Best);
    }
    // Three levels (0.15) lead by more than the margin: b is held from
    // tick 51 for the 0.75 s hold time, then suggested on tick 59.
    let three = [x, a, ranked("b", "cam2", 0.9, 3)];
    for k in 51..59 {
        let decision = tick(k, &three);
        assert_eq!(decision.suggestion, Some(id("a")), "tick {k}");
        assert_eq!(
            decision.reason,
            Reason::Holding {
                challenger: id("b")
            }
        );
    }
    assert_eq!(tick(59, &three).suggestion, Some(id("b")));
}

#[test]
fn auto_takes_the_higher_priority_shot_of_equal_quality() {
    let mut sim = Sim::new(
        director(DirectorMode::Auto, ContentProfile::Talk),
        Some("x"),
    );
    // Neither challenger beats program by the margin, so program stays to
    // its maximum dwell; the take then goes to the higher-priority shot,
    // although it is listed last.
    sim.run(Duration::from_secs(21), |_| {
        vec![
            subject("x", "cam0", 0.9),
            ranked("a", "cam1", 0.9, 0),
            ranked("b", "cam2", 0.9, 1),
        ]
    });
    let (when, shot, reason) = sim.takes[0].clone();
    assert_eq!(when, Duration::from_secs(20));
    assert_eq!(shot, id("b"));
    assert_eq!(reason, Reason::MaxDwell);
    assert_eq!(sim.takes.len(), 1);
}

#[test]
fn program_keeps_its_priority_against_challengers() {
    let run = |levels: u8| {
        let mut sim = Sim::new(
            director(DirectorMode::Auto, ContentProfile::Talk),
            Some("b"),
        );
        sim.run(Duration::from_secs(15), |_| {
            vec![
                ranked("a", "cam1", 1.0, 0),
                ranked("b", "cam2", 0.8, levels),
            ]
        });
        sim.takes
    };
    // Without priority, a beats program b by 0.2 and is taken at the
    // minimum dwell.
    let takes = run(0);
    assert_eq!(takes[0], (Duration::from_secs(4), id("a"), Reason::Better));
    // Four levels up, b scores 1.0 on program too: a never leads by the
    // margin, and before the maximum dwell nothing is taken.
    assert!(run(4).is_empty());
}
