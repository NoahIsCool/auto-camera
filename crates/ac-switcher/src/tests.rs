//! Unit tests: takes, transitions, the T-bar, the queue of presses made
//! during a transition, and tally.

use super::*;

/// A shot id.
fn id(s: &str) -> ShotId {
    ShotId(s.into())
}

/// Every shot is available.
fn all(_: &ShotId) -> bool {
    true
}

/// Every shot but `shot` is available.
fn all_but(shot: &str) -> impl Fn(&ShotId) -> bool + '_ {
    move |s| s.0 != shot
}

/// "wide" on program, "pastor" on preview, a 0.8 s mix selected.
fn staged() -> Switcher {
    let mut s = Switcher::new();
    s.set_preview(id("wide"));
    s.cut(all).unwrap();
    s.set_preview(id("pastor"));
    s
}

/// [`staged`] with a mix from "wide" to "pastor" running for `elapsed` s.
fn mixing(elapsed: f64) -> Switcher {
    let mut s = staged();
    s.auto(all).unwrap();
    s.tick(elapsed, all);
    assert!(s.in_transition());
    s
}

/// A transition of `kind` lasting `duration` seconds.
fn transition(kind: TransitionKind, duration: f64) -> Transition {
    Transition { kind, duration }
}

#[test]
fn cut_swaps_program_and_preview() {
    let mut s = staged();
    s.cut(all).unwrap();
    assert_eq!(s.program(), Some(&id("pastor")));
    assert_eq!(s.preview(), Some(&id("wide")));
}

#[test]
fn take_without_preview_is_refused() {
    let mut s = Switcher::new();
    assert_eq!(s.cut(all), Err(SwitchError::NoPreview));
    assert_eq!(s.auto(all), Err(SwitchError::NoPreview));
    assert_eq!(s, Switcher::new());
}

#[test]
fn unavailable_shot_is_never_taken() {
    let mut s = staged();
    assert_eq!(
        s.auto(|_| false),
        Err(SwitchError::Unavailable(id("pastor")))
    );
    assert_eq!(
        s.cut(all_but("pastor")),
        Err(SwitchError::Unavailable(id("pastor")))
    );
    assert_eq!(s.program(), Some(&id("wide")));
    assert!(!s.in_transition());
}

#[test]
fn mix_blends_then_completes_after_duration() {
    let mut s = mixing(0.4);
    let out = s.output();
    assert_eq!(out.base, Some(id("wide")));
    let (shot, alpha) = out.overlay.unwrap();
    assert_eq!(shot, id("pastor"));
    assert!((alpha - 0.5).abs() < 1e-9);
    s.tick(0.41, all);
    assert!(!s.in_transition());
    assert_eq!(s.program(), Some(&id("pastor")));
}

#[test]
fn dip_goes_through_black_at_midpoint() {
    let mut s = staged();
    s.set_transition(transition(TransitionKind::Dip, 1.0))
        .unwrap();
    s.auto(all).unwrap();
    s.tick(0.5, all);
    assert!(s.output().dip > 0.99);
    s.tick(0.3, all);
    assert_eq!(s.output().base, Some(id("pastor")));
}

#[test]
fn a_dt_that_is_not_a_positive_number_changes_nothing() {
    let s = mixing(0.2);
    for dt in [0.0, -0.5, f64::NAN, f64::INFINITY] {
        let mut t = s.clone();
        t.tick(dt, all);
        assert_eq!(t, s, "dt {dt}");
    }
}

#[test]
fn a_take_during_a_mix_fires_once_at_its_end() {
    let mut s = mixing(0.2);
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    s.auto(all).unwrap();
    assert_eq!(
        s.queued(),
        Some(&Queued {
            shot: id("band"),
            take: true
        })
    );
    // The running mix and the preview bus are untouched until it ends.
    assert_eq!(s.preview(), Some(&id("pastor")));
    s.tick(0.3, all);
    assert_eq!(s.program(), Some(&id("wide")));
    // Every state the buses pass through, in 20 ms ticks for 3 s.
    let state = |s: &Switcher| {
        (
            s.program().cloned(),
            s.preview().cloned(),
            s.in_transition(),
        )
    };
    let mut states = vec![state(&s)];
    for _ in 0..150 {
        s.tick(0.02, all);
        if states.last() != Some(&state(&s)) {
            states.push(state(&s));
        }
    }
    let some = |name| Some(id(name));
    assert_eq!(
        states,
        [
            (some("wide"), some("pastor"), true),
            (some("pastor"), some("band"), true),
            (some("band"), some("pastor"), false),
        ],
        "pastor lands, then the queued take brings band in, once"
    );
    assert_eq!(s.queued(), None);
    assert_eq!(s.dropped_takes(), 0);
}

