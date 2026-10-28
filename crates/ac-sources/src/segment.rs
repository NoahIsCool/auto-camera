//! A part of a file to play: where to start, and for how long.
//!
//! Scoring framing on long footage (a two-hour service) means scoring
//! three-to-five-minute segments of it, each as if it were a clip of its
//! own. A [`Segment`] says which: `start_s` seconds into the file, for at
//! most `duration_s` seconds (to the end of the file without one).
//!
//! # Seeking
//!
//! The decoders seek with ffmpeg's *input* seeking (`-ss` before `-i`):
//! ffmpeg jumps to the keyframe at or before the start, decodes from there
//! and drops every frame before the start (accurate seeking, its default
//! whenever it decodes). `-t` before `-i` ends the input after the
//! duration. The source then rebases time itself: it numbers the frames it
//! reads from 0 and derives PTS from that number (`pts = seq / fps`, as for
//! a whole file), so the first frame delivered, the file's first frame at
//! or after `start_s` (the frame *at* `start_s` when it falls on one), has
//! sequence number 0 and PTS 0.
//!
//! ffmpeg's own timestamps only drive its constant-rate conversion (`-r`),
//! and they are kept as they are in the file (`-copyts`). Shifted to start
//! at the seek point instead, a start between two frames would put the
//! first frame kept half a frame or less after zero, and the conversion
//! would show it twice (measured: a start at 2.05 s in a 10 fps file
//! delivered frame 21 twice). Kept, the conversion starts at that frame.
//! Audio is cut with the same arguments, so both streams start together.
//!
//! A looping source loops its segment. A detection log records PTS on the
//! segment's own timeline, so it replays over the segment it was recorded
//! on (`ac_vision::replay`).
//!
//! # Invariants
//!
//! * `start` is finite and at least 0; a `duration`, when given, is finite
//!   and more than 0 ([`Segment::from_seconds`] refuses anything else).
//! * The whole file ([`Segment::WHOLE`]) adds no argument, so its decoder
//!   command lines are exactly those of a file without a segment; a
//!   segment from the start adds only `-t`.
//! * Times reach ffmpeg as exact decimals of whole nanoseconds, never
//!   through a float's shortest form.

use std::ffi::OsString;
use std::time::Duration;

use ac_media::MediaError;

/// Which part of a file to play; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Segment {
    /// How far into the file to start.
    start: Duration,
    /// How long to play from there; `None` to the end of the file.
    duration: Option<Duration>,
}

impl Segment {
    /// The whole file.
    pub const WHOLE: Segment = Segment {
        start: Duration::ZERO,
        duration: None,
    };

    /// The segment `start_s` seconds into the file (0 when `None`), lasting
    /// `duration_s` seconds (to the end when `None`). Refuses a start that
    /// is negative or not finite and a duration that is not more than 0 or
    /// not finite.
    pub fn from_seconds(start_s: Option<f64>, duration_s: Option<f64>) -> Result<Self, MediaError> {
        let start = match start_s {
            None => Duration::ZERO,
            Some(s) => Duration::try_from_secs_f64(s).map_err(|_| {
                MediaError::Open(format!(
                    "start_s must be a finite number of seconds, at least 0; got {s}"
                ))
            })?,
        };
        let duration = match duration_s {
            None => None,
            Some(d) => Some(
                Duration::try_from_secs_f64(d)
                    .ok()
                    .filter(|d| !d.is_zero())
                    .ok_or_else(|| {
                        MediaError::Open(format!(
                            "duration_s must be a finite number of seconds, more than 0; got {d}"
                        ))
                    })?,
            ),
        };
        Ok(Self { start, duration })
    }

    /// How far into the file the segment starts.
    pub fn start(&self) -> Duration {
        self.start
    }

    /// How long it lasts; `None` to the end of the file.
    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// True for the whole file.
    pub fn is_whole(&self) -> bool {
        *self == Self::WHOLE
    }

