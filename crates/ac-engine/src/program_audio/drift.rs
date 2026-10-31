//! Drift correction of program audio from an audio input: an input on its
//! own sample clock (a soundboard's USB interface, a few tens of ppm off
//! the engine's clock) put back on the engine's clock before the output's
//! pacer, so that a show of any length stays in sync without the pacer
//! inserting silence or cutting audio (spec C.3, "±20 ms, drift-free over
//! 3 h").
//!
//! # What is corrected
//!
//! Each audio input's slot owns an [`InputDrift`]. The blocks the program
//! takes from its origin, when the origin is an audio input, are handed to
//! it once they are picked to stereo, in delivery order; it measures the
//! input's drift from their PTS (master-clock times) against their sample
//! counts and, outside a dead band, resamples them a few ppm
//! ([`ac_audio::DriftCorrector`]). Inputs that are not the origin are
//! drained but not corrected: nothing hears them.
//!
//! A video source's embedded audio is never touched. It comes out of the
//! same stream as its pictures, stamped on the same clock, so it has no
//! drift of its own against them; drift between a camera and the engine
//! (video-side drift) and locking devices to a house clock (genlock) are
//! not what this corrects.
//!
//! # Steps, breaks and settings
//!
//! A step in the PTS is never taken for drift. One larger than the pacer's
//! slack ([`AUDIO_SLACK`]) is a break: a reconnection (whose new connection
//! is stamped from when it opened), a long stall whose audio was lost. The
//! pacer's slack runs out before the audio after such a gap comes, so the
//! pacer fills it with silence; the corrector starts measuring again from
//! zero and counts the reset. A smaller step (audio a full ring dropped, a
//! short loss) is made up by the corrector once the few blocks after it
//! confirm it, in the first second of a connection as later: that much
//! silence ahead of the block that confirms it, or for a step back that
//! much cut, and counted ([`InputDrift::steps`]). A timestamp that strays
//! and comes back within three blocks (a host-time glitch) is not a step:
//! those blocks pass as they are and nothing is made up. One that stays off
//! for four blocks or more (and, should the blocks be shorter than the step
//! threshold, 2 ms, for at least three thresholds of input from its first
//! block to its fourth or a later one) is confirmed at that block like a
//! loss and made up, and taken back once the fourth block back confirms the
//! way back: two short edits the size of the stray (silence and a cut, for
//! PTS stamped late), a sync error that size for about as long as it
//! lasted, and no lasting offset ([`ac_audio::drift`] has the rule). Should
//! its silence lift the pacer's lead over its limit, the audio dropped for
//! that is given back as silence once the stray's silence is out again, if
//! that is within a second of the drop ([`crate::audio::RECOVERY_WINDOW`]),
//! while the pacer owes no silence of its own, and with the lead then
//! within half the limit (where it was before the step); otherwise it stays
//! dropped, and the audio plays that much early ([`crate::audio`]). A loss
//! close to the slack can run the pacer out of slack too; then the pacer's
//! silence comes first, and the corrector's comes after it as late audio,
//! from which the pacer cuts exactly the silence it owes, splitting a block
//! ([`crate::audio`]), so the gap is made up once either way and the audio
//! ends where it was. A stall whose audio arrives late keeps its PTS and is
//! not a step.
//!
//! A change of format (a reconnection at another rate) starts a new
//! corrector, counted as a reset too. The few frames the old one held back
//! (about half a millisecond, and only while it was resampling) are
//! discarded with it, unheard beside the reconnection's own gap. The output
//! keeps the rate it was started with, so until it is started again its
//! pacer drops the new rate's blocks (see the Rate section of
//! [`crate::program_audio`]).
//!
//! # Invariants
//!
//! * A block the corrector leaves unchanged is passed on as the very block
//!   (its samples shared, not copied), so a file input played on the
//!   engine's clock, whose PTS count its samples and so measure no drift,
//!   reaches the pacer bit for bit.
//! * A block that was resampled, or had silence put ahead of it or its
//!   start cut for a step, keeps its source, sequence number, rate and
//!   channels; its PTS moves back by the frames the resampler held back
//!   (half a millisecond at 48 kHz) and the silence, and on by the cut.
//!   One that came out empty is not passed on.
//! * Pure: blocks in, blocks out, time from PTS and sample counts only.

use std::time::Duration;

use ac_audio::{Corrected, DriftCorrector, DriftSettings};
use ac_media::AudioBlock;

use crate::output::AUDIO_SLACK;

/// The settings program audio's drift is corrected with: the defaults,
/// with a break where the pacer surely fills a gap; see the module docs.
pub(crate) fn settings() -> DriftSettings {
    DriftSettings {
        discontinuity: AUDIO_SLACK,
        ..DriftSettings::default()
    }
}

/// Drift correction of one audio input's program blocks; see the module
/// docs.
#[derive(Debug, Default)]
pub(crate) struct InputDrift {
    /// The corrector for the format of the latest block, once one came in
    /// a format it takes.
    corrector: Option<DriftCorrector>,
    /// Resets of correctors a format change replaced, and those changes.
    earlier_resets: u64,
    /// Steps made up by correctors a format change replaced.
    earlier_steps: u64,
}

impl InputDrift {
    /// Corrects `block` (picked to stereo); returns what to pass on in its
    /// place, `None` when nothing came out.
    pub(crate) fn correct(&mut self, block: AudioBlock) -> Option<AudioBlock> {
        let format = (block.sample_rate, block.channels);
        if self.corrector.as_ref().map(DriftCorrector::format) != Some(format) {
            if let Some(old) = self.corrector.take() {
                let stats = old.stats();
                self.earlier_resets += stats.resets + 1;
                self.earlier_steps += stats.steps;
            }
            self.corrector =
                DriftCorrector::new(block.sample_rate, block.channels, settings()).ok();
        }
        // A format no corrector takes (no rate) is left to the pacer.
        let Some(corrector) = &mut self.corrector else {
            return Some(block);
        };
        match corrector.process(block.pts, &block.samples) {
            Corrected::Unchanged => Some(block),
            Corrected::Resampled(r) if r.samples.is_empty() => None,
            Corrected::Resampled(r) => {
                let by = Duration::from_secs_f64(r.behind.abs() / f64::from(block.sample_rate));
                let pts = if r.behind >= 0.0 {
                    block.pts.saturating_sub(by)
                } else {
                    block.pts.saturating_add(by)
                };
                Some(AudioBlock {
                    pts,
                    samples: r.samples.into(),
                    ..block
                })
            }
        }
    }

    /// The correction applied now, in ppm of the input's rate: below zero
    /// while samples are taken out (its clock runs fast), zero while blocks
    /// pass through.
    pub(crate) fn correction_ppm(&self) -> f64 {
        self.corrector
            .as_ref()
            .map_or(0.0, |c| c.stats().correction_ppm)
    }

    /// Breaks and format changes that restarted the measurement.
    pub(crate) fn resets(&self) -> u64 {
        self.earlier_resets + self.corrector.as_ref().map_or(0, |c| c.stats().resets)
    }

    /// Steps in the PTS made up with silence or a cut.
    pub(crate) fn steps(&self) -> u64 {
        self.earlier_steps + self.corrector.as_ref().map_or(0, |c| c.stats().steps)
    }
}

#[cfg(test)]
mod tests;
