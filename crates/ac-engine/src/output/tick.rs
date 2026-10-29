//! The controller's per-tick calls on the engine thread: pushing the
//! program and reading the output's view.
//!
//! [`OutputController::push`] hands the encoder exactly one frame per tick,
//! plus a repeat owed for each tick the engine skipped, so the encoded
//! frame count is the master clock; the [`AudioPacer`] then plans the
//! program audio against that count, filling a lag with silence and
//! dropping audio that runs too far ahead. Neither call blocks: the
//! encoder's queues drop instead (see `ac_output`).
//!
//! Each item the pacer plans is measured by the session's program meter
//! ([`LoudnessMeter`], spec M12) just before it goes to the encoder:
//! source blocks are read in place and inserted silence is measured
//! without a buffer, so the meter adds no copy to the hot path (the one
//! copy of a block is into the encoder's queue) and reads the program
//! audio as the encoder is given it. Should the encoder's queue be full,
//! it drops the block and later writes silence of the same length instead
//! (see `ac_output`): the meter has then measured audio the recording
//! lacks, but the two timelines stay the same length.

use ac_audio::LoudnessMeter;
use ac_media::{AudioBlock, VideoFrame};
use ac_output::ProgramEncoder;

use super::OutputController;
use crate::audio::{AudioPacer, PacedAudio};
use crate::snapshot::OutputView;

impl OutputController {
    /// Pushes one program frame and the program audio drained this tick.
    /// `skipped` ticks were skipped just before this one; the encoder is
    /// owed a repeated frame for each, and they count as pushed, so the
    /// encoded timeline and the audio target stay on the master clock.
    pub fn push(&mut self, frame: &VideoFrame, audio: Vec<AudioBlock>, skipped: u64) {
        let Some(encoder) = &self.encoder else {
            return;
        };
        self.frames_pushed += encoder.repeat_frames(skipped);
        if encoder.push_frame(frame).is_ok() {
            self.frames_pushed += 1;
        }
        let Some(pacer) = &mut self.pacer else {
            return;
        };
        for item in pacer.plan(self.frames_pushed, audio) {
            measure(self.meter.as_mut(), &item);
            let _ = match item {
                PacedAudio::Block(block) => encoder.push_audio_block(&block),
                PacedAudio::Silence(frames) => {
                    let channels = encoder.config().audio.map_or(2, |a| a.channels);
                    let n = usize::try_from(frames).unwrap_or(0) * usize::from(channels);
                    encoder.push_audio(&vec![0.0; n])
                }
            };
        }
    }

    /// The output as published in snapshots.
    pub fn view(&self) -> OutputView {
        OutputView {
            state: self.state.clone(),
            rtmp_url: self.rtmp_url.as_deref().map(ac_output::config::redact_url),
            record_path: self.record_path.clone(),
            stats: self
                .encoder
                .as_ref()
                .map(ProgramEncoder::stats)
                .or_else(|| self.last_stats.clone()),
            audio: self.pacer.as_ref().map(AudioPacer::stats),
            loudness: self.meter.as_ref().map(LoudnessMeter::reading),
            frames_pushed: self.frames_pushed,
            keep_awake: self.awake.state(),
        }
    }
}

/// Measures one planned item on the program meter, if there is one. The
/// pacer only plans blocks in the meter's format, so the meter refuses
/// nothing here.
fn measure(meter: Option<&mut LoudnessMeter>, item: &PacedAudio) {
    let Some(meter) = meter else {
        return;
    };
    match item {
        PacedAudio::Block(block) => {
            let _ = meter.push(block.sample_rate, block.channels, &block.samples);
        }
        PacedAudio::Silence(frames) => meter.push_silence(*frames),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ac_core::SourceId;

    use super::*;

    /// `seconds` of a stereo 48 kHz 1 kHz sine whose crest is at `dbfs`.
    fn tone(dbfs: f64, seconds: u32) -> AudioBlock {
        let amplitude = 10f64.powf(dbfs / 20.0);
        let samples: Vec<f32> = (0..48_000 * seconds)
            .flat_map(|n| {
                let x = amplitude * (2.0 * std::f64::consts::PI * f64::from(n) / 48.0).sin();
                [x as f32; 2]
            })
            .collect();
        AudioBlock {
            source: SourceId("stage".into()),
            pts: Duration::ZERO,
            sample_rate: 48_000,
            channels: 2,
            samples: Arc::from(samples),
        }
    }

    #[test]
    fn the_meter_measures_blocks_and_inserted_silence() {
        let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
        measure(Some(&mut meter), &PacedAudio::Silence(48_000));
        measure(Some(&mut meter), &PacedAudio::Block(tone(-23.0, 20)));
        let r = meter.reading();
        assert_eq!(r.measured, Duration::from_secs(21));
        // The silence is below the absolute gate: the tone's loudness.
        let integrated = r.integrated.unwrap();
        assert!((integrated + 23.0).abs() < 0.1, "{integrated}");
        // Without a meter (no audio in the session) nothing happens.
        measure(None, &PacedAudio::Silence(10));
    }

    #[test]
    fn a_block_in_another_format_is_not_measured() {
        let mut meter = LoudnessMeter::new(44_100, 2).unwrap();
        measure(Some(&mut meter), &PacedAudio::Block(tone(-23.0, 1)));
        assert_eq!(meter.reading(), Default::default());
    }
}
