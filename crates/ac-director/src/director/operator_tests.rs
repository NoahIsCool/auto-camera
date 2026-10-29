//! Scenario tests for the operator's side of the director: skipped
//! suggestions and each takeover setting, driven tick by tick at 10 Hz.

use std::time::Duration;

use super::Director;
use super::test_support::{Sim, id, input, subject, tick_time, wide};
use crate::{ContentProfile, DirectorConfig, DirectorMode, Handover, Reason, Takeover};

/// A director in `mode` for music (2.5 s minimum dwell) with `takeover`, a
/// 3 s pause and a 5 s skip window.
fn operated(mode: DirectorMode, takeover: Takeover) -> Director {
    Director::new(DirectorConfig {
        takeover,
        pause: Duration::from_secs(3),
        skip: Duration::from_secs(5),
        ..DirectorConfig::new(mode, ContentProfile::Music)
    })
    .unwrap()
}

#[test]
fn a_skipped_shot_is_not_suggested_within_its_window() {
    let mut d = operated(DirectorMode::Suggest, Takeover::Suggest);
    let cands = [
        subject("a", "cam1", 0.9),
        subject("b", "cam2", 0.95),
        subject("c", "cam3", 0.8),
    ];
    let a = id("a");
    let at = |d: &mut Director, k| {
        d.tick(&input(tick_time(k), Some(&a), None, tick_time(k), &cands))
            .suggestion
    };
    assert_eq!(at(&mut d, 0), Some(id("b")));
    // Skipped at 1 s: c replaces it on that tick, and b stays out for the
    // whole window, although it leads c by more than the margin.
    d.skip(id("b"), tick_time(10));
    assert_eq!(
        d.skipped(tick_time(10)),
        [(id("b"), Duration::from_secs(5))]
    );
    for k in 10..60 {
        assert_eq!(at(&mut d, k), Some(id("c")), "tick {k}");
    }
    assert!(d.skipped(tick_time(60)).is_empty());
    // Then it competes again under the usual hysteresis: back once its lead
    // has held for the 0.75 s hold, the first tick from 6.75 s.
    let back = (60..80).find(|&k| at(&mut d, k) == Some(id("b")));
    assert_eq!(back, Some(68));
}

#[test]
fn skipping_again_restarts_the_window_and_off_forgets_it() {
    let mut d = operated(DirectorMode::Suggest, Takeover::Suggest);
    let cands = [subject("a", "cam1", 0.9), subject("b", "cam2", 0.95)];
    let a = id("a");
    d.skip(id("b"), tick_time(0));
    d.skip(id("b"), tick_time(30));
    assert_eq!(d.skipped(tick_time(30)).len(), 1, "one entry per shot");
    for k in 0..80 {
        let decision = d.tick(&input(tick_time(k), Some(&a), None, tick_time(k), &cands));
        assert_eq!(decision.suggestion, None, "tick {k}");
        assert_eq!(decision.reason, Reason::NoCandidate);
    }
    assert_eq!(
        d.tick(&input(tick_time(80), Some(&a), None, tick_time(80), &cands))
            .suggestion,
        Some(id("b"))
    );
    // Off mode keeps no state, skips included.
    d.skip(id("b"), tick_time(81));
    d.set_mode(DirectorMode::Off);
    d.tick(&input(tick_time(82), Some(&a), None, tick_time(82), &cands));
    d.set_mode(DirectorMode::Suggest);
    assert!(d.skipped(tick_time(83)).is_empty());
}

#[test]
fn auto_never_takes_a_skipped_shot() {
    let mut sim = Sim::new(operated(DirectorMode::Auto, Takeover::Suggest), Some("a"));
    sim.director.skip(id("b"), Duration::ZERO);
    // b is by far the best shot; c is taken at the minimum dwell instead.
    sim.run(Duration::from_millis(4900), |_| {
        vec![
            subject("a", "cam1", 0.5),
            subject("b", "cam2", 0.95),
            subject("c", "cam3", 0.7),
        ]
    });
    let taken: Vec<_> = sim.takes.iter().map(|(_, s, _)| s.clone()).collect();
    assert_eq!(taken, [id("c")]);
}

#[test]
fn a_clock_step_backwards_restarts_the_skip_and_pause_windows() {
    let mut d = operated(DirectorMode::Auto, Takeover::Pause);
    let cands = [subject("a", "cam1", 0.9), subject("b", "cam2", 0.95)];
    let a = id("a");
    d.tick(&input(tick_time(40), Some(&a), None, tick_time(40), &cands));
    d.skip(id("b"), tick_time(40));
    d.take_over(tick_time(40));
    d.tick(&input(tick_time(10), Some(&a), None, tick_time(10), &cands));
    assert_eq!(d.skipped(tick_time(10))[0].1, Duration::from_secs(5));
    assert_eq!(d.paused(tick_time(10)), Some(Duration::from_secs(3)));
}