    /// Decoder `args` (one `-i` input) cut to the segment: `-copyts`, `-ss`
    /// and `-t` go in just before the `-i`, the last two as input options
    /// (see the module docs). The whole file, or arguments without `-i`,
    /// come back unchanged.
    pub(crate) fn apply(&self, mut args: Vec<OsString>) -> Vec<OsString> {
        let Some(at) = args.iter().position(|a| a == "-i") else {
            return args;
        };
        let mut cut: Vec<OsString> = Vec::new();
        if !self.start.is_zero() {
            cut.extend(["-copyts".into(), "-ss".into(), seconds(self.start).into()]);
        }
        if let Some(d) = self.duration {
            cut.extend(["-t".into(), seconds(d).into()]);
        }
        args.splice(at..at, cut);
        args
    }
}

/// `d` as an exact decimal number of seconds, such as `2.050000000`.
fn seconds(d: Duration) -> String {
    format!("{}.{:09}", d.as_secs(), d.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `args` as strings.
    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// A decoder command line with one input.
    fn command() -> Vec<OsString> {
        ["-nostdin", "-i", "file:/a.mp4", "-f", "rawvideo", "pipe:1"]
            .map(OsString::from)
            .to_vec()
    }

    #[test]
    fn seconds_become_a_segment() {
        let s = Segment::from_seconds(Some(2.0), Some(180.5)).unwrap();
        assert_eq!(s.start(), Duration::from_secs(2));
        assert_eq!(s.duration(), Some(Duration::from_millis(180_500)));
        assert!(!s.is_whole());
        let open = Segment::from_seconds(Some(90.0), None).unwrap();
        assert_eq!(open.duration(), None);
        assert_eq!(Segment::from_seconds(None, None).unwrap(), Segment::WHOLE);
        assert_eq!(
            Segment::from_seconds(Some(0.0), None).unwrap(),
            Segment::WHOLE
        );
        assert!(Segment::default().is_whole());
    }

    #[test]
    fn bad_seconds_are_refused_with_the_key() {
        for start in [-1.0, f64::NAN, f64::INFINITY, 1e300] {
            let err = Segment::from_seconds(Some(start), None).unwrap_err();
            assert!(
                err.to_string().contains("start_s must be"),
                "{start}: {err}"
            );
        }
        for duration in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            let err = Segment::from_seconds(None, Some(duration)).unwrap_err();
            assert!(
                err.to_string().contains("duration_s must be"),
                "{duration}: {err}"
            );
        }
        // Less than a nanosecond rounds to nothing, which is no duration.
        assert!(Segment::from_seconds(None, Some(1e-12)).is_err());
    }

    #[test]
    fn a_segment_seeks_and_ends_on_the_input() {
        let s = Segment::from_seconds(Some(2.05), Some(3.0)).unwrap();
        assert_eq!(
            strings(&s.apply(command())),
            [
                "-nostdin",
                "-copyts",
                "-ss",
                "2.050000000",
                "-t",
                "3.000000000",
                "-i",
                "file:/a.mp4",
                "-f",
                "rawvideo",
                "pipe:1"
            ]
        );
        let start_only = Segment::from_seconds(Some(1.5), None).unwrap();
        let args = strings(&start_only.apply(command()));
        assert_eq!(&args[1..5], ["-copyts", "-ss", "1.500000000", "-i"]);
        assert!(!args.contains(&"-t".to_string()));
        let length_only = Segment::from_seconds(None, Some(0.25)).unwrap();
        let args = strings(&length_only.apply(command()));
        assert_eq!(&args[1..4], ["-t", "0.250000000", "-i"]);
    }

    #[test]
    fn the_whole_file_leaves_the_command_alone() {
        assert_eq!(Segment::WHOLE.apply(command()), command());
        let no_input: Vec<OsString> = vec!["-version".into()];
        let s = Segment::from_seconds(Some(1.0), None).unwrap();
        assert_eq!(s.apply(no_input.clone()), no_input);
    }
}
