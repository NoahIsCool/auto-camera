//! Scenario tests for what the director may suggest and take: modes,
//! eligibility, jump cuts, transitions, preview staging and configuration.
//! Timing rules (dwell, hysteresis, fallback, variety) live in
//! `timing_tests`.

use std::time::Duration;

use ac_core::{Rect, ShotId};

use super::DirectorInput;
use super::test_support::{Sim, director, id, input, subject, tick_time, wide};
use crate::{
    ConfigError, ContentProfile, DirectorConfig, DirectorDecision, DirectorMode, Reason,
    ShotCandidate, TransitionHint, is_jump_cut,
};

#[test]
fn off_mode_outputs_nothing_and_keeps_no_state() {
    let mut d = director(DirectorMode::Off, ContentProfile::Talk);
    let cands = [wide("wide", "cam"), subject("a", "cam2", 0.9)];
    let a = id("a");
    for k in 0..100 {
        for program in [None, Some(&a)] {
            let decision = d.tick(&input(tick_time(k), program, None, tick_time(k), &cands));
            assert_eq!(decision, DirectorDecision::off());
        }
    }
    assert!(d.suggestion().is_none());
    d.set_mode(DirectorMode::Suggest);
    let decision = d.tick(&input(
        tick_time(100),
        Some(&a),
        None,
        tick_time(100),
        &cands,
    ));
    assert_eq!(decision.suggestion, Some(id("wide")));
}

#[test]
fn suggest_mode_never_takes() {
    let mut sim = Sim::new(director(DirectorMode::Suggest, ContentProfile::Music), None);
    // Nothing on air, then a lost program held past its maximum dwell: each
    // would take at once in Auto mode.
    let empty = sim.run(Duration::from_secs(5), |_| {
        vec![wide("wide", "cam"), subject("a", "cam2", 0.9)]
    });
    assert_eq!(empty[0].1.reason, Reason::NothingOnAir);
    assert_eq!(empty[0].1.suggestion, Some(id("wide")));
    sim.program = Some(id("a"));
    sim.on_air_since = Duration::from_secs(5);
    let lost = sim.run(Duration::from_secs(30), |_| {
        vec![wide("wide", "cam"), subject("a", "cam2", 0.1)]
    });
    assert!(lost.iter().any(|(_, d)| d.reason == Reason::Fallback));
    assert!(sim.takes.is_empty());
    assert!(empty.iter().chain(&lost).all(|(_, d)| d.take.is_none()));
}

#[test]
fn suggests_best_eligible_shot_only() {
    let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
    let mut invalid = subject("invalid", "cam5", 1.0);
    invalid.crop.w = -0.1;
    let mut down = subject("down", "cam3", 1.0);
    down.available = false;
    let cands = [
        subject("a", "cam1", 1.0),
        down,
        subject("weak", "cam4", 0.2),
        invalid,
        subject("b", "cam2", 0.8),
    ];
    let a = id("a");
    let decision = d.tick(&input(
        Duration::ZERO,
        Some(&a),
        None,
        Duration::ZERO,
        &cands,
    ));
    assert_eq!(decision.suggestion, Some(id("b")));
    assert_eq!(decision.reason, Reason::Best);
}

#[test]
fn no_eligible_shot_gives_no_suggestion() {
    let mut d = director(DirectorMode::Auto, ContentProfile::Talk);
    let decision = d.tick(&input(Duration::ZERO, None, None, Duration::ZERO, &[]));
    assert_eq!(decision.suggestion, None);
    assert_eq!(decision.take, None);
    assert_eq!(decision.reason, Reason::NoCandidate);
    // NaN quality counts as zero, below the fallback threshold.
    let cands = [subject("a", "cam1", 0.9), subject("nan", "cam2", f32::NAN)];
    let a = id("a");
    let decision = d.tick(&input(tick_time(1), Some(&a), None, tick_time(1), &cands));
    assert_eq!(decision.reason, Reason::NoCandidate);
}

#[test]
fn duplicate_ids_use_first_entry() {
    let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
    let a = id("a");
    let mut b_down = subject("b", "cam2", 0.9);
    b_down.available = false;
    let cands = [
        subject("a", "cam1", 0.9),
        subject("b", "cam2", 0.9),
        b_down.clone(),
    ];
    let decision = d.tick(&input(
        Duration::ZERO,
        Some(&a),
        None,
        Duration::ZERO,
        &cands,
    ));
    assert_eq!(decision.suggestion, Some(id("b")));
    let cands = [subject("a", "cam1", 0.9), b_down, subject("b", "cam2", 0.9)];
    let decision = d.tick(&input(tick_time(1), Some(&a), None, tick_time(1), &cands));
    assert_eq!(decision.suggestion, None);
}

