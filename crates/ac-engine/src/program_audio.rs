//! Program audio: where it comes from, and the stereo pair it is made of.
//!
//! `output.audio_source` names the origin of program audio: a video source,
//! whose embedded audio it is, or an audio input (`[[audio_inputs]]`, an
//! audio-only feed such as a soundboard's;
//! [`AudioInputEntry`](crate::config::AudioInputEntry)). Whatever the origin
//! delivers, the program carries one stereo pair (spec M12, "one stereo
//! input").
//!
//! # Channels
//!
//! The encoder is always opened for [`PROGRAM_CHANNELS`] (two) channels.
//! Every block of the origin is cut down to two before it reaches the pacer
//! ([`ChannelPick::select`]): the samples of the picked channels are copied
//! bit for bit, nothing is mixed. An audio input takes its configured
//! `channels` (a pair, or one channel on both sides); otherwise, and for a
//! video source's audio always, the first two channels are taken, or a mono
//! block's one on both sides ([`ChannelPick::first_of`]). A stereo block
//! with the default pick passes through as it is. A block that lacks the
//! picked channels (an input that changed its layout under its supervisor)
//! is dropped and counted ([`AudioInputView::unusable`]), and the pacer
//! fills the gap with silence, so the program never gets the wrong
//! channels.
//!
//! # Rate
//!
//! Nothing resamples: an output's sample rate is the origin's, as its newest
//! connection describes it when the output starts ([`ProgramAudio::format`]).
//! A video source that has described no audio gives the output none. An
//! audio input that has not connected yet is taken to run at
//! [`STAND_IN_RATE`], the rate a soundboard runs at, so the recording still
//! has its audio track and the input is heard as soon as it connects at that
//! rate; at another rate its blocks are dropped by the pacer (its
//! `mismatched_blocks`) until the output is started again.
//!
//! # Ownership and threads
//!
//! The engine thread owns the [`ProgramAudio`] and through it every audio
//! input, each under its supervisor (`ac-audio-supervise-<id>`, see
//! [`ac_sources::SupervisedAudio`]), which captures or decodes on its own
//! threads and reopens the input when it fails. Each tick
//! ([`ProgramAudio::take`]) drains every video source's audio and every
//! input's ring, so none builds up a backlog, and keeps the origin's.
//! Draining takes a lock that the input's thread holds only to push a block,
//! so a stalled, drifting or unplugged input never holds the engine up:
//!
//! * a stall delivers nothing for a while (the pacer fills silence), then
//!   perhaps a burst (the pacer cuts the late backlog);
//! * a sample clock that drifts from the engine's delivers a little more or
//!   less than the video's share, which the pacer's fills and cuts absorb;
//! * blocks nobody drained in time are dropped oldest first by the input's
//!   bounded ring ([`AudioInputView::dropped`]);
//! * an unplugged input goes offline with its reason and comes back when
//!   its supervisor reopens it ([`AudioInputView::state`] and `error`).
//!
//! The phase reference of the master clock ([`crate::schedule`]) is the
//! video source whose audio is program audio, so ticks fall between its
//! frames; with an audio input as the origin it is the first source.

use std::sync::Arc;

use ac_core::SourceId;
use ac_media::{AudioBlock, AudioInput, ChannelPick, PROGRAM_CHANNELS, SourceState, VideoSource};
use ac_output::AudioFormat;
use ac_sources::{Backoff, Clock, open_supervised_audio};

use crate::config::StudioConfig;
use crate::runtime::EngineError;

/// The sample rate an output starts with when its audio input has not
/// connected yet; see the module docs.
pub const STAND_IN_RATE: u32 = 48_000;

/// Where program audio comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The embedded audio of the video source at this index.
    Source(usize),
    /// The audio input at this index.
    Input(usize),
}

/// One audio input as the operator sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioInputView {
    /// Its id.
    pub id: SourceId,
    /// Operator-facing name.
    pub name: String,
    /// Health.
    pub state: SourceState,
    /// Why it is offline, while it is.
    pub error: Option<String>,
    /// Sample rate and channel count of its newest connection, once one
    /// opened.
    pub format: Option<(u32, u16)>,
    /// The channels it gives program audio; `None` for the default (the
    /// first two, or a mono input's one on both sides).
    pub pick: Option<ChannelPick>,
    /// It is the program audio's origin.
    pub program: bool,
    /// Blocks drained from it.
    pub blocks: u64,
    /// Blocks its ring dropped because they were not drained in time.
    pub dropped: u64,
    /// Program blocks dropped because they lacked the picked channels.
    pub unusable: u64,
}

/// An opened audio input and what the engine counts of it.
pub(crate) struct InputSlot {
    /// Operator-facing name.
    pub(crate) name: String,
    /// The configured pick, `None` for the default.
    pub(crate) pick: Option<ChannelPick>,
    /// The input, under its supervisor.
    pub(crate) input: Box<dyn AudioInput>,
    /// Blocks drained.
    blocks: u64,
    /// Program blocks without the picked channels.
    unusable: u64,
}

