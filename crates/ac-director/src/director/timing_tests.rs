//! Scenario tests for the director's timing rules: minimum and maximum
//! dwell, hysteresis, fallback on loss, variety and clock discontinuities.

use std::time::Duration;

use super::Director;
use super::test_support::{Sim, director, id, input, subject, tick_time, wide};
use crate::{ContentProfile, DirectorConfig, DirectorMode, Reason, ShotCandidate, TransitionHint};

#[test]
fn min_dwell_is_respected() {
    for profile in [ContentProfile::Talk, ContentProfile::Music] {
        let min = DirectorConfig::new(DirectorMode::Auto, profile).dwell.min;
        let mut sim = Sim::new(director(DirectorMode::Auto, profile), Some("a"));
        // b beats a by far more than the margin from the very first tick.
        let decisions = sim.run(Duration::from_secs(6), |_| {
            vec![subject("a", "cam1", 0.5), subject("b", "cam2", 0.95)]
        });
        let (when, shot, reason) = sim.takes[0].clone();
        assert_eq!(when, min, "{profile:?}");
        assert_eq!(shot, id("b"));
        assert_eq!(reason, Reason::Better);
        for (now, d) in decisions.iter().filter(|(now, _)| *now < min) {
            assert!(d.take.is_none());
            let remaining = min - *now;
            assert_eq!(d.reason, Reason::MinDwell { remaining });
        }
    }
}

#[test]
fn max_shot_dwell_forces_a_take() {
    let mut sim = Sim::new(
        director(DirectorMode::Auto, ContentProfile::Talk),
        Some("a"),
    );
    // b is close to a but never leads by the margin.
    sim.run(Duration::from_secs(25), |_| {
        vec![subject("a", "cam1", 0.9), subject("b", "cam2", 0.85)]
    });
    let (when, shot, reason) = sim.takes[0].clone();
    assert_eq!(when, Duration::from_secs(20));
    assert_eq!(shot, id("b"));
    assert_eq!(reason, Reason::MaxDwell);
    assert_eq!(sim.takes.len(), 1);
}

#[test]
fn max_wide_dwell_forces_a_take() {
    let mut sim = Sim::new(
        director(DirectorMode::Auto, ContentProfile::Talk),
        Some("wide"),
    );
    // a scores 0.8 against the wide's 0.75: not enough lead to take early.
    sim.run(Duration::from_secs(12), |_| {
        vec![wide("wide", "cam1"), subject("a", "cam2", 0.8)]
    });
    let (when, shot, reason) = sim.takes[0].clone();
    assert_eq!(when, Duration::from_secs(10));
    assert_eq!(shot, id("a"));
    assert_eq!(reason, Reason::MaxDwell);
}

/// Program `wide` plus subjects `a` and `b` with the given qualities.
fn pair(qa: f32, qb: f32) -> Vec<ShotCandidate> {
    vec![
        wide("wide", "cam"),
        subject("a", "cam2", qa),
        subject("b", "cam3", qb),
    ]
}

#[test]
fn hysteresis_prevents_flapping_between_near_equal_shots() {
    let suggest = || {
        Sim::new(
            director(DirectorMode::Suggest, ContentProfile::Talk),
            Some("wide"),
        )
    };
    // a and b swap the lead every tick by less than the margin.
    let small = suggest().run(Duration::from_secs(20), |now| {
        if (now.as_millis() / 100) % 2 == 0 {
            pair(0.80, 0.85)
        } else {
            pair(0.85, 0.80)
        }
    });
    let first = small[0].1.suggestion.clone();
    assert!(small.iter().all(|(_, d)| d.suggestion == first));

    // Large swings that never last the hold time do not switch either.
    let large = suggest().run(Duration::from_secs(20), |now| {
        if (now.as_millis() / 500) % 2 == 0 {
            pair(0.95, 0.75)
        } else {
            pair(0.75, 0.95)
        }
    });
    let first = large[0].1.suggestion.clone();
    assert_eq!(first, Some(id("a")));
    assert!(large.iter().all(|(_, d)| d.suggestion == first));
    let holding = |d: &crate::DirectorDecision| matches!(d.reason, Reason::Holding { .. });
    assert!(large.iter().any(|(_, d)| holding(d)));
}

#[test]
fn sustained_challenger_replaces_suggestion_after_hold() {
    let mut sim = Sim::new(
        director(DirectorMode::Suggest, ContentProfile::Talk),
        Some("wide"),
    );
    let decisions = sim.run(Duration::from_secs(3), |now| {
        let qb = if now >= Duration::from_secs(1) {
            0.95
        } else {
            0.7
        };
        pair(0.8, qb)
    });
    let switched = decisions
        .iter()
        .find(|(_, d)| d.suggestion == Some(id("b")))
        .unwrap()
        .0;
    // Lead starts at 1.0 s; the first 10 Hz tick at least 0.75 s later is 1.8 s.
    assert_eq!(switched, Duration::from_millis(1800));
    let holding = &decisions[15].1;
    assert_eq!(holding.suggestion, Some(id("a")));
    let challenger = id("b");
    assert_eq!(holding.reason, Reason::Holding { challenger });
}

/// Candidates for the loss tests: program `a` loses tracking from 10 s on.
fn loss_catalog(now: Duration) -> Vec<ShotCandidate> {
    let qa = if now >= Duration::from_secs(10) {
        0.1
    } else {
        0.9
    };
    vec![
        subject("a", "cam1", qa),
        subject("b", "cam2", 0.95),
        wide("wide", "cam1"),
    ]
}