#[test]
fn outside_auto_a_manual_action_changes_nothing() {
    for mode in [DirectorMode::Off, DirectorMode::Suggest] {
        for takeover in [Takeover::Suggest, Takeover::Pause, Takeover::Director] {
            let mut d = operated(mode, takeover);
            assert_eq!(d.take_over(tick_time(5)), None, "{mode:?} {takeover:?}");
            assert_eq!(d.config().mode, mode);
            assert_eq!(d.paused(tick_time(5)), None);
        }
    }
}

/// A shot "a" on program with "b" beating it by far from the start: Auto
/// takes "b" at the minimum dwell (2.5 s) unless something stops it.
fn beaten(_: Duration) -> Vec<crate::ShotCandidate> {
    vec![subject("a", "cam1", 0.5), subject("b", "cam2", 0.95)]
}

#[test]
fn the_suggest_takeover_drops_auto_to_suggest_for_good() {
    let mut sim = Sim::new(operated(DirectorMode::Auto, Takeover::Suggest), Some("a"));
    sim.run(tick_time(20), beaten);
    assert_eq!(
        sim.director.take_over(tick_time(20)),
        Some(Handover::ToSuggest)
    );
    assert_eq!(sim.director.config().mode, DirectorMode::Suggest);
    assert_eq!(sim.director.paused(tick_time(20)), None);
    let decisions = sim.run(Duration::from_secs(30), beaten);
    assert!(sim.takes.is_empty());
    assert!(decisions.iter().all(|(_, d)| d.take.is_none()));
    assert!(decisions.iter().all(|(_, d)| d.suggestion == Some(id("b"))));
    // Once in Suggest, further actions change nothing.
    assert_eq!(sim.director.take_over(tick_time(300)), None);
}

#[test]
fn the_pause_takeover_holds_takes_for_its_window_from_the_last_action() {
    let mut sim = Sim::new(operated(DirectorMode::Auto, Takeover::Pause), Some("a"));
    sim.run(tick_time(20), beaten);
    assert_eq!(
        sim.director.take_over(tick_time(20)),
        Some(Handover::Paused(Duration::from_secs(3)))
    );
    // Without the pause "b" would be taken at 2.5 s; it is taken at 5 s,
    // when the pause ends, and the reason counts the pause down meanwhile.
    let decisions = sim.run(tick_time(50), beaten);
    assert_eq!(sim.director.config().mode, DirectorMode::Auto);
    for (now, d) in decisions.iter().filter(|(now, _)| *now < tick_time(50)) {
        let remaining = Duration::from_secs(3) - (*now - tick_time(20));
        assert_eq!(d.reason, Reason::Paused { remaining }, "{now:?}");
        assert_eq!(d.suggestion, Some(id("b")), "it still suggests");
    }
    assert_eq!(sim.takes.len(), 1);
    assert_eq!(sim.takes[0].0, tick_time(50));
    assert_eq!(sim.takes[0].1, id("b"));
    // Each further action restarts the pause; a mode change ends it.
    let mut sim = Sim::new(operated(DirectorMode::Auto, Takeover::Pause), Some("a"));
    sim.run(tick_time(20), beaten);
    sim.director.take_over(tick_time(20));
    sim.run(tick_time(40), beaten);
    sim.director.take_over(tick_time(40));
    sim.run(tick_time(60), beaten);
    assert!(sim.takes.is_empty(), "paused until 7 s");
    assert_eq!(
        sim.director.paused(tick_time(60)),
        Some(Duration::from_secs(1))
    );
    sim.director.set_mode(DirectorMode::Auto);
    assert_eq!(sim.director.paused(tick_time(60)), None);
    sim.run(tick_time(61), beaten);
    assert_eq!(sim.takes.len(), 1);
    assert_eq!(sim.takes[0].0, tick_time(61));
}

#[test]
fn the_director_takeover_lets_auto_carry_on() {
    let mut sim = Sim::new(operated(DirectorMode::Auto, Takeover::Director), Some("a"));
    sim.run(tick_time(20), beaten);
    assert_eq!(sim.director.take_over(tick_time(20)), None);
    assert_eq!(sim.director.config().mode, DirectorMode::Auto);
    assert_eq!(sim.director.paused(tick_time(20)), None);
    sim.run(tick_time(30), beaten);
    assert_eq!(sim.takes.len(), 1);
    assert_eq!(sim.takes[0].0, Duration::from_millis(2500));
}

#[test]
fn a_pause_holds_even_an_establishing_take() {
    let mut d = operated(DirectorMode::Auto, Takeover::Pause);
    let cands = [wide("wide", "cam")];
    d.take_over(Duration::ZERO);
    let decision = d.tick(&input(Duration::ZERO, None, None, Duration::ZERO, &cands));
    assert_eq!(decision.take, None);
    assert_eq!(decision.suggestion, Some(id("wide")));
    assert!(matches!(decision.reason, Reason::Paused { .. }));
    let later = Duration::from_secs(3);
    let decision = d.tick(&input(later, None, None, Duration::ZERO, &cands));
    assert_eq!(decision.reason, Reason::NothingOnAir);
    assert!(decision.take.is_some());
}
