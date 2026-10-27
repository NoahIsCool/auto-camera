//! Property tests: the switcher's health guard and tally hold for any
//! sequence of presses.
//!
//! Each case is a random sequence of operator presses and clock ticks over
//! five shots (`s0..s4`), with a random set of shots that are never
//! available (dead) and, per step, a random set of further shots that are
//! unavailable at that step only, so a shot can fail between the press that
//! queued a take of it and the end of the transition, when the take fires.
//!
//! "Program shows a shot" means the shot is on air or on its way: it is the
//! program shot, or the incoming shot of a running transition. After every
//! step:
//!
//! * a shot only starts being shown when it is available at that step, so
//!   no dead shot is ever shown and no dead shot is ever lit;
//! * tally equals exposure: a shot is lit red exactly when its exposure is
//!   above zero, and green exactly when it is on preview and not red;
//! * exposure, worked out from the buses, equals each shot's share of the
//!   picture [`Switcher::output`] describes (the base, less the overlay's
//!   opacity, and the overlay);
//! * a refused press changes nothing; nothing is queued outside a
//!   transition; a dropped take was dropped for its own shot, which is
//!   unavailable at that step and stays on preview.

use std::collections::HashSet;

use ac_core::ShotId;
use proptest::prelude::*;

use super::{Output, SwitchError, Switcher, Transition, TransitionKind};

/// Shots in play.
const SHOTS: usize = 5;

/// One press or tick.
#[derive(Debug, Clone)]
enum Op {
    /// Stage shot `i`.
    Stage(usize),
    /// Cut.
    Cut,
    /// Auto.
    Auto,
    /// Move the T-bar to a position, possibly out of range.
    TBar(f64),
    /// Advance the clock by `dt` seconds, possibly zero.
    Tick(f64),
    /// Select a transition kind and duration.
    Select(TransitionKind, f64),
}

/// One step: a press and which further shots are unavailable meanwhile.
#[derive(Debug, Clone)]
struct Step {
    /// The press.
    op: Op,
    /// Bit `i` set: shot `i` is unavailable at this step.
    failing: u8,
}

/// The id of shot `i`.
fn shot(i: usize) -> ShotId {
    ShotId(format!("s{i}"))
}

/// Any transition kind.
fn arb_kind() -> impl Strategy<Value = TransitionKind> {
    prop_oneof![
        Just(TransitionKind::Cut),
        Just(TransitionKind::Mix),
        Just(TransitionKind::Dip),
    ]
}

/// Any press, ticks most often so transitions run to their ends.
fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..SHOTS).prop_map(Op::Stage),
        1 => Just(Op::Cut),
        2 => Just(Op::Auto),
        1 => (-0.2f64..1.2).prop_map(Op::TBar),
        6 => prop_oneof![4 => 0.0f64..0.5, 1 => Just(0.0)].prop_map(Op::Tick),
        1 => (arb_kind(), 0.05f64..2.0).prop_map(|(k, d)| Op::Select(k, d)),
    ]
}

prop_compose! {
    /// A step that fails each shot with probability about 1/4.
    fn arb_step()(op in arb_op(), a in any::<u8>(), b in any::<u8>()) -> Step {
        Step { op, failing: a & b }
    }
}

/// The shots program shows: the program shot and a running transition's
/// incoming one.
fn shown(s: &Switcher) -> HashSet<ShotId> {
    let mut shots: HashSet<ShotId> = s.program().cloned().into_iter().collect();
    if s.in_transition() {
        shots.extend(s.preview().cloned());
    }
    shots
}

/// The share of the picture `out` gives `shot`, before the dip to black.
fn picture_share(out: &Output, shot: &ShotId) -> f64 {
    let alpha = out.overlay.as_ref().map_or(0.0, |(_, a)| *a);
    let base = if out.base.as_ref() == Some(shot) {
        1.0 - alpha
    } else {
        0.0
    };
    let overlay = match &out.overlay {
        Some((s, a)) if s == shot => *a,
        _ => 0.0,
    };
    base + overlay
}

/// Applies `op`; the result of a press, `Ok` for a tick.
fn apply(
    s: &mut Switcher,
    op: &Op,
    available: &dyn Fn(&ShotId) -> bool,
) -> Result<(), SwitchError> {
    match op {
        Op::Stage(i) => {
            s.set_preview(shot(*i));
            Ok(())
        }
        Op::Cut => s.cut(available),
        Op::Auto => s.auto(available),
        Op::TBar(p) => s.set_tbar(*p, available),
        Op::Tick(dt) => {
            s.tick(*dt, available);
            Ok(())
        }
        Op::Select(kind, duration) => s.set_transition(Transition {
            kind: *kind,
            duration: *duration,
        }),
    }
}

/// Checks every invariant of the module docs after one step from `before`
/// to `after`, where `available` held.
fn check(
    before: &Switcher,
    after: &Switcher,
    result: &Result<(), SwitchError>,
    available: &dyn Fn(&ShotId) -> bool,
    dead: u8,
) -> Result<(), TestCaseError> {
    if result.is_err() {
        prop_assert_eq!(before, after, "a refused press changed the switcher");
    }
    let was = shown(before);
    for s in shown(after).difference(&was) {
        prop_assert!(available(s), "{:?} went on air while unavailable", s);
    }
    let out = after.output();
    for i in 0..SHOTS {
        let id = shot(i);
        let exposure = after.exposure(&id);
        let tally = after.tally(&id);
        prop_assert!((0.0..=1.0).contains(&exposure), "exposure {}", exposure);
        prop_assert!(
            (exposure - picture_share(&out, &id)).abs() < 1e-12,
            "{:?}: exposure {} but picture {:?}",
            id,
            exposure,
            out
        );
        prop_assert_eq!(tally.program, exposure > 0.0, "{:?} red lamp", &id);
        prop_assert_eq!(
            tally.preview,
            !tally.program && after.preview() == Some(&id),
            "{:?} green lamp",
            &id
        );
        if dead & (1 << i) != 0 {
            prop_assert!(!tally.program, "dead {:?} is lit", &id);
            prop_assert!(!shown(after).contains(&id), "dead {:?} is shown", &id);
        }
    }
    if let Some(q) = after.queued() {
        prop_assert!(after.in_transition(), "queued {:?} while idle", q);
        prop_assert!(after.preview() != Some(&q.shot), "queued the incoming shot");
    }
    let dropped = after.dropped_takes() - before.dropped_takes();
    prop_assert!(dropped <= 1);
    if dropped == 1 {
        let Some(SwitchError::Unavailable(s)) = after.last_dropped() else {
            return Err(TestCaseError::fail("a drop without its reason"));
        };
        prop_assert!(!available(s), "dropped {:?}, which is available", s);
        prop_assert_eq!(after.preview(), Some(s), "a dropped shot stays staged");
        prop_assert_eq!(before.queued().map(|q| (&q.shot, q.take)), Some((s, true)));
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn program_never_shows_an_unavailable_shot_and_tally_equals_exposure(
        dead in any::<u8>().prop_map(|m| m & 0b1_1000),
        steps in prop::collection::vec(arb_step(), 1..80),
    ) {
        let mut s = Switcher::new();
        for step in &steps {
            let unavailable = dead | step.failing;
            let available = |id: &ShotId| {
                (0..SHOTS).any(|i| shot(i) == *id && unavailable & (1 << i) == 0)
            };
            let before = s.clone();
            let result = apply(&mut s, &step.op, &available);
            check(&before, &s, &result, &available, dead)?;
        }
    }
}