impl InputSlot {
    /// A slot for `input`, shown as `name`, giving `pick`.
    pub(crate) fn new(name: &str, pick: Option<ChannelPick>, input: Box<dyn AudioInput>) -> Self {
        Self {
            name: name.into(),
            pick,
            input,
            blocks: 0,
            unusable: 0,
        }
    }
}

/// Program audio's origin and every audio input; see the module docs.
pub struct ProgramAudio {
    /// Where program audio comes from; `None` for a silent program.
    origin: Option<Origin>,
    /// The audio inputs, in config order.
    inputs: Vec<InputSlot>,
}

impl std::fmt::Debug for ProgramAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramAudio")
            .field("origin", &self.origin)
            .field("inputs", &self.inputs.len())
            .finish()
    }
}

impl ProgramAudio {
    /// Opens every audio input of `config` under a supervisor paced by
    /// `clock` (one that cannot be opened yet starts offline and is
    /// retried) and finds the origin `output.audio_source` names.
    pub fn open(config: &StudioConfig, clock: &Arc<dyn Clock>) -> Result<Self, EngineError> {
        let mut inputs = Vec::new();
        for entry in &config.audio_inputs {
            let input = open_supervised_audio(
                &entry.input,
                entry.placeholder_info(),
                entry.check_opened(),
                clock.clone(),
                Backoff::default(),
            )
            .map_err(|e| EngineError::Source {
                id: entry.id.clone(),
                message: e.to_string(),
            })?;
            let pick = entry.pick().ok().flatten();
            inputs.push(InputSlot::new(entry.display_name(), pick, Box::new(input)));
        }
        let origin = config.output.audio_source.as_deref().and_then(|id| {
            let source = config.sources.iter().position(|s| s.id == id);
            let input = || config.audio_inputs.iter().position(|a| a.id == id);
            source
                .map(Origin::Source)
                .or_else(|| input().map(Origin::Input))
        });
        Ok(Self::with_inputs(origin, inputs))
    }

    /// Program audio from `origin` with `inputs` already open.
    pub(crate) fn with_inputs(origin: Option<Origin>, inputs: Vec<InputSlot>) -> Self {
        Self { origin, inputs }
    }

    /// Where program audio comes from.
    pub fn origin(&self) -> Option<Origin> {
        self.origin
    }

    /// The video source the master clock locks its phase to: the origin
    /// when it is a video source, else the first.
    pub fn phase_reference(&self) -> usize {
        match self.origin {
            Some(Origin::Source(i)) => i,
            _ => 0,
        }
    }

    /// The format of program audio for an output starting now, among
    /// `sources`: always stereo, at the origin's rate (see the module
    /// docs); `None` for a silent program.
    pub fn format(&self, sources: &[Box<dyn VideoSource>]) -> Option<AudioFormat> {
        let sample_rate = match self.origin? {
            Origin::Source(i) => sources.get(i)?.current_info().audio?.0,
            Origin::Input(i) => {
                let info = self.inputs.get(i)?.input.current_info();
                info.format.map_or(STAND_IN_RATE, |(rate, _)| rate)
            }
        };
        Some(AudioFormat {
            sample_rate,
            channels: PROGRAM_CHANNELS,
        })
    }

    /// Drains the audio of every one of `sources` and every input, and
    /// returns the origin's, oldest first, picked to stereo; see the
    /// module docs.
    pub fn take(&mut self, sources: &[Box<dyn VideoSource>]) -> Vec<AudioBlock> {
        let mut program = Vec::new();
        for (i, s) in sources.iter().enumerate() {
            let blocks = s.drain_audio();
            if self.origin == Some(Origin::Source(i)) {
                program.extend(blocks.iter().filter_map(|b| stereo(None, b)));
            }
        }
        for (i, slot) in self.inputs.iter_mut().enumerate() {
            let blocks = slot.input.drain();
            slot.blocks += blocks.len() as u64;
            if self.origin != Some(Origin::Input(i)) {
                continue;
            }
            for block in &blocks {
                match stereo(slot.pick, block) {
                    Some(picked) => program.push(picked),
                    None => slot.unusable += 1,
                }
            }
        }
        program
    }

    /// Every audio input as the operator sees it, in config order.
    pub fn views(&self) -> Vec<AudioInputView> {
        self.inputs
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let info = slot.input.current_info();
                AudioInputView {
                    id: info.id,
                    name: slot.name.clone(),
                    state: slot.input.state(),
                    error: slot.input.error(),
                    format: info.format,
                    pick: slot.pick,
                    program: self.origin == Some(Origin::Input(i)),
                    blocks: slot.blocks,
                    dropped: slot.input.dropped(),
                    unusable: slot.unusable,
                }
            })
            .collect()
    }

    /// Stops every audio input (its supervisor and its connection).
    pub fn stop(&mut self) {
        for slot in &mut self.inputs {
            slot.input.stop();
        }
    }
}

/// `block` picked to stereo by `pick`, or by default when `None` (see the
/// module docs); `None` when it lacks the channels.
fn stereo(pick: Option<ChannelPick>, block: &AudioBlock) -> Option<AudioBlock> {
    pick.or_else(|| ChannelPick::first_of(block.channels))?
        .select(block)
}

#[cfg(test)]
mod tests;
