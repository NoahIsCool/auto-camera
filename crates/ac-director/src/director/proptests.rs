//! Property tests: the director's safety invariants hold for arbitrary input.
//!
//! Each case is a random mode and profile and a random sequence of ticks.
//! Every tick has its own random catalog (sources, kinds, crops, qualities
//! including NaN and out-of-range values, availability, recency), a random
//! program and preview (a listed shot, nothing, or an id that is not listed),
//! a random dwell, a random transition flag and a random clock step, which
//! may go backwards. Candidates are at priority 0, as with one priority
//! everywhere, except in the case that draws a random priority level for
//! each: priority only moves scores, so the same invariants must hold.

use std::time::Duration;

use ac_core::{Rect, ShotId, SourceId};
use proptest::prelude::*;

use super::{Director, DirectorInput};
use crate::{
    CandidateKind, ContentProfile, DirectorConfig, DirectorDecision, DirectorMode, Reason,
    ShotCandidate, is_jump_cut,
};

/// One random tick.
#[derive(Debug, Clone)]
struct Step {
    /// Candidates, with ids `s0..sN` in order.
    candidates: Vec<ShotCandidate>,
    /// Index of the program shot; past the end means none or unknown.
    program: usize,
    /// Index of the preview shot; past the end means none or unknown.
    preview: usize,
    /// Clock step in milliseconds; negative steps go backwards.
    dt_ms: i64,
    /// How long program has been on air, in milliseconds.
    on_air_ms: u64,
    /// Whether a transition is running.
    in_transition: bool,
}

/// Any quality, including the invalid values the director must tolerate.
fn arb_quality() -> impl Strategy<Value = f32> {
    prop_oneof![
        6 => 0.0f32..=1.0,
        1 => -1.0f32..2.0,
        1 => Just(f32::NAN),
    ]
}

prop_compose! {
    /// A candidate with a placeholder id.
    fn arb_candidate()(
        source in 0u8..3,
        is_wide in any::<bool>(),
        x in -0.1f64..0.9,
        y in -0.1f64..0.9,
        w in -0.05f64..0.8,
        h in -0.05f64..0.8,
        quality in arb_quality(),
        available in prop::bool::weighted(0.8),
        used_ms in prop::option::of(0u64..60_000),
    ) -> ShotCandidate {
        let kind = if is_wide { CandidateKind::Wide } else { CandidateKind::Subject };
        let crop = if is_wide { Rect::FULL } else { Rect::new(x, y, w, h) };
        ShotCandidate {
            id: ShotId(String::new()),
            source: SourceId(format!("cam{source}")),
            kind,
            crop,
            quality,
            available,
            recently_used_at: used_ms.map(Duration::from_millis),
            priority: 0,
        }
    }
}

prop_compose! {
    /// A random tick with up to eight candidates.
    fn arb_step()(
        mut candidates in prop::collection::vec(arb_candidate(), 0..8),
        program in 0usize..10,
        preview in 0usize..10,
        dt_ms in prop_oneof![9 => 0i64..800, 1 => -500i64..0],
        on_air_ms in 0u64..30_000,
        in_transition in prop::bool::weighted(0.15),
    ) -> Step {
        for (i, c) in candidates.iter_mut().enumerate() {
            c.id = ShotId(format!("s{i}"));
        }
        Step { candidates, program, preview, dt_ms, on_air_ms, in_transition }
    }
}

prop_compose! {
    /// [`arb_step`] with a random priority level on every candidate, mostly
    /// in the range a studio config gives (0 to 4 levels).
    fn arb_prioritised_step()(
        mut step in arb_step(),
        levels in prop::collection::vec(prop_oneof![8 => 0u8..=4, 1 => any::<u8>()], 8),
    ) -> Step {
        for (c, level) in step.candidates.iter_mut().zip(levels) {
            c.priority = level;
        }
        step
    }
}

/// Any mode.
fn arb_mode() -> impl Strategy<Value = DirectorMode> {
    prop_oneof![
        Just(DirectorMode::Off),
        Just(DirectorMode::Suggest),
        Just(DirectorMode::Auto),
    ]
}

/// Any profile.
fn arb_profile() -> impl Strategy<Value = ContentProfile> {
    prop_oneof![Just(ContentProfile::Talk), Just(ContentProfile::Music)]
}