#[test]
fn the_queued_take_starts_on_the_tick_the_running_mix_ends() {
    let mut s = mixing(0.5);
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    s.tick(0.31, all);
    assert_eq!(s.program(), Some(&id("pastor")));
    assert_eq!(s.preview(), Some(&id("band")));
    assert!(s.in_transition());
    assert_eq!(s.output().overlay, Some((id("band"), 0.0)));
}

#[test]
fn a_queued_take_whose_shot_became_unavailable_is_dropped_with_a_reason() {
    let mut s = mixing(0.2);
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    s.tick(0.7, all_but("band"));
    assert!(!s.in_transition(), "nothing is taken");
    assert_eq!(s.program(), Some(&id("pastor")));
    assert_eq!(s.preview(), Some(&id("band")), "band stays staged");
    assert_eq!(s.queued(), None);
    assert_eq!(s.dropped_takes(), 1);
    assert_eq!(
        s.last_dropped(),
        Some(&SwitchError::Unavailable(id("band")))
    );
    // Once band is healthy again, it can be taken as usual.
    s.auto(all).unwrap();
    assert!(s.in_transition());
}

#[test]
fn a_take_that_could_not_fire_now_is_refused_at_the_press() {
    let mut s = mixing(0.2);
    s.set_preview(id("band"));
    let before = s.clone();
    assert_eq!(
        s.auto(all_but("band")),
        Err(SwitchError::Unavailable(id("band")))
    );
    assert_eq!(s, before);
}

#[test]
fn staging_alone_lands_on_preview_in_place_of_the_outgoing_shot() {
    let mut s = mixing(0.2);
    s.set_preview(id("band"));
    s.set_preview(id("drums"));
    assert_eq!(
        s.queued(),
        Some(&Queued {
            shot: id("drums"),
            take: false
        }),
        "the latest press wins"
    );
    s.tick(1.0, all);
    assert!(!s.in_transition());
    assert_eq!(s.program(), Some(&id("pastor")));
    assert_eq!(s.preview(), Some(&id("drums")));
}

#[test]
fn a_queued_take_follows_the_latest_staging() {
    let mut s = mixing(0.2);
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    s.set_preview(id("drums"));
    s.tick(0.65, all);
    assert_eq!(s.preview(), Some(&id("drums")));
    assert!(s.in_transition());
    s.tick(0.8, all);
    assert_eq!(s.program(), Some(&id("drums")));
}

#[test]
fn staging_the_incoming_shot_clears_the_queue() {
    let mut s = mixing(0.2);
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    s.set_preview(id("pastor"));
    assert_eq!(s.queued(), None);
    s.tick(1.0, all);
    assert!(!s.in_transition());
    assert_eq!(s.preview(), Some(&id("wide")), "preview follows program");
}

#[test]
fn auto_during_a_transition_with_nothing_staged_changes_nothing() {
    let mut s = mixing(0.2);
    let before = s.clone();
    s.auto(all).unwrap();
    assert_eq!(s, before);
    s.tick(1.0, all);
    assert!(!s.in_transition(), "no second transition");
    assert_eq!(s.program(), Some(&id("pastor")));
}

#[test]
fn cut_during_a_mix_or_dip_lands_the_incoming_shot_at_once() {
    for kind in [TransitionKind::Mix, TransitionKind::Dip] {
        let mut s = staged();
        s.set_transition(transition(kind, 2.0)).unwrap();
        s.auto(all).unwrap();
        s.tick(0.5, all);
        // The incoming shot was checked when the transition started.
        s.cut(|_| false).unwrap();
        assert!(!s.in_transition(), "{kind:?}");
        assert_eq!(s.program(), Some(&id("pastor")));
        assert_eq!(s.preview(), Some(&id("wide")));
        assert_eq!(
            s.output(),
            Output {
                base: Some(id("pastor")),
                overlay: None,
                dip: 0.0
            }
        );
    }
}

#[test]
fn cut_during_a_transition_takes_the_shot_staged_meanwhile() {
    let mut s = mixing(0.2);
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    assert_eq!(
        s.cut(all_but("band")),
        Err(SwitchError::Unavailable(id("band")))
    );
    assert!(s.in_transition(), "a refused cut changes nothing");
    s.cut(all).unwrap();
    assert!(!s.in_transition());
    assert_eq!(s.program(), Some(&id("band")));
    assert_eq!(s.preview(), Some(&id("wide")), "the old program shot");
    assert_eq!(s.queued(), None, "the queued take is spent");
    s.tick(2.0, all);
    assert_eq!(s.program(), Some(&id("band")));
}

