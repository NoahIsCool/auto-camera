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
//! # Breaks and settings
//!
//! The corrector takes a step in the PTS larger than the pacer's slack
//! ([`AUDIO_SLACK`]) for a break, not drift: a reconnection (whose new
//! connection is stamped from when it opened), a stall whose audio was
//! lost. The pacer fills such a gap with silence; the corrector starts
//! measuring again from zero and counts the reset. A smaller step, which
//! the pacer leaves alone, is timing error the corrector takes up slowly.
//! A stall whose audio arrives late keeps its PTS and is not a break.
//!
//! A change of format (a reconnection at another rate) starts a new
//! corrector, counted as a reset too; the few frames the old one held back
//! go with it, as the pacer drops the old rate's blocks anyway.
//!
//! # Invariants
//!
//! * A block the corrector leaves unchanged is passed on as the very block
//!   (its samples shared, not copied), so a file input played on the
//!   engine's clock, whose PTS count its samples and so measure no drift,
//!   reaches the pacer bit for bit.
//! * A block that was resampled keeps its source, sequence number, rate
//!   and channels; its PTS moves back by the frames the resampler held
//!   back (half a millisecond at 48 kHz). One that came out empty is not
//!   passed on.
//! * Pure: blocks in, blocks out, time from PTS and sample counts only.

use std::time::Duration;

use ac_audio::{Corrected, DriftCorrector, DriftSettings};
use ac_media::AudioBlock;

use crate::output::AUDIO_SLACK;

/// The settings program audio's drift is corrected with: the defaults,
/// with a break where the pacer would fill a gap; see the module docs.
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
}

impl InputDrift {
    /// Corrects `block` (picked to stereo); returns what to pass on in its
    /// place, `None` when nothing came out.
    pub(crate) fn correct(&mut self, block: AudioBlock) -> Option<AudioBlock> {
        let format = (block.sample_rate, block.channels);
        if self.corrector.as_ref().map(DriftCorrector::format) != Some(format) {
            if let Some(old) = self.corrector.take() {
                self.earlier_resets += old.stats().resets + 1;
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
                let back = Duration::from_secs_f64(r.behind / f64::from(block.sample_rate));
                Some(AudioBlock {
                    pts: block.pts.saturating_sub(back),
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
}

#[cfg(test)]
mod tests;
