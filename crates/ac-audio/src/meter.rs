//! The program meter: momentary, short-term and integrated loudness, sample
//! peak and true peak of interleaved audio (the measures are explained in
//! the [crate docs](crate)).
//!
//! # Time
//!
//! The meter's clock is its count of sample frames. The audio is cut into
//! 100 ms hops: hop `k` ends after `⌊(k + 1)·rate / 10⌋` frames, so hops are
//! exactly 4 800 frames at 48 kHz and, where a tenth of the rate is not
//! whole (11 025 Hz), alternate between one frame shorter and one longer
//! while averaging exactly 100 ms. Each completed hop leaves its K-weighted
//! energy (summed over channels) and its frame count in a ring of the last
//! 30; a window's power is its hops' energy over their frames, so windows
//! are exact whatever the hop lengths. Loudness readings change only when a
//! hop completes, ten times a second, the refresh rate Tech 3341 asks of a
//! meter; the momentary window's newest block also goes to the gated
//! measurement then. Peaks are maxima since the meter started (or was
//! reset), taken sample by sample.
//!
//! # Input
//!
//! [`LoudnessMeter::push`] takes interleaved `f32` (full scale ±1.0, louder
//! samples allowed) in the format the meter was made for and refuses
//! anything else whole, before measuring any of it. A NaN or infinite
//! sample is measured as silence: one bad sample would otherwise poison the
//! filters' state, and every reading, for the rest of the show.
//! [`LoudnessMeter::push_silence`] measures digital silence without a
//! buffer, for the silence a pacer inserts. Nothing is copied: samples are
//! read in place, one frame at a time.

use std::time::Duration;

use thiserror::Error;

use crate::gating::GatedLoudness;
use crate::kweighting::KWeighting;
use crate::true_peak::TruePeak;
use crate::{decibels, loudness};

/// Lowest sample rate the meter takes, in Hz.
pub const MIN_SAMPLE_RATE: u32 = 8_000;

/// Highest sample rate the meter takes, in Hz.
pub const MAX_SAMPLE_RATE: u32 = 384_000;

/// Most channels the meter takes (the encoder's limit too).
pub const MAX_CHANNELS: u16 = 8;

/// Hops per second: one hop is 100 ms.
const HOPS_PER_SECOND: u64 = 10;

/// Hops in the momentary window (400 ms).
pub const MOMENTARY_HOPS: usize = 4;

/// Hops in the short-term window (3 s).
pub const SHORT_TERM_HOPS: usize = 30;

/// A frame of silence on every channel the meter takes.
const SILENT_FRAME: [f32; MAX_CHANNELS as usize] = [0.0; MAX_CHANNELS as usize];

/// What the meter reads now. Every value is finite; `None` is "no reading"
/// and says why in each field's docs. A window holds digital silence once
/// the K-weighting filters have come to rest, within half a second of the
/// sound stopping; until then it reads a very low but finite level.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LoudnessReading {
    /// Momentary loudness in LUFS: the last 400 ms, ungated. `None` before
    /// 400 ms of audio, and while the window holds digital silence.
    pub momentary: Option<f64>,
    /// Short-term loudness in LUFS: the last 3 s, ungated. `None` before
    /// 3 s of audio, and while the window holds digital silence.
    pub short_term: Option<f64>,
    /// Integrated loudness in LUFS: every 400 ms block since the start,
    /// gated. `None` while no block is above the absolute gate (−70 LUFS).
    pub integrated: Option<f64>,
    /// Largest absolute sample since the start, in dBFS. `None` while every
    /// sample was zero.
    pub sample_peak: Option<f64>,
    /// Largest absolute value of the 4× oversampled signal since the
    /// start, in dBTP; never below the sample peak. `None` while every
    /// sample was zero.
    pub true_peak: Option<f64>,
    /// Audio measured since the start.
    pub measured: Duration,
}