#[test]
fn suggestion_falls_back_to_wide_on_loss() {
    let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
    let a = id("a");
    for k in 0..=120 {
        let now = tick_time(k);
        let cands = loss_catalog(now);
        let decision = d.tick(&input(now, Some(&a), None, now, &cands));
        assert!(decision.take.is_none());
        if now < Duration::from_millis(10_500) {
            assert_eq!(decision.suggestion, Some(id("b")), "at {now:?}");
        } else {
            // Immediate: no hold time even though b still scores higher.
            assert_eq!(decision.suggestion, Some(id("wide")), "at {now:?}");
            assert_eq!(decision.reason, Reason::Fallback);
        }
    }
}

#[test]
fn auto_falls_back_immediately_with_a_cut() {
    let mut d = director(DirectorMode::Auto, ContentProfile::Talk);
    let (a, b) = (id("a"), id("b"));
    // Program has been on air for only 1 s: well inside the minimum dwell.
    let on_air = Duration::from_secs(1);
    for k in 0..=105 {
        let now = tick_time(k);
        let cands = loss_catalog(now);
        let decision = d.tick(&input(now, Some(&a), Some(&b), on_air, &cands));
        if now < Duration::from_millis(10_500) {
            assert!(decision.take.is_none());
        } else {
            let take = decision.take.unwrap();
            assert_eq!(take.shot, id("wide"));
            assert_eq!(take.transition, TransitionHint::Cut);
            assert_eq!(decision.reason, Reason::Fallback);
        }
    }
}

#[test]
fn brief_dip_does_not_fall_back() {
    let mut d = director(DirectorMode::Auto, ContentProfile::Talk);
    let a = id("a");
    for k in 0..=50 {
        let now = tick_time(k);
        let lost = (10..14).contains(&k);
        let cands = [
            subject("a", "cam1", if lost { 0.1 } else { 0.9 }),
            wide("wide", "cam1"),
        ];
        let decision = d.tick(&input(now, Some(&a), None, Duration::from_secs(1), &cands));
        assert_ne!(decision.reason, Reason::Fallback);
        assert!(decision.take.is_none());
    }
}

#[test]
fn missing_or_unavailable_program_counts_as_lost() {
    for listed in [false, true] {
        let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
        let a = id("a");
        let mut cands = vec![subject("b", "cam2", 0.95), wide("wide", "cam1")];
        if listed {
            let mut down = subject("a", "cam1", 0.9);
            down.available = false;
            cands.push(down);
        }
        let zero = Duration::ZERO;
        let early = d.tick(&input(zero, Some(&a), None, zero, &cands));
        assert_eq!(early.suggestion, Some(id("b")));
        let late = d.tick(&input(tick_time(5), Some(&a), None, tick_time(5), &cands));
        assert_eq!(late.reason, Reason::Fallback);
        assert_eq!(late.suggestion, Some(id("wide")));
    }
}

/// A wide and three subjects of equal quality on four sources.
fn band(_: Duration) -> Vec<ShotCandidate> {
    vec![
        wide("wide", "cam0"),
        subject("a", "cam1", 0.9),
        subject("b", "cam2", 0.9),
        subject("c", "cam3", 0.9),
    ]
}

#[test]
fn variety_rotates_among_equal_shots() {
    let mut sim = Sim::new(director(DirectorMode::Auto, ContentProfile::Music), None);
    sim.run(Duration::from_secs(120), band);
    let order = sim.subject_takes();
    assert!(order.len() >= 9, "{order:?}");
    for name in ["a", "b", "c"] {
        let count = order.iter().filter(|s| s.0 == name).count();
        assert!(count >= 3, "{order:?}");
    }
    for w in order.windows(3) {
        assert!(w[0] != w[1] && w[1] != w[2] && w[0] != w[2], "{order:?}");
    }
    // A wide breaks the rhythm once its own penalty has decayed.
    assert!(sim.takes.iter().skip(1).any(|(_, s, _)| s.0 == "wide"));
}

#[test]
fn without_variety_penalty_equal_shots_ping_pong() {
    let mut config = DirectorConfig::new(DirectorMode::Auto, ContentProfile::Music);
    config.scoring.variety_penalty = 0.0;
    let mut sim = Sim::new(Director::new(config).unwrap(), None);
    sim.run(Duration::from_secs(120), band);
    let order = sim.subject_takes();
    assert!(order.len() >= 9);
    assert!(order.iter().all(|s| s.0 != "c"), "{order:?}");
}

#[test]
fn clock_step_backwards_restarts_timers() {
    let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
    let program = id("wide");
    let (before, after) = (pair(0.8, 0.7), pair(0.8, 0.95));
    let at = Duration::from_millis;
    let mut tick = |ms, cands: &[ShotCandidate]| {
        d.tick(&input(at(ms), Some(&program), None, at(ms), cands))
            .suggestion
    };
    assert_eq!(tick(0, &before), Some(id("a")));
    assert_eq!(tick(1000, &after), Some(id("a")));
    assert_eq!(tick(1500, &after), Some(id("a")));
    // The clock jumps back: the lead timer restarts at 1.2 s, so 1.8 s is
    // too early (it would have switched without the restart).
    assert_eq!(tick(1200, &after), Some(id("a")));
    assert_eq!(tick(1800, &after), Some(id("a")));
    assert_eq!(tick(2000, &after), Some(id("b")));
}
