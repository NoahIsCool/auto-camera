//! Cross-PR tests: a host-time excursion ahead of four buffers or more,
//! close to the drift correction's limit, on a board whose audio runs
//! ahead of the video: the silence that makes it up lifts the pacer's
//! lead over [`AUDIO_MAX_LEAD`], and what the pacer drops there it gives
//! back once that silence is out again.
//!
//! # Theory
//!
//! The device input reports such an excursion as one glitch, with no
//! audio missing, its PTS back on the old timeline after it. The drift
//! correction cannot tell it from a step while it lasts: its fourth block
//! confirms it and it is made up with silence, then taken back by a cut
//! once the fourth block back confirms the way back ([`ac_audio::drift`]).
//! In between, the board's audio at hand is ahead of the video by the
//! stray as well as by the board's own lead, which is set by where in the
//! board's IO cycle the output started: above zero, a stray of 60 to
//! 99 ms takes it over the limit, and the pacer drops its oldest blocks.
//! It keeps them as credit and gives them back as silence once the cut
//! has brought the lead down again ([`crate::audio`]), so the board's
//! audio ends where its twin's is. The correction's silence comes in one
//! block with the board's audio after it, which the pacer may drop whole;
//! the cut then leaves it more than its slack short, and the silence that
//! fills that gap is the credit given back.
//!
//! # Invariants checked
//!
//! For buffers of 128, 512, 1024 and 2048 frames at 48 kHz, four or ten
//! buffers 60 to 99 ms ahead, and the output started 7, 15 or 30 ticks
//! after the device input opened:
//!
//! * one glitch reported, and no break;
//! * the pacer's silence equal to the audio it dropped, which over a
//!   third of the runs do;
//! * the lead its twin's within 0.1 ms, from a second after the excursion.

use super::*;

#[test]
fn a_stray_whose_silence_lifts_the_lead_over_the_limit_leaves_no_lasting_offset() {
    let at = 5.0;
    let (mut limited, mut runs) = (0, 0);
    for frames in [128u64, 512, 1024, 2048] {
        for output_at in [7, 15, FPS] {
            let twin = play_from(Device::new(RATE, frames), 8, output_at);
            untouched(&twin, "twin");
            for (buffers, ms) in [4, 10]
                .into_iter()
                .flat_map(|n| [60.0, 75.0, 85.0, 90.0, 95.0, 99.0].map(|ms| (n, ms)))
            {
                let off = [(at, Event::Off(buffers, ms / 1000.0))];
                let run = play_from(Device::new(RATE, frames).with(&off), 8, output_at);
                let apart = run.apart(&twin, at + 1.0);
                let what = format!(
                    "{buffers} buffers +{ms} ms at {frames} frames, output at tick {output_at}: \
                     {apart:.3} ms apart, {:?}, {:?}, {:?}",
                    run.found, run.view, run.pacer
                );
                assert!(
                    matches!(run.found[..], [Discontinuity::Glitch { .. }]),
                    "{what}"
                );
                assert_eq!(run.view.drift_resets, 0, "{what}");
                assert_eq!(run.pacer.silence, run.pacer.dropped, "{what}");
                assert!(apart <= 0.1, "{what}");
                (limited, runs) = (limited + usize::from(run.pacer.dropped > 0), runs + 1);
            }
        }
    }
    assert!(
        3 * limited >= runs,
        "only {limited} of {runs} runs met the lead limit"
    );
}