#[test]
fn the_tbar_and_the_clock_do_not_share_a_transition() {
    let mut s = mixing(0.2);
    let before = s.clone();
    assert_eq!(s.set_tbar(0.5, all), Err(SwitchError::Busy));
    assert_eq!(s, before);
    let mut s = staged();
    s.set_tbar(0.5, all).unwrap();
    s.tick(5.0, all);
    assert!(s.in_transition(), "the clock does not move the T-bar");
    let before = s.clone();
    assert_eq!(s.cut(all), Err(SwitchError::TBarHeld));
    assert_eq!(s, before);
}

#[test]
fn a_take_queued_during_a_tbar_transition_fires_at_either_end() {
    let mut s = staged();
    s.set_tbar(0.5, all).unwrap();
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    s.set_tbar(1.0, all).unwrap();
    assert_eq!(s.program(), Some(&id("pastor")));
    assert_eq!(s.preview(), Some(&id("band")));
    assert!(s.in_transition(), "an automatic mix to band");
    assert_eq!(s.set_tbar(0.5, all), Err(SwitchError::Busy));
    s.tick(0.8, all);
    assert_eq!(s.program(), Some(&id("band")));

    let mut s = staged();
    s.set_tbar(0.3, all).unwrap();
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    s.set_tbar(0.0, all).unwrap();
    assert_eq!(s.program(), Some(&id("wide")), "abandoned");
    assert_eq!(s.preview(), Some(&id("band")));
    assert!(s.in_transition());
}

#[test]
fn a_queued_take_with_cut_selected_cuts_at_the_end() {
    let mut s = mixing(0.2);
    s.set_transition(transition(TransitionKind::Cut, 0.0))
        .unwrap();
    s.set_preview(id("band"));
    s.auto(all).unwrap();
    s.tick(0.65, all);
    assert!(!s.in_transition());
    assert_eq!(s.program(), Some(&id("band")));
    assert_eq!(s.preview(), Some(&id("pastor")));
}

#[test]
fn tbar_completes_and_reverses_direction() {
    let mut s = staged();
    s.set_tbar(0.5, all).unwrap();
    assert!(s.in_transition());
    s.set_tbar(1.0, all).unwrap();
    assert_eq!(s.program(), Some(&id("pastor")));
    // The bar now sits at 1.0; pulling it back towards 0 takes the next shot.
    s.set_tbar(0.4, all).unwrap();
    assert!(s.in_transition());
    s.set_tbar(0.0, all).unwrap();
    assert_eq!(s.program(), Some(&id("wide")));
}

#[test]
fn tbar_returned_to_start_abandons_transition() {
    let mut s = staged();
    s.set_tbar(0.3, all).unwrap();
    s.set_tbar(0.0, all).unwrap();
    assert!(!s.in_transition());
    assert_eq!(s.program(), Some(&id("wide")));
}

#[test]
fn tally_follows_exposure() {
    let lamps = |s: &Switcher, shot: &str| {
        let t = s.tally(&id(shot));
        (t.program, t.preview)
    };
    let mut s = staged();
    assert_eq!(lamps(&s, "wide"), (true, false));
    assert_eq!(lamps(&s, "pastor"), (false, true));
    s.auto(all).unwrap();
    assert_eq!(lamps(&s, "pastor"), (false, true), "mix at zero");
    s.tick(0.4, all);
    assert_eq!(lamps(&s, "wide"), (true, false));
    assert_eq!(lamps(&s, "pastor"), (true, false), "both sides of a mix");
    assert!((s.exposure(&id("wide")) + s.exposure(&id("pastor")) - 1.0).abs() < 1e-12);
    s.set_preview(id("band"));
    assert_eq!(lamps(&s, "band"), (false, false), "queued, not on preview");

    let mut s = staged();
    s.set_transition(transition(TransitionKind::Dip, 1.0))
        .unwrap();
    s.auto(all).unwrap();
    s.tick(0.3, all);
    assert_eq!(lamps(&s, "wide"), (true, false));
    assert_eq!(lamps(&s, "pastor"), (false, true), "hidden by the dip");
    s.tick(0.4, all);
    assert_eq!(lamps(&s, "wide"), (false, false));
    assert_eq!(lamps(&s, "pastor"), (true, false));
    assert_eq!(s.exposure(&id("nobody")), 0.0);
}

#[test]
fn invalid_duration_is_rejected() {
    let mut s = Switcher::new();
    let bad = transition(TransitionKind::Mix, 0.0);
    assert_eq!(
        s.set_transition(bad),
        Err(SwitchError::InvalidDuration(0.0))
    );
    assert_eq!(
        s.set_transition(transition(TransitionKind::Dip, f64::NAN))
            .map_err(|e| e.to_string()),
        Err("invalid transition duration NaN".into())
    );
    assert!(
        s.set_transition(transition(TransitionKind::Cut, 0.0))
            .is_ok()
    );
}
