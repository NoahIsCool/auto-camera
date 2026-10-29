//! Tests of the drift corrector: blocks in the dead band untouched, a
//! drifting input put on the master clock without a seam, breaks that end
//! the resampling, limits on the correction, and bad blocks and formats.

use super::*;

/// Sample rate of the tests.
const RATE: u32 = 48_000;

/// Frames per block (10 ms).
const BLOCK: usize = 480;

/// Block `k` of a stereo signal that differs on every frame and channel.
fn samples(k: usize) -> Vec<f32> {
    (k * BLOCK..(k + 1) * BLOCK)
        .flat_map(|n| {
            let x = (n % 9_973) as f32 / 9_973.0 - 0.5;
            [x, -0.5 * x]
        })
        .collect()
}

/// The PTS of frame `frame` of an input whose clock runs `ppm` fast.
fn pts(frame: usize, ppm: f64) -> Duration {
    Duration::from_secs_f64(frame as f64 / (f64::from(RATE) * (1.0 + ppm * 1e-6)))
}

/// A corrector of stereo 48 kHz audio with the default settings.
fn corrector() -> DriftCorrector {
    DriftCorrector::new(RATE, 2, DriftSettings::default()).unwrap()
}

/// Block `k` of a signal that is mostly silence, with a block of
/// [`samples`] every tenth: what the corrector does with the timing does not
/// depend on the sound, and silence is cheap to resample.
fn sparse(k: usize) -> Vec<f32> {
    if k.is_multiple_of(10) {
        samples(k % 50)
    } else {
        vec![0.0; BLOCK * 2]
    }
}

/// The frames `outcome` passes on for `block`.
fn frames(outcome: &Corrected, block: &[f32]) -> usize {
    match outcome {
        Corrected::Unchanged => block.len() / 2,
        Corrected::Resampled(r) => r.samples.len() / 2,
    }
}

#[test]
fn blocks_in_the_dead_band_pass_through_unchanged() {
    // An input on the master clock (PTS counted from its samples): ten
    // minutes, every block left as it is.
    let mut c = corrector();
    let block = samples(0);
    for k in 0..60_000 {
        let at = Duration::from_nanos((k * BLOCK) as u64 * 1_000_000_000 / u64::from(RATE));
        assert_eq!(c.process(at, &block), Corrected::Unchanged, "block {k}");
    }
    let stats = c.stats();
    assert_eq!(
        (stats.correction_ppm, stats.resets, stats.resampling),
        (0.0, 0, false)
    );
    assert!(stats.drift_ppm.abs() < 1e-3, "{stats:?}");
    // A drift inside the band (1 ppm) is left alone too.
    let mut c = corrector();
    for k in 0..30_000 {
        let outcome = c.process(pts(k * BLOCK, 1.0), &block);
        assert_eq!(outcome, Corrected::Unchanged, "block {k}");
    }
    assert!((c.stats().drift_ppm - 1.0).abs() < 0.1, "{:?}", c.stats());
}

#[test]
fn a_drifting_input_is_put_on_the_master_clock() {
    for ppm in [100.0, -100.0] {
        let mut c = corrector();
        let (mut out, mut worst) = (0usize, 0f64);
        for k in 0..30_000 {
            let block = sparse(k);
            out += frames(&c.process(pts(k * BLOCK, ppm), &block), &block);
            // What came out against the master-clock time the input covers,
            // less the frames the resampler holds back.
            if k > 0 {
                let due = pts((k + 1) * BLOCK, ppm).as_secs_f64() * f64::from(RATE);
                worst = worst.max((out as f64 + HALF_TAPS as f64 - due).abs());
            }
        }
        let stats = c.stats();
        assert!(stats.resampling, "{stats:?}");
        // The correction cancels the drift, less what is left of the timing
        // error being taken up.
        assert!((stats.correction_ppm + ppm).abs() < 2.0, "{ppm}: {stats:?}");
        assert!((stats.drift_ppm - ppm).abs() < 0.5, "{ppm}: {stats:?}");
        // Five minutes of 100 ppm is 30 ms; the output kept within 3 ms.
        assert!(worst < 3e-3 * f64::from(RATE), "{ppm}: {worst} frames off");
    }
}