/// The id at `index`, or `None` / an unlisted id past the end.
fn pick(candidates: &[ShotCandidate], index: usize) -> Option<ShotId> {
    match index.checked_sub(candidates.len()) {
        None => Some(candidates[index].id.clone()),
        Some(0) => Some(ShotId("unlisted".into())),
        Some(_) => None,
    }
}

/// Checks every documented invariant of one decision.
fn check(
    config: &DirectorConfig,
    step: &Step,
    program: Option<&ShotId>,
    decision: &DirectorDecision,
) -> Result<(), TestCaseError> {
    if config.mode == DirectorMode::Off {
        prop_assert_eq!(decision, &DirectorDecision::off());
        return Ok(());
    }
    let Some(s) = &decision.suggestion else {
        prop_assert!(decision.take.is_none());
        prop_assert_eq!(&decision.reason, &Reason::NoCandidate);
        return Ok(());
    };
    prop_assert!(Some(s) != program, "suggested program shot {:?}", s);
    // The director uses the first entry of an id.
    let entry = step.candidates.iter().find(|c| &c.id == s);
    prop_assert!(entry.is_some(), "suggested unlisted shot {:?}", s);
    let entry = entry.unwrap();
    prop_assert!(entry.available, "suggested unavailable shot {:?}", s);
    prop_assert!(entry.has_valid_crop());
    prop_assert!(entry.quality() >= config.fallback_quality);
    if let Some(p) = program.and_then(|p| step.candidates.iter().find(|c| &c.id == p)) {
        prop_assert!(
            !is_jump_cut(p, entry, config.jump_cut_iou),
            "jump cut to {:?}",
            s
        );
    }
    if let Some(take) = &decision.take {
        prop_assert_eq!(config.mode, DirectorMode::Auto);
        prop_assert!(!step.in_transition, "took during a transition");
        prop_assert_eq!(&take.shot, s);
    }
    Ok(())
}

/// Runs a director with the defaults of `mode` and `profile` over `steps`,
/// checking every decision.
fn run_checked(
    mode: DirectorMode,
    profile: ContentProfile,
    steps: &[Step],
) -> Result<(), TestCaseError> {
    let config = DirectorConfig::new(mode, profile);
    let mut director = Director::new(config.clone()).unwrap();
    let mut now = Duration::from_secs(100);
    for step in steps {
        now = if step.dt_ms >= 0 {
            now + Duration::from_millis(step.dt_ms.unsigned_abs())
        } else {
            now.saturating_sub(Duration::from_millis(step.dt_ms.unsigned_abs()))
        };
        let program = pick(&step.candidates, step.program);
        let preview = pick(&step.candidates, step.preview);
        let decision = director.tick(&DirectorInput {
            now,
            program: program.as_ref(),
            preview: preview.as_ref(),
            program_on_air: Duration::from_millis(step.on_air_ms),
            in_transition: step.in_transition,
            candidates: &step.candidates,
        });
        check(&config, step, program.as_ref(), &decision)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn never_suggests_program_or_unavailable_shots(
        mode in arb_mode(),
        profile in arb_profile(),
        steps in prop::collection::vec(arb_step(), 1..40),
    ) {
        run_checked(mode, profile, &steps)?;
    }

    #[test]
    fn priorities_never_break_the_safety_rules(
        mode in arb_mode(),
        profile in arb_profile(),
        steps in prop::collection::vec(arb_prioritised_step(), 1..40),
    ) {
        run_checked(mode, profile, &steps)?;
    }

    #[test]
    fn same_inputs_give_same_decisions(
        mode in arb_mode(),
        steps in prop::collection::vec(arb_step(), 1..20),
    ) {
        let config = DirectorConfig::new(mode, ContentProfile::Music);
        let mut a = Director::new(config.clone()).unwrap();
        let mut b = Director::new(config).unwrap();
        let mut now = Duration::ZERO;
        for step in &steps {
            now += Duration::from_millis(step.dt_ms.unsigned_abs());
            let program = pick(&step.candidates, step.program);
            let input = DirectorInput {
                now,
                program: program.as_ref(),
                preview: None,
                program_on_air: Duration::from_millis(step.on_air_ms),
                in_transition: step.in_transition,
                candidates: &step.candidates,
            };
            prop_assert_eq!(a.tick(&input), b.tick(&input));
        }
    }
}
