//! The settle gate on its own, driven tick by tick with a `dt` and the
//! settledness each tick would see: a waiting take fires when preview
//! settles or once `max_wait` has passed, whichever comes first, is dropped
//! when preview changes, and is forced by a second press; without a rule
//! nothing ever waits.

use super::*;

/// One tick at 30 fps.
const DT: f64 = 1.0 / 30.0;

/// The spec's rule: settled at 0.6, at most 0.5 s.
fn rule() -> SettleRule {
    SettleRule {
        threshold: 0.6,
        max_wait: 0.5,
    }
}

/// The shot with `id`.
fn shot(id: &str) -> ShotId {
    ShotId(id.into())
}

/// A gate with [`rule`] and a Cut of "a" waiting, pressed at settledness
/// 0.2.
fn waiting_cut() -> SettleGate {
    let mut gate = SettleGate::new(Some(rule()));
    assert!(gate.press(TakeKind::Cut, Some(&shot("a")), Some(0.2), true));
    gate
}

/// Ticks `gate` at [`DT`] with "a" on preview at the settledness of each
/// entry of `settling`; returns the 1-based tick on which a take fired,
/// with what fired.
fn run(gate: &mut SettleGate, settling: &[Option<f64>]) -> Option<(usize, TakeKind, Fired)> {
    for (i, s) in settling.iter().enumerate() {
        if let Some((kind, why)) = gate.tick(DT, Some(&shot("a")), *s) {
            assert!(gate.waiting().is_none(), "a fired take no longer waits");
            return Some((i + 1, kind, why));
        }
        assert!(gate.waiting().is_some(), "tick {}", i + 1);
    }
    None
}

#[test]
fn a_take_fires_on_the_first_tick_preview_is_settled() {
    // Settling 0.2 → 0.65 over six ticks: the sixth reaches 0.6.
    let mut gate = waiting_cut();
    let rising = [0.2, 0.3, 0.4, 0.5, 0.59, 0.65, 0.9].map(Some);
    assert_eq!(
        run(&mut gate, &rising),
        Some((6, TakeKind::Cut, Fired::Settled))
    );
    assert_eq!(gate.dropped(), 0);
    // Exactly at the threshold counts as settled.
    let mut gate = waiting_cut();
    assert_eq!(
        run(&mut gate, &[Some(0.6)]),
        Some((1, TakeKind::Cut, Fired::Settled))
    );
}

#[test]
fn a_take_fires_at_the_max_wait_when_preview_never_settles() {
    // 15 ticks of 1/30 s are 0.5 s: the take fires on the 15th, not one
    // tick later because the sum of the dts rounds below 0.5.
    let mut gate = waiting_cut();
    let stuck = [Some(0.3); 40];
    assert_eq!(
        run(&mut gate, &stuck),
        Some((15, TakeKind::Cut, Fired::TimedOut))
    );
    // Settling on the 15th tick too: settled wins, as it is checked first.
    let mut gate = waiting_cut();
    let mut late = [Some(0.3); 15];
    late[14] = Some(0.8);
    assert_eq!(
        run(&mut gate, &late),
        Some((15, TakeKind::Cut, Fired::Settled))
    );
    // Whichever comes first: settling on tick 16 is too late.
    let mut gate = waiting_cut();
    let mut later = [Some(0.3); 16];
    later[15] = Some(0.8);
    assert_eq!(
        run(&mut gate, &later),
        Some((15, TakeKind::Cut, Fired::TimedOut))
    );
}

#[test]
fn the_wait_counts_dt_only() {
    let mut gate = SettleGate::new(Some(SettleRule {
        threshold: 0.6,
        max_wait: 0.25,
    }));
    assert!(gate.press(TakeKind::Auto, Some(&shot("a")), Some(0.0), true));
    // Ticks without time, or with time that is not a number, wait on.
    for dt in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(gate.tick(dt, Some(&shot("a")), Some(0.0)), None, "{dt}");
    }
    assert_eq!(gate.waiting().map(|w| w.waited), Some(0.0));
    // One long tick spends the whole wait.
    assert_eq!(
        gate.tick(0.3, Some(&shot("a")), Some(0.0)),
        Some((TakeKind::Auto, Fired::TimedOut))
    );
}

#[test]
fn a_shot_with_nothing_left_to_settle_fires_at_once() {
    // Its subject was lost, or it left the catalog: let the switcher's
    // checks have it.
    let mut gate = waiting_cut();
    assert_eq!(
        run(&mut gate, &[Some(0.2), None]),
        Some((2, TakeKind::Cut, Fired::Settled))
    );
}

#[test]
fn a_take_whose_preview_changes_is_dropped_as_a_fenced_take_is() {
    let mut gate = waiting_cut();
    assert_eq!(gate.tick(DT, Some(&shot("a")), Some(0.2)), None);
    assert_eq!(gate.tick(DT, Some(&shot("b")), Some(1.0)), None);
    assert!(gate.waiting().is_none());
    assert_eq!(gate.dropped(), 1);
    let (kind, reason) = gate.last_dropped().unwrap();
    assert_eq!(*kind, TakeKind::Cut);
    assert_eq!(
        reason,
        &CommandError::PreviewChanged {
            seen: Some(shot("a")),
            preview: Some(shot("b")),
        }
    );
    // An emptied preview drops it too; a gate with nothing waiting does
    // nothing.
    let mut gate = waiting_cut();
    assert_eq!(gate.tick(DT, None, None), None);
    assert_eq!(gate.dropped(), 1);
    assert_eq!(gate.tick(DT, None, None), None);
    assert_eq!(gate.dropped(), 1);
}

