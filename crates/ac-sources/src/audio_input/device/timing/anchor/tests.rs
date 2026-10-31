//! Reference-history invariants: outliers do not move the phase center,
//! confirmed nested jumps retain only their final epoch, and tiny buffers
//! cannot grow the history without bound.

use super::*;

/// Adds an independent callback at `at_ms`, with phase `phase_ms`.
fn observe(anchor: &mut Anchor, at_ms: i128, phase_ms: i128, provisional: bool) {
    anchor.elapsed = at_ms * 1_000_000;
    anchor.observe(anchor.elapsed + phase_ms * 1_000_000, provisional);
}

/// A short large glitch is excluded before a subsequent real rewind is
/// classified, even before a full second of reference data exists.
#[test]
fn large_glitches_cannot_teach_a_large_noise_floor() {
    let mut anchor = Anchor::default();
    assert_eq!(anchor.measure(123, -456), (-456, Noise::default()));
    for at in 0..3 {
        observe(&mut anchor, at * 10, 1000, false);
    }
    for at in 3..7 {
        observe(&mut anchor, at * 10, 1030, true);
    }
    anchor.close(30_000_000, Noise::default());
    observe(&mut anchor, 70, 1000, false);
    anchor.elapsed = 80_000_000;
    let (offset, noise) = anchor.measure(1_065_000_000, -15_000_000);
    assert_eq!(offset, -15_000_000);
    assert_eq!(noise, Noise::default());
}

/// A lasting second event confirms only its own level, so the preceding
/// rewind cannot inflate the new reference's noise estimate.
#[test]
fn confirmation_excludes_the_previous_excursions_level() {
    let mut anchor = Anchor::default();
    for at in 0..10 {
        observe(&mut anchor, at * 10, 1000, false);
    }
    for at in 10..30 {
        observe(&mut anchor, at * 10, 985, true);
    }
    for at in 30..90 {
        observe(&mut anchor, at * 10, 1010, true);
    }
    anchor.elapsed = 900_000_000;
    anchor.confirm(Duration::from_millis(600));
    assert_eq!(anchor.measure(1_910_000_000, 0), (0, Noise::default()));
    assert!(anchor.pending.is_empty());
}

/// The history has both a duration bound and a sample-count bound; an
/// undecided excursion has the same bounds as a calm stream.
#[test]
fn both_histories_stay_bounded() {
    let mut anchor = Anchor::default();
    for provisional in [false, true] {
        for at in 0..(CAPACITY as i128 * 2) {
            anchor.elapsed = at;
            anchor.observe(10_000 + at, provisional);
        }
        let history = if provisional {
            &anchor.pending
        } else {
            &anchor.calm
        };
        assert_eq!(history.len(), CAPACITY);
    }
    anchor.discard();
    observe(&mut anchor, 3000, 1000, false);
    assert_eq!(anchor.calm.len(), 1);
    assert!(anchor.pending.is_empty());
}
