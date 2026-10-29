//! The pictures the runner shows the engine: each source's frame at a tick.
//!
//! A live studio reads each source's *newest* frame when a tick starts, and
//! the runtime's phase lock keeps the ticks midway between the frames of a
//! source at the tick rate (`ac_engine::schedule`). On simulated time there
//! is no "newest so far", so the runner says which frame that is: at a tick
//! with media time `t`, an [`Input`] shows its newest frame whose PTS is at
//! most `t + slack`, with `slack` half a tick period. For a source at the
//! tick rate that is frame `n` at tick `n`, as the phase-locked runtime
//! shows it, and the half period absorbs the nanosecond rounding between
//! the frame rate's PTS (`seq / fps`, rounded) and the tick's media time
//! (`n` times a rounded period). A source at another rate has frames
//! repeated or skipped, as it would live.
//!
//! Two kinds of input exist:
//!
//! * a **pattern**, the synthetic source's test pattern, rendered for the
//!   one frame shown, never for the frames skipped over;
//! * a **clip**, a file source read through [`ClipReader`] one frame at a
//!   time with one frame of look-ahead, so the reader knows the clip has
//!   ended as soon as it shows its last frame.
//!
//! # Invariants
//!
//! * Frames carry the source's id and the numbering of a live source of
//!   the same kind (sequence numbers from 0, `pts = seq / fps`), so a
//!   detection log recorded live replays here and the other way round.
//! * Shown frames only move forward: a later tick never shows an older
//!   frame, and a tick shows the previous frame again when no newer one is
//!   due.
//! * Nothing here reads a clock. A clip's decoder is an ffmpeg child owned
//!   by its reader, which kills it when the input is dropped.

use ac_core::SourceId;
use ac_media::{MediaError, SourceInfo, VideoFrame};
use ac_sources::{ClipReader, SourceConfig, SyntheticPattern};
use std::time::Duration;

/// One source's frames on simulated time; see the module docs.
#[derive(Debug)]
pub struct Input {
    /// What the source is.
    info: SourceInfo,
    /// Where its frames come from.
    kind: Kind,
    /// The frame shown at the newest tick.
    shown: Option<VideoFrame>,
}

/// Where an [`Input`]'s frames come from.
#[derive(Debug)]
enum Kind {
    /// A synthetic test pattern.
    Pattern {
        /// The pattern, a pure function of the sequence number.
        pattern: SyntheticPattern,
        /// Sequence number of the first frame not shown yet.
        next: u64,
    },
    /// A decoded file.
    Clip {
        /// The decoder (boxed: it is large, and a pattern is not).
        reader: Box<ClipReader>,
        /// The first frame not shown yet, read ahead.
        next: Option<VideoFrame>,
        /// The reader returned its end.
        ended: bool,
        /// The file loops, so it never ends.
        looping: bool,
    },
}

impl Input {
    /// The input of source `id` configured as `config`: a pattern for a
    /// synthetic source, a clip (with its segment and looping) for a file;
    /// an error for a camera.
    pub fn open(id: SourceId, config: &SourceConfig) -> Result<Self, MediaError> {
        match config {
            SourceConfig::Synthetic {
                width,
                height,
                fps,
                subjects,
            } => Ok(Self::pattern(
                id,
                SyntheticPattern::new(*width, *height, *fps, *subjects)?,
            )),
            SourceConfig::File { loop_playback, .. } => {
                let reader = ClipReader::from_config(id, config)?;
                Ok(Self::clip(reader, *loop_playback))
            }
            SourceConfig::Capture { .. } => Err(MediaError::Open(format!(
                "source {}: a capture source is a live camera, which simulated time cannot replay",
                id.0
            ))),
        }
    }

    /// The input showing `pattern` as source `id`.
    pub fn pattern(id: SourceId, pattern: SyntheticPattern) -> Self {
        let info = SourceInfo {
            id,
            width: pattern.width(),
            height: pattern.height(),
            fps: pattern.frame_rate().as_f64(),
            audio: None,
        };
        Self {
            info,
            kind: Kind::Pattern { pattern, next: 0 },
            shown: None,
        }
    }

    /// The input showing the frames of `reader`, which `looping` says
    /// starts again at its end.
    pub fn clip(reader: ClipReader, looping: bool) -> Self {
        Self {
            info: reader.info().clone(),
            kind: Kind::Clip {
                reader: Box::new(reader),
                next: None,
                ended: false,
                looping,
            },
            shown: None,
        }
    }

    /// What the source is; a clip has no audio.
    pub fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// True when the input never runs out of frames: a pattern, or a
    /// looping clip.
    pub fn is_endless(&self) -> bool {
        match &self.kind {
            Kind::Pattern { .. } => true,
            Kind::Clip { looping, .. } => *looping,
        }
    }