/// Why the meter refused to be made or to measure some input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MeterError {
    /// The sample rate is zero or out of range.
    #[error("sample rate {0} Hz is outside {MIN_SAMPLE_RATE}..={MAX_SAMPLE_RATE} Hz")]
    SampleRate(u32),
    /// The channel count is zero or above [`MAX_CHANNELS`].
    #[error("{0} channels: the meter takes 1 to {MAX_CHANNELS}")]
    Channels(u16),
    /// Input at another rate than the meter's.
    #[error("{actual} Hz audio on a {expected} Hz meter")]
    RateMismatch {
        /// The meter's rate.
        expected: u32,
        /// The input's.
        actual: u32,
    },
    /// Input with another channel count than the meter's.
    #[error("{actual}-channel audio on a {expected}-channel meter")]
    ChannelMismatch {
        /// The meter's channel count.
        expected: u16,
        /// The input's.
        actual: u16,
    },
    /// Interleaved input that is not a whole number of frames.
    #[error("{len} samples are not a whole number of {channels}-channel frames")]
    PartialFrame {
        /// Samples given.
        len: usize,
        /// Channels per frame.
        channels: u16,
    },
}

/// One completed 100 ms hop.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Hop {
    /// K-weighted energy, summed over its frames and channels.
    energy: f64,
    /// Its length in frames.
    frames: u64,
}

/// The per-channel filters.
#[derive(Debug, Clone, PartialEq)]
struct Channel {
    /// K-weighting before the mean square.
    filter: KWeighting,
    /// 4× interpolation for the true peak.
    peak: TruePeak,
}

/// Length in frames of hop `k` at `rate` Hz; see the module docs.
fn hop_length(k: u64, rate: u32) -> u64 {
    let end = |k: u64| u128::from(k) * u128::from(rate) / u128::from(HOPS_PER_SECOND);
    u64::try_from(end(k + 1) - end(k)).unwrap_or(u64::MAX)
}

/// The duration of `frames` sample frames at `rate` Hz.
fn duration(frames: u64, rate: u32) -> Duration {
    let rate = u64::from(rate);
    Duration::from_secs(frames / rate)
        + Duration::from_nanos((frames % rate) * 1_000_000_000 / rate)
}

/// A BS.1770-4 / EBU R 128 meter for one stream; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct LoudnessMeter {
    /// Sample rate in Hz.
    sample_rate: u32,
    /// Filters, one per channel (the channel count is their number).
    channels: Vec<Channel>,
    /// The last [`SHORT_TERM_HOPS`] completed hops; hop `k` is at
    /// `k % SHORT_TERM_HOPS`.
    hops: [Hop; SHORT_TERM_HOPS],
    /// Hops completed.
    completed: u64,
    /// Energy of the hop under way.
    energy: f64,
    /// Length of the hop under way.
    hop_frames: u64,
    /// Frames still to come in the hop under way (at least one).
    left: u64,
    /// Frames measured.
    frames: u64,
    /// Largest absolute sample, linear.
    sample_peak: f64,
    /// Largest absolute oversampled value, linear.
    true_peak: f64,
    /// Momentary loudness after the latest hop.
    momentary: Option<f64>,
    /// Short-term loudness after the latest hop.
    short_term: Option<f64>,
    /// Integrated loudness after the latest hop.
    integrated: Option<f64>,
    /// The gated blocks.
    gated: GatedLoudness,
}