#[test]
fn a_second_press_forces_the_take_as_pressed() {
    // Space again: the cut is to run now, and nothing waits.
    let mut gate = waiting_cut();
    assert!(!gate.press(TakeKind::Cut, Some(&shot("a")), Some(0.2), true));
    assert!(gate.waiting().is_none());
    assert_eq!(gate.dropped(), 0, "forcing is not a drop");
    // Enter, then Space: the second press runs as pressed, the core's to
    // say how (a cut, here).
    let mut gate = SettleGate::new(Some(rule()));
    assert!(gate.press(TakeKind::Auto, Some(&shot("a")), Some(0.2), true));
    assert!(!gate.press(TakeKind::Cut, Some(&shot("a")), Some(0.2), true));
    assert!(gate.waiting().is_none());
    // The press after that is a first press again.
    assert!(gate.press(TakeKind::Cut, Some(&shot("a")), Some(0.2), true));
}

#[test]
fn a_press_of_another_shot_replaces_the_waiting_take() {
    let mut gate = waiting_cut();
    assert!(gate.press(TakeKind::Auto, Some(&shot("b")), Some(0.1), true));
    assert_eq!(
        gate.waiting(),
        Some(&WaitingTake::new(TakeKind::Auto, shot("b")))
    );
    // ... or goes at once when its own shot is settled.
    assert!(!gate.press(TakeKind::Cut, Some(&shot("c")), Some(0.9), true));
    assert!(gate.waiting().is_none());
    assert_eq!(gate.dropped(), 0, "a replaced take is not announced");
}

#[test]
fn only_an_idle_switcher_with_an_unsettled_shot_on_preview_waits() {
    let mut gate = SettleGate::new(Some(rule()));
    let a = shot("a");
    assert!(
        !gate.press(TakeKind::Cut, Some(&a), Some(0.6), true),
        "settled"
    );
    assert!(
        !gate.press(TakeKind::Cut, Some(&a), None, true),
        "nothing to settle"
    );
    assert!(
        !gate.press(TakeKind::Cut, None, Some(0.0), true),
        "nothing staged"
    );
    assert!(
        !gate.press(TakeKind::Auto, Some(&a), Some(0.0), false),
        "in a transition"
    );
    assert!(gate.waiting().is_none());
    assert!(gate.press(TakeKind::Auto, Some(&a), Some(0.59), true));
}

#[test]
fn without_a_rule_nothing_ever_waits() {
    let mut gate = SettleGate::new(None);
    assert_eq!(gate.rule(), None);
    for settling in [None, Some(0.0), Some(0.5), Some(1.0)] {
        assert!(!gate.press(TakeKind::Cut, Some(&shot("a")), settling, true));
        assert!(!gate.press(TakeKind::Auto, Some(&shot("a")), settling, true));
    }
    assert_eq!(gate.tick(DT, Some(&shot("a")), Some(0.0)), None);
    assert_eq!(gate, SettleGate::default());
}

#[test]
fn staging_forces_and_other_takes_cancel_the_waiting_take() {
    let mut gate = waiting_cut();
    assert_eq!(gate.force(Some(&shot("a"))), Some(TakeKind::Cut));
    assert!(gate.waiting().is_none());
    assert_eq!(gate.force(Some(&shot("a"))), None, "nothing waits");
    // Forcing is fenced as a tick is: a preview that changed drops it.
    let mut gate = waiting_cut();
    assert_eq!(gate.force(Some(&shot("b"))), None);
    assert_eq!(gate.dropped(), 1);
    assert!(matches!(
        gate.last_dropped(),
        Some((TakeKind::Cut, CommandError::PreviewChanged { .. }))
    ));
    let mut gate = waiting_cut();
    gate.cancel();
    assert!(gate.waiting().is_none());
    assert_eq!(gate.dropped(), 0, "a superseded take is not announced");
    assert_eq!(gate.tick(DT, Some(&shot("a")), Some(0.0)), None);
}

#[test]
fn refused_takes_are_counted_with_the_newest_reason() {
    let mut gate = SettleGate::new(Some(rule()));
    let unavailable = CommandError::Switch(ac_switcher::SwitchError::Unavailable(shot("a")));
    gate.refused(TakeKind::Auto, unavailable.clone());
    gate.refused(TakeKind::Cut, unavailable.clone());
    assert_eq!(gate.dropped(), 2);
    assert_eq!(gate.last_dropped(), Some(&(TakeKind::Cut, unavailable)));
    assert_eq!(TakeKind::Cut.name(), "cut");
    assert_eq!(TakeKind::Auto.name(), "auto");
}