#[test]
fn the_resampling_starts_without_a_seam() {
    // The block that leaves the dead band starts with the frame after the
    // last one passed through, bit for bit.
    let mut c = corrector();
    for k in 0..10_000 {
        let block = samples(k);
        match c.process(pts(k * BLOCK, 300.0), &block) {
            Corrected::Unchanged => continue,
            Corrected::Resampled(r) => {
                assert!(k > 0, "not on the first block");
                assert_eq!(r.behind, 0.0);
                assert_eq!(r.samples[..2], block[..2]);
                // It holds back the frames the kernel reaches.
                assert_eq!(r.samples.len() / 2, BLOCK - HALF_TAPS);
                return;
            }
        }
    }
    panic!("never started resampling");
}

#[test]
fn a_break_ends_the_resampling_and_the_new_epoch_passes_through() {
    let mut c = corrector();
    let mut k = 0;
    while !c.stats().resampling || k < 6_000 {
        c.process(pts(k * BLOCK, 400.0), &samples(k));
        k += 1;
    }
    assert!(c.stats().correction_ppm < -100.0, "{:?}", c.stats());
    // A reconnection two seconds on: the held frames are let go as they
    // are, ahead of the new block, which passes as it is.
    let block = samples(k);
    let at = pts(k * BLOCK, 400.0) + Duration::from_secs(2);
    let Corrected::Resampled(r) = c.process(at, &block) else {
        panic!("the held frames come first");
    };
    let held = r.behind as usize;
    assert!((HALF_TAPS - 1..=HALF_TAPS + 1).contains(&held), "{held}");
    assert_eq!(r.samples.len(), (held + BLOCK) * 2);
    assert_eq!(r.samples[held * 2..], block[..]);
    let previous = samples(k - 1);
    assert_eq!(r.samples[..held * 2], previous[(BLOCK - held) * 2..]);
    let stats = c.stats();
    assert_eq!(
        (stats.resets, stats.resampling, stats.correction_ppm),
        (1, false, 0.0)
    );
    // The new epoch is drift-free and is not resampled.
    for j in 1..3_000 {
        let at = at + Duration::from_secs_f64((j * BLOCK) as f64 / f64::from(RATE));
        assert_eq!(c.process(at, &samples(k + j)), Corrected::Unchanged);
    }
}

#[test]
fn the_correction_is_held_to_its_limit_and_slew() {
    // A clock 1 % fast: far beyond any crystal, so the correction stops at
    // its limit, reached no faster than the slew.
    let settings = DriftSettings::default();
    let mut c = corrector();
    let mut last = 0.0;
    let per_block = settings.max_slew_ppm_per_s * BLOCK as f64 / f64::from(RATE);
    for k in 0..6_000 {
        c.process(pts(k * BLOCK, 10_000.0), &sparse(k));
        let now = c.stats().correction_ppm;
        assert!(
            (now - last).abs() <= per_block + 1e-9,
            "block {k}: {last} to {now}"
        );
        assert!(now.abs() <= settings.max_correction_ppm + 1e-9);
        last = now;
    }
    assert_eq!(last, -settings.max_correction_ppm);
}

#[test]
fn bad_blocks_are_left_alone_and_not_measured() {
    let mut c = corrector();
    assert_eq!(c.process(Duration::ZERO, &[]), Corrected::Unchanged);
    assert_eq!(
        c.process(Duration::ZERO, &[0.1, 0.2, 0.3]),
        Corrected::Unchanged
    );
    // Neither started an epoch: this block does.
    c.process(Duration::from_secs(5), &samples(0));
    c.process(Duration::from_millis(5_010), &samples(1));
    assert_eq!(c.stats().resets, 0);
    // A block with a NaN passes as it is while nothing is resampled.
    let mut nan = samples(2);
    nan[7] = f32::NAN;
    assert_eq!(
        c.process(Duration::from_millis(5_020), &nan),
        Corrected::Unchanged
    );
}

#[test]
fn bad_formats_are_refused() {
    let ok = DriftSettings::default();
    assert_eq!(
        DriftCorrector::new(RATE, 0, ok).unwrap_err(),
        DriftError::Channels(0)
    );
    assert_eq!(
        DriftCorrector::new(RATE, 9, ok).unwrap_err(),
        DriftError::Channels(9)
    );
    assert_eq!(DriftCorrector::new(0, 2, ok).unwrap_err(), DriftError::Rate);
    let settings = DriftSettings {
        settle: Duration::ZERO,
        ..ok
    };
    assert!(DriftCorrector::new(RATE, 2, settings).is_err());
    assert_eq!(corrector().format(), (RATE, 2));
    assert!(
        DriftError::Channels(9)
            .to_string()
            .contains("1 to 8 channels")
    );
}