impl LoudnessMeter {
    /// A meter for `channels`-channel audio at `sample_rate` Hz, which
    /// must be within [`MIN_SAMPLE_RATE`]..=[`MAX_SAMPLE_RATE`] and
    /// 1..=[`MAX_CHANNELS`].
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self, MeterError> {
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(MeterError::SampleRate(sample_rate));
        }
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(MeterError::Channels(channels));
        }
        let channel = Channel {
            filter: KWeighting::new(sample_rate),
            peak: TruePeak::new(),
        };
        let first = hop_length(0, sample_rate);
        Ok(Self {
            sample_rate,
            channels: vec![channel; usize::from(channels)],
            hops: [Hop::default(); SHORT_TERM_HOPS],
            completed: 0,
            energy: 0.0,
            hop_frames: first,
            left: first,
            frames: 0,
            sample_peak: 0.0,
            true_peak: 0.0,
            momentary: None,
            short_term: None,
            integrated: None,
            gated: GatedLoudness::new(),
        })
    }

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channels per frame.
    pub fn channels(&self) -> u16 {
        // At most MAX_CHANNELS, checked in `new`.
        u16::try_from(self.channels.len()).unwrap_or(MAX_CHANNELS)
    }

    /// Measures interleaved `samples` of `channels`-channel audio at
    /// `sample_rate` Hz, which must be the meter's format and a whole
    /// number of frames; otherwise nothing is measured.
    pub fn push(
        &mut self,
        sample_rate: u32,
        channels: u16,
        samples: &[f32],
    ) -> Result<(), MeterError> {
        if sample_rate != self.sample_rate {
            return Err(MeterError::RateMismatch {
                expected: self.sample_rate,
                actual: sample_rate,
            });
        }
        if channels != self.channels() {
            return Err(MeterError::ChannelMismatch {
                expected: self.channels(),
                actual: channels,
            });
        }
        let width = usize::from(channels);
        if !samples.len().is_multiple_of(width) {
            return Err(MeterError::PartialFrame {
                len: samples.len(),
                channels,
            });
        }
        self.measure(samples.chunks_exact(width));
        Ok(())
    }

    /// Measures `frames` frames of digital silence.
    pub fn push_silence(&mut self, frames: u64) {
        let silent = &SILENT_FRAME[..self.channels.len()];
        let frames = usize::try_from(frames).unwrap_or(usize::MAX);
        self.measure(std::iter::repeat_n(silent, frames));
    }

    /// What the meter reads now.
    pub fn reading(&self) -> LoudnessReading {
        LoudnessReading {
            momentary: self.momentary,
            short_term: self.short_term,
            integrated: self.integrated,
            sample_peak: decibels(self.sample_peak),
            true_peak: decibels(self.true_peak),
            measured: duration(self.frames, self.sample_rate),
        }
    }

    /// Starts over, as if just made.
    pub fn reset(&mut self) {
        for c in &mut self.channels {
            c.filter.reset();
            c.peak.reset();
        }
        self.hops = [Hop::default(); SHORT_TERM_HOPS];
        self.completed = 0;
        self.energy = 0.0;
        self.hop_frames = hop_length(0, self.sample_rate);
        self.left = self.hop_frames;
        self.frames = 0;
        self.sample_peak = 0.0;
        self.true_peak = 0.0;
        self.momentary = None;
        self.short_term = None;
        self.integrated = None;
        self.gated.reset();
    }

    /// Measures each frame of `frames`, oldest first.
    fn measure<'a>(&mut self, frames: impl Iterator<Item = &'a [f32]>) {
        for frame in frames {
            let mut energy = 0.0;
            for (channel, &sample) in self.channels.iter_mut().zip(frame) {
                let x = if sample.is_finite() {
                    f64::from(sample)
                } else {
                    0.0
                };
                self.sample_peak = self.sample_peak.max(x.abs());
                self.true_peak = self.true_peak.max(channel.peak.push(x));
                let y = channel.filter.process(x);
                energy += y * y;
            }
            self.energy += energy;
            self.frames += 1;
            self.left -= 1;
            if self.left == 0 {
                self.finish_hop();
            }
        }
    }

    /// Files the hop under way, starts the next and updates the loudness
    /// readings.
    fn finish_hop(&mut self) {
        let slot = (self.completed % SHORT_TERM_HOPS as u64) as usize;
        self.hops[slot] = Hop {
            energy: self.energy,
            frames: self.hop_frames,
        };
        self.completed += 1;
        self.energy = 0.0;
        self.hop_frames = hop_length(self.completed, self.sample_rate);
        self.left = self.hop_frames;
        if self.completed >= MOMENTARY_HOPS as u64 {
            let power = self.window(MOMENTARY_HOPS);
            self.momentary = loudness(power);
            self.gated.add(power);
            self.integrated = self.gated.integrated();
        }
        if self.completed >= SHORT_TERM_HOPS as u64 {
            self.short_term = loudness(self.window(SHORT_TERM_HOPS));
        }
    }

    /// The channel-weighted mean square of the last `n` completed hops
    /// (at most [`SHORT_TERM_HOPS`], and no more than have completed).
    fn window(&self, n: usize) -> f64 {
        let (energy, frames) = (1..=n as u64)
            .map(|back| self.hops[((self.completed - back) % SHORT_TERM_HOPS as u64) as usize])
            .fold((0.0, 0), |(e, f), hop| (e + hop.energy, f + hop.frames));
        energy / frames as f64
    }
}

#[cfg(test)]
mod tests;