    /// True once the frame shown is the last there is (never for an
    /// endless input).
    pub fn has_ended(&self) -> bool {
        match &self.kind {
            Kind::Pattern { .. } => false,
            Kind::Clip { next, ended, .. } => *ended && next.is_none(),
        }
    }

    /// The frame to show at a tick: the newest whose PTS is at most
    /// `limit` (the tick's media time plus the slack of the module docs),
    /// or the one shown before when none is newer. `None` before the
    /// first frame is due. A clip that cannot be decoded is an error.
    pub fn frame_at(&mut self, limit: Duration) -> Result<Option<VideoFrame>, MediaError> {
        match &mut self.kind {
            Kind::Pattern { pattern, next } => {
                let rate = pattern.frame_rate();
                let mut newest = None;
                while rate.pts(*next) <= limit {
                    newest = Some(*next);
                    *next += 1;
                }
                if let Some(seq) = newest {
                    self.shown = Some(pattern.frame(&self.info.id, seq));
                }
            }
            Kind::Clip {
                reader,
                next,
                ended,
                ..
            } => loop {
                if next.is_none() && !*ended {
                    *next = reader.next_frame()?;
                    *ended = next.is_none();
                }
                match next {
                    Some(f) if f.pts <= limit => self.shown = next.take(),
                    _ => break,
                }
            },
        }
        Ok(self.shown.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `v` milliseconds.
    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    /// The pattern input "cam", 64x36 at `fps` with one subject.
    fn pattern(fps: f64) -> Input {
        let p = SyntheticPattern::new(64, 36, fps, 1).unwrap();
        Input::pattern(SourceId("cam".into()), p)
    }

    /// The sequence number shown at `limit`.
    fn seq_at(input: &mut Input, limit: Duration) -> Option<u64> {
        input.frame_at(limit).unwrap().map(|f| f.seq)
    }

    #[test]
    fn a_pattern_shows_its_newest_due_frame() {
        let mut input = pattern(10.0);
        assert_eq!(input.info().fps, 10.0);
        assert_eq!(input.info().width, 64);
        assert_eq!(seq_at(&mut input, ms(0)), Some(0));
        assert_eq!(seq_at(&mut input, ms(99)), Some(0), "frame 1 is not due");
        assert_eq!(seq_at(&mut input, ms(100)), Some(1));
        // Frames between ticks are skipped, not queued.
        let f = input.frame_at(ms(450)).unwrap().unwrap();
        assert_eq!((f.seq, f.pts, f.source.0.as_str()), (4, ms(400), "cam"));
        assert!(input.is_endless() && !input.has_ended());
    }

    #[test]
    fn a_frame_is_shown_again_and_never_goes_back() {
        let mut input = pattern(30.0);
        assert_eq!(seq_at(&mut input, ms(200)), Some(6));
        assert_eq!(seq_at(&mut input, ms(200)), Some(6), "shown again");
        assert_eq!(seq_at(&mut input, ms(50)), Some(6), "an earlier tick");
    }

    #[test]
    fn nothing_is_shown_before_the_first_frame() {
        let mut input = pattern(30.0);
        // Only a tick before media time 0 could see nothing, and there is
        // none; frame 0 is always due at the first tick.
        assert_eq!(seq_at(&mut input, Duration::ZERO), Some(0));
    }

    #[test]
    fn half_a_tick_of_slack_covers_rounding_at_the_tick_rate() {
        // At 30 fps, tick n of a 30 Hz schedule has media time n times the
        // rounded period, a nanosecond or so before frame n's PTS.
        let mut input = pattern(30.0);
        let period = Duration::from_secs_f64(1.0 / 30.0);
        for n in 0..300u64 {
            let t = period * u32::try_from(n).unwrap();
            assert_eq!(seq_at(&mut input, t + period / 2), Some(n), "tick {n}");
        }
    }

    #[test]
    fn a_file_source_that_cannot_be_read_is_an_error() {
        let file = SourceConfig::File {
            path: "/nonexistent/auto-camera/clip.mp4".into(),
            loop_playback: true,
            max_width: None,
            audio: false,
            start_s: None,
            duration_s: None,
        };
        let err = Input::open(SourceId("f".into()), &file).unwrap_err();
        assert!(err.to_string().contains("clip.mp4"), "{err}");
        let bad = SourceConfig::Synthetic {
            width: 0,
            height: 36,
            fps: 30.0,
            subjects: 1,
        };
        assert!(Input::open(SourceId("s".into()), &bad).is_err());
        let camera = SourceConfig::Capture {
            unique_id: "0x1".into(),
            width: 64,
            height: 36,
            fps: 30.0,
            pixel_format: None,
        };
        let err = Input::open(SourceId("cam".into()), &camera).unwrap_err();
        assert!(err.to_string().contains("live camera"), "{err}");
    }
}