#[test]
fn nothing_on_air_establishes_wide_and_auto_takes_it() {
    let mut d = director(DirectorMode::Auto, ContentProfile::Talk);
    let cands = [subject("a", "cam2", 0.95), wide("wide", "cam")];
    let decision = d.tick(&input(Duration::ZERO, None, None, Duration::ZERO, &cands));
    assert_eq!(decision.suggestion, Some(id("wide")));
    let take = decision.take.unwrap();
    assert_eq!(take.shot, id("wide"));
    assert_eq!(take.transition, TransitionHint::Mix);
    assert_eq!(decision.reason, Reason::NothingOnAir);
    // Without an eligible wide the best shot establishes instead.
    let mut d = director(DirectorMode::Auto, ContentProfile::Talk);
    let decision = d.tick(&input(
        Duration::ZERO,
        None,
        None,
        Duration::ZERO,
        &cands[..1],
    ));
    assert_eq!(decision.take.unwrap().shot, id("a"));
}

#[test]
fn due_take_waits_for_suggestion_on_preview() {
    let mut d = director(DirectorMode::Auto, ContentProfile::Talk);
    let cands = [subject("a", "cam1", 0.9), subject("b", "cam2", 0.85)];
    let (a, b, other) = (id("a"), id("b"), id("other"));
    let on_air = Duration::from_secs(30);
    let decision = d.tick(&input(
        Duration::ZERO,
        Some(&a),
        Some(&other),
        on_air,
        &cands,
    ));
    assert_eq!(decision.take, None);
    assert_eq!(decision.reason, Reason::AwaitingPreview);
    let decision = d.tick(&input(tick_time(1), Some(&a), Some(&b), on_air, &cands));
    assert_eq!(decision.take.unwrap().shot, b);
    assert_eq!(decision.reason, Reason::MaxDwell);
}

#[test]
fn jump_cuts_are_never_suggested() {
    let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
    let medium = subject("medium", "cam", 0.8);
    // A tighter crop of the same source: IoU with the medium ≈ 0.56.
    let close = ShotCandidate {
        crop: Rect::new(0.35, 0.25, 0.3, 0.3),
        ..subject("close", "cam", 1.0)
    };
    let elsewhere = ShotCandidate {
        crop: close.crop,
        ..subject("elsewhere", "cam2", 0.6)
    };
    let m = id("medium");
    let cands = [
        medium.clone(),
        close.clone(),
        elsewhere,
        wide("wide", "cam"),
    ];
    let decision = d.tick(&input(
        Duration::ZERO,
        Some(&m),
        None,
        Duration::ZERO,
        &cands,
    ));
    // close scores 1.0 but is a jump cut; the wide (0.75) beats elsewhere (0.6).
    assert_eq!(decision.suggestion, Some(id("wide")));
    let decision = d.tick(&input(
        tick_time(1),
        Some(&m),
        None,
        tick_time(1),
        &[medium, close],
    ));
    assert_eq!(decision.reason, Reason::NoCandidate);
}

#[test]
fn unattended_run_never_jump_cuts() {
    let catalog = |_| {
        vec![
            wide("wide", "cam"),
            subject("medium", "cam", 0.9),
            ShotCandidate {
                crop: Rect::new(0.35, 0.25, 0.3, 0.3),
                ..subject("close", "cam", 0.95)
            },
            subject("drums", "cam2", 0.9),
        ]
    };
    let mut sim = Sim::new(director(DirectorMode::Auto, ContentProfile::Music), None);
    sim.run(Duration::from_secs(120), catalog);
    let list = catalog(Duration::ZERO);
    let by_id = |s: &ShotId| list.iter().find(|c| &c.id == s).unwrap();
    assert!(sim.takes.len() > 5);
    for pair in sim.takes.windows(2) {
        assert!(!is_jump_cut(by_id(&pair[0].1), by_id(&pair[1].1), 0.4));
    }
}

#[test]
fn never_takes_during_a_transition() {
    let mut d = director(DirectorMode::Auto, ContentProfile::Talk);
    let cands = [wide("wide", "cam"), subject("a", "cam2", 0.1)];
    let a = id("a");
    // Nothing on air for 1 s, then a lost program long enough to fall back:
    // both would take at once outside a transition.
    for k in 0..=20 {
        let program = (k >= 10).then_some(&a);
        let decision = d.tick(&DirectorInput {
            in_transition: true,
            ..input(tick_time(k), program, None, Duration::from_secs(60), &cands)
        });
        assert_eq!(decision.take, None);
        assert_eq!(decision.reason, Reason::TransitionInProgress);
        assert_eq!(decision.suggestion, Some(id("wide")));
    }
    let decision = d.tick(&input(
        tick_time(21),
        Some(&a),
        None,
        Duration::ZERO,
        &cands,
    ));
    assert_eq!(decision.reason, Reason::Fallback);
    assert!(decision.take.is_some());
}

#[test]
fn invalid_config_is_rejected() {
    let config = DirectorConfig {
        margin: -1.0,
        ..DirectorConfig::default()
    };
    assert!(matches!(
        super::Director::new(config.clone()),
        Err(ConfigError::OutOfRange { name: "margin", .. })
    ));
    let mut d = director(DirectorMode::Suggest, ContentProfile::Talk);
    assert!(d.set_config(config).is_err());
    assert_eq!(d.config().mode, DirectorMode::Suggest);
    let music = DirectorConfig::new(DirectorMode::Auto, ContentProfile::Music);
    assert!(d.set_config(music.clone()).is_ok());
    assert_eq!(d.config(), &music);
}
