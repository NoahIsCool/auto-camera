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

/// A fixed sequence of jitter in `-amount..amount` seconds.
fn jitter(amount: f64) -> impl FnMut() -> f64 {
    let mut state = 0x2545_f491_u32;
    move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (f64::from(state) / f64::from(u32::MAX) * 2.0 - 1.0) * amount
    }
}

/// Plays two minutes of 10 ms blocks of an input `ppm` fast whose PTS
/// jitter by up to half a millisecond and step by `step` seconds at one
/// minute (below zero: audio lost); returns the corrector and the output's
/// worst distance, from a tenth of a second after the step, from where the
/// PTS put it, in frames.
fn step_at_a_minute(ppm: f64, step: f64) -> (DriftCorrector, f64) {
    let mut c = corrector();
    let mut noise = jitter(0.000_5);
    let (mut out, mut worst) = (0usize, 0f64);
    let rate = f64::from(RATE);
    for k in 0..12_000 {
        let moved = if k >= 6_000 { -step } else { 0.0 };
        let block = sparse(k);
        let at = pts(k * BLOCK, ppm).as_secs_f64() + moved;
        let outcome = c.process(Duration::from_secs_f64(at + 1.0 + noise()), &block);
        out += frames(&outcome, &block);
        if k > 6_010 {
            // Where the PTS put the end of this block, once a cut (whole
            // blocks while it lasts) is done.
            let due = (pts((k + 1) * BLOCK, ppm).as_secs_f64() + moved) * rate;
            let held = if c.stats().resampling {
                HALF_TAPS as f64
            } else {
                0.0
            };
            worst = worst.max((out as f64 + held - due).abs());
        }
    }
    (c, worst)
}

#[test]
fn a_step_in_the_pts_is_made_up_at_once_and_not_taken_for_drift() {
    // 20 or 50 ms lost (the PTS step forward) is filled with that much
    // silence; a PTS that steps back 30 ms has that much cut. Neither is
    // read as drift: a drift-free input is not resampled, and one 100 ppm
    // fast is resampled as before, the silence through the resampler.
    for ppm in [0.0, 100.0] {
        for step in [-0.02, -0.05, 0.03] {
            let (c, worst) = step_at_a_minute(ppm, step);
            let stats = c.stats();
            assert!(
                (stats.drift_ppm - ppm).abs() < 3.0,
                "{ppm}, {step}: {stats:?}"
            );
            assert_eq!(stats.resampling, ppm != 0.0, "{ppm}, {step}: {stats:?}");
            assert_eq!((stats.steps, stats.resets), (1, 0), "{ppm}, {step}");
            if ppm != 0.0 {
                // Less what is left of the step's own jitter being taken
                // up: half a millisecond over the minute's settling.
                let cancels = (stats.correction_ppm + ppm).abs();
                assert!(cancels < 5.0, "{ppm}, {step}: {stats:?}");
            }
            // Within the PTS's jitter.
            let frames_off = 0.001 * f64::from(RATE);
            assert!(worst < frames_off, "{ppm}, {step}: {worst} frames off");
        }
    }
}
#[test]
fn a_first_block_stamped_off_does_not_move_the_stream() {
    // Ten minutes of a drift-free input with exact PTS but for the first
    // block's, tens of milliseconds off: what comes out keeps the input's
    // own timing, within a couple of milliseconds.
    for off in [0.015, 0.03, -0.03, 0.06] {
        let mut c = corrector();
        let mut out = 0usize;
        let rate = f64::from(RATE);
        for k in 0..60_000 {
            let t = 1.0 + (k * BLOCK) as f64 / rate + if k == 0 { off } else { 0.0 };
            let block = sparse(k);
            out += frames(&c.process(Duration::from_secs_f64(t), &block), &block);
        }
        let held = if c.stats().resampling { HALF_TAPS } else { 0 };
        let lead = (out + held) as f64 - (60_000 * BLOCK) as f64;
        assert!(
            lead.abs() < 0.002 * rate,
            "{off}: {lead} frames, {:?}",
            c.stats()
        );
    }
}

#[test]
fn a_fill_and_a_cut_are_placed_exactly() {
    // Exact PTS, so the steps are exactly 25 ms (1200 frames): two seconds
    // in, the PTS step forward, and the block comes after that much
    // silence; a second later they step back, and as much is cut from the
    // blocks that follow, two and a half of them.
    let mut c = corrector();
    let at = |frame: usize| Duration::from_nanos(frame as u64 * 1_000_000_000 / u64::from(RATE));
    let mut frame = 0;
    for k in 0..200 {
        assert_eq!(c.process(at(frame), &samples(k)), Corrected::Unchanged);
        frame += BLOCK;
    }
    let block = samples(200);
    let Corrected::Resampled(r) = c.process(at(frame + 1_200), &block) else {
        panic!("the silence comes first");
    };
    assert_eq!(r.behind, 1_200.0);
    assert_eq!(r.samples.len(), (1_200 + BLOCK) * 2);
    assert!(r.samples[..2_400].iter().all(|&v| v == 0.0));
    assert_eq!(r.samples[2_400..], block[..]);
    frame += BLOCK;
    for k in 201..300 {
        assert_eq!(
            c.process(at(frame + 1_200), &samples(k)),
            Corrected::Unchanged
        );
        frame += BLOCK;
    }
    for (k, behind) in [(300, -480.0), (301, -480.0), (302, -240.0)] {
        let block = samples(k);
        let Corrected::Resampled(r) = c.process(at(frame), &block) else {
            panic!("block {k} is cut");
        };
        assert_eq!(r.behind, behind, "block {k}");
        assert!(
            r.samples[..] == block[(-behind) as usize * 2..],
            "block {k}"
        );
        frame += BLOCK;
    }
    assert_eq!(c.process(at(frame), &samples(303)), Corrected::Unchanged);
    let stats = c.stats();
    assert_eq!((stats.steps, stats.resets, stats.resampling), (2, 0, false));
}
