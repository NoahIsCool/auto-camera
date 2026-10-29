//! Channel selection: which channels of an input become program left and
//! right.
//!
//! Program audio is one stereo input (spec M12), so the encoder is always
//! given two channels. An input may carry more: a soundboard's USB audio
//! interface sends every channel of the desk, with the main mix on one
//! pair of them. Or it carries one, a mono feed. A [`ChannelPick`] names the
//! input channels that become program left and right:
//!
//! * [`ChannelPick::Pair`]: one channel for the left side and one for the
//!   right, in that order, so `4, 3` swaps the sides;
//! * [`ChannelPick::Mono`]: one channel on both sides.
//!
//! People number channels from 1, as a desk does, so the config and every
//! message do too ([`ChannelPick::from_numbers`], [`fmt::Display`]); the
//! type holds 0-based indices.
//!
//! # Selection copies, it never mixes
//!
//! A selected block holds exactly the input's samples of the picked
//! channels, sample for sample: nothing is scaled, summed or dithered, so
//! the encoder gets the board's pair bit for bit and a mono channel at full
//! level on both sides (not the −3 dB a pan law would give it). An input
//! without a pick is sent as its first two channels, a mono one on both
//! sides ([`ChannelPick::first_of`]).
//!
//! # Invariants
//!
//! * [`ChannelPick::select`] returns a block of [`PROGRAM_CHANNELS`]
//!   channels with the input block's source, sequence number, PTS, sample
//!   rate and number of sample frames.
//! * It refuses (`None`) a block that lacks a picked channel, rather than
//!   send other channels in their place.
//! * A stereo block under the pick of its first two channels is passed on
//!   as it is, sharing its samples, so a stereo source costs no copy.

use std::fmt;
use std::sync::Arc;

use crate::AudioBlock;

/// Channels in program audio: stereo.
pub const PROGRAM_CHANNELS: u16 = 2;

/// The input channels that become program left and right; see the module
/// docs. Indices are 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelPick {
    /// Channel `.0` on both sides.
    Mono(u16),
    /// Channel `.0` on the left, channel `.1` on the right.
    Pair(u16, u16),
}

impl ChannelPick {
    /// The pick for an input of `channels` channels that names none: its
    /// first two, a mono input on both sides; `None` without channels.
    pub fn first_of(channels: u16) -> Option<Self> {
        match channels {
            0 => None,
            1 => Some(Self::Mono(0)),
            _ => Some(Self::Pair(0, 1)),
        }
    }

    /// The pick written as channel numbers counted from 1: one number for
    /// that channel on both sides, two for left and right.
    pub fn from_numbers(numbers: &[u16]) -> Result<Self, String> {
        if let Some(zero) = numbers.iter().find(|&&n| n == 0) {
            return Err(format!(
                "channel {zero} does not exist: channels are numbered from 1"
            ));
        }
        match *numbers {
            [c] => Ok(Self::Mono(c - 1)),
            [l, r] => Ok(Self::Pair(l - 1, r - 1)),
            _ => Err(format!(
                "list one channel (on both sides) or two (left and right), not {}",
                numbers.len()
            )),
        }
    }

    /// The picked channels as numbers counted from 1, left first.
    pub fn numbers(&self) -> Vec<u16> {
        match *self {
            Self::Mono(c) => vec![c + 1],
            Self::Pair(l, r) => vec![l + 1, r + 1],
        }
    }

    /// The fewest channels an input must have for this pick.
    pub fn needs(&self) -> u16 {
        let highest = match *self {
            Self::Mono(c) => c,
            Self::Pair(l, r) => l.max(r),
        };
        highest.saturating_add(1)
    }

    /// `Ok` when an input of `channels` channels has every picked channel,
    /// else why not.
    pub fn check(&self, channels: u16) -> Result<(), String> {
        if channels >= self.needs() {
            return Ok(());
        }
        let verb = match self {
            Self::Mono(_) => "needs",
            Self::Pair(..) => "need",
        };
        Err(format!(
            "{self} {verb} an input of at least {} channels; it has {channels}",
            self.needs()
        ))
    }

    /// `block` as program audio: the picked channels, interleaved left and
    /// right; `None` when the block lacks one of them (see the module docs).
    pub fn select(&self, block: &AudioBlock) -> Option<AudioBlock> {
        let channels = usize::from(block.channels);
        if block.channels < self.needs() {
            return None;
        }
        if block.channels == PROGRAM_CHANNELS && *self == Self::Pair(0, 1) {
            return Some(block.clone());
        }
        let (left, right) = match *self {
            Self::Mono(c) => (usize::from(c), usize::from(c)),
            Self::Pair(l, r) => (usize::from(l), usize::from(r)),
        };
        let samples: Arc<[f32]> = block
            .samples
            .chunks_exact(channels)
            .flat_map(|frame| [frame[left], frame[right]])
            .collect();
        Some(AudioBlock {
            channels: PROGRAM_CHANNELS,
            samples,
            ..block.clone()
        })
    }
}

