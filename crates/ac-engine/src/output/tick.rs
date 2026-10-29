//! The controller's per-tick calls on the engine thread: pushing the
//! program and reading the output's view.
//!
//! [`OutputController::push`] hands the encoder exactly one frame per tick,
//! plus a repeat owed for each tick the engine skipped, so the encoded
//! frame count is the master clock; the [`AudioPacer`] then plans the
//! program audio against that count, filling a lag with silence and
//! dropping audio that runs too far ahead. Neither call blocks: the
//! encoder's queues drop instead (see `ac_output`).

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
            frames_pushed: self.frames_pushed,
        }
    }
}