impl fmt::Display for ChannelPick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Mono(c) => write!(f, "channel {} on both sides", c + 1),
            Self::Pair(l, r) => write!(f, "channels {} and {}", l + 1, r + 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ac_core::SourceId;

    use super::*;

    /// A distinct value for sample frame `n` of channel `c`.
    fn value(c: usize, n: usize) -> f32 {
        (c * 1000 + n) as f32 / 65_536.0
    }

    /// Block 7 of `frames` sample frames over `channels` channels, each
    /// sample its [`value`].
    fn block(channels: u16, frames: usize) -> AudioBlock {
        let samples: Vec<f32> = (0..frames)
            .flat_map(|n| (0..usize::from(channels)).map(move |c| value(c, n)))
            .collect();
        AudioBlock {
            source: SourceId("board".into()),
            seq: 7,
            pts: Duration::from_millis(70),
            sample_rate: 48_000,
            channels,
            samples: Arc::from(samples),
        }
    }

    #[test]
    fn a_pair_of_sixteen_channels_is_selected_sample_for_sample() {
        let input = block(16, 480);
        let pick = ChannelPick::from_numbers(&[3, 4]).unwrap();
        assert_eq!(pick, ChannelPick::Pair(2, 3));
        let out = pick.select(&input).unwrap();
        assert_eq!(out.channels, PROGRAM_CHANNELS);
        assert_eq!(out.frames(), 480);
        let want: Vec<f32> = (0..480).flat_map(|n| [value(2, n), value(3, n)]).collect();
        assert_eq!(&out.samples[..], &want[..]);
        assert_eq!(
            (&out.source, out.seq, out.pts, out.sample_rate),
            (&input.source, input.seq, input.pts, input.sample_rate),
            "the tags are the input block's"
        );
    }

    #[test]
    fn mono_goes_to_both_sides_at_full_level() {
        let input = block(1, 100);
        let pick = ChannelPick::first_of(1).unwrap();
        assert_eq!(pick, ChannelPick::Mono(0));
        let out = pick.select(&input).unwrap();
        assert_eq!(out.channels, 2);
        for (n, frame) in out.samples.chunks_exact(2).enumerate() {
            assert_eq!(frame, [value(0, n); 2], "frame {n}");
        }
        // One channel of a multichannel input, the same way.
        let out = ChannelPick::Mono(5).select(&block(8, 10)).unwrap();
        let want: Vec<f32> = (0..10).flat_map(|n| [value(5, n); 2]).collect();
        assert_eq!(&out.samples[..], &want[..]);
    }

    #[test]
    fn a_pair_may_swap_the_sides_or_repeat_a_channel() {
        let out = ChannelPick::Pair(1, 0).select(&block(2, 4)).unwrap();
        let want: Vec<f32> = (0..4).flat_map(|n| [value(1, n), value(0, n)]).collect();
        assert_eq!(&out.samples[..], &want[..]);
        let twice = ChannelPick::Pair(2, 2).select(&block(3, 4)).unwrap();
        let mono = ChannelPick::Mono(2).select(&block(3, 4)).unwrap();
        assert_eq!(twice.samples, mono.samples);
    }

    #[test]
    fn a_stereo_block_under_the_default_pick_is_passed_on_as_it_is() {
        let input = block(2, 480);
        let out = ChannelPick::first_of(2).unwrap().select(&input).unwrap();
        assert!(Arc::ptr_eq(&out.samples, &input.samples), "no copy");
        assert_eq!(ChannelPick::first_of(16), Some(ChannelPick::Pair(0, 1)));
        assert_eq!(ChannelPick::first_of(0), None);
    }

    #[test]
    fn a_block_without_the_picked_channels_is_refused() {
        let pick = ChannelPick::Pair(2, 3);
        assert!(pick.select(&block(2, 4)).is_none());
        assert!(pick.select(&block(3, 4)).is_none());
        assert!(pick.select(&block(4, 4)).is_some());
        assert!(ChannelPick::Mono(1).select(&block(1, 4)).is_none());
        // An empty block of enough channels is an empty stereo block.
        let empty = pick.select(&block(4, 0)).unwrap();
        assert_eq!((empty.channels, empty.frames()), (2, 0));
    }

    #[test]
    fn channel_numbers_count_from_one() {
        let pick = ChannelPick::from_numbers(&[17, 18]).unwrap();
        assert_eq!((pick.numbers(), pick.needs()), (vec![17, 18], 18));
        assert_eq!(ChannelPick::from_numbers(&[5]).unwrap().numbers(), [5]);
        assert_eq!(ChannelPick::Pair(9, 1).needs(), 10);
        assert_eq!(ChannelPick::Mono(u16::MAX).needs(), u16::MAX);
        for (numbers, error) in [
            (&[][..], "not 0"),
            (&[1, 2, 3][..], "not 3"),
            (&[0, 2][..], "numbered from 1"),
            (&[0][..], "numbered from 1"),
        ] {
            let e = ChannelPick::from_numbers(numbers).unwrap_err();
            assert!(e.contains(error), "{numbers:?}: {e}");
        }
    }

    #[test]
    fn a_pick_is_checked_against_the_inputs_channels() {
        let pick = ChannelPick::Pair(2, 3);
        assert_eq!(pick.check(4), Ok(()));
        assert_eq!(pick.check(16), Ok(()));
        assert_eq!(
            pick.check(2),
            Err("channels 3 and 4 need an input of at least 4 channels; it has 2".into())
        );
        let mono = ChannelPick::Mono(4);
        assert_eq!(mono.to_string(), "channel 5 on both sides");
        assert_eq!(
            mono.check(1),
            Err("channel 5 on both sides needs an input of at least 5 channels; it has 1".into())
        );
        assert!(ChannelPick::Mono(0).check(0).is_err());
    }
}
