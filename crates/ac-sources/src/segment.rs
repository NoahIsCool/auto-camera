//! A part of a file to play: where to start, and for how long.
//!
//! Scoring framing on long footage (a two-hour service) means scoring
//! three-to-five-minute segments of it, each as if it were a clip of its
//! own. A [`Segment`] says which: `start_s` seconds into the file, for at
//! most `duration_s` seconds (to the end of the file without one). The
//! source rebases time itself: it numbers the frames it reads from 0 and
//! derives PTS from that number (`pts = seq / fps`, as for a whole file),
//! and stamps its audio from sample 0, so the segment's first frame and its
//! first audio sample both have PTS 0.
//!
//! # Where the cut falls
//!
//! A `Cut` is a segment worked out on one file from what ffprobe found in
//! it (`Segment::cut`). A file's timestamps need not start at zero;
//! `start_s` counts from the container's start, as ffmpeg's `-ss` does. The
//! video's frames lie on a grid: frame `n` at the video stream's start plus
//! `n` intervals of the probed rate. The segment starts at the first frame
//! of that grid at or after `start_s` (a frame up to `AT_TOLERANCE`
//! before it counts as at it: Matroska stores times to the millisecond and
//! ffprobe prints them to the microsecond), and a duration covers the
//! frames that start within it, `ceil(duration * fps)` of them.
//!
//! Both decoders cut at the same two instants: the first frame's time, and
//! the end of the last frame's interval. The audio is cut there to the
//! sample, so a start between two frames moves to the next frame for both
//! streams, and the first audio sample plays with the first frame. The
//! video keeps the frames whose timestamps fall from half a frame before
//! the first instant to half a frame before the second, so a frame whose
//! stored time was rounded to its container's timebase, or lies a little
//! off the grid, is still the frame the grid expects. (Variable-rate video
//! lies off the grid: its first frame is the first from half a frame
//! before the cut, in step with the audio only as far as its frames keep
//! to the probed rate.)
//!
//! # Seeking
//!
//! Seeking takes two steps. The decoders first seek with ffmpeg's input
//! `-ss`, to `SEEK_MARGIN` before the first frame rather than to it.
//! Input seeking is only as good as the container's index: a file with one
//! (MP4, Matroska) lands on a keyframe before the target, but one without
//! (MPEG-TS, the usual format of stream and broadcast recordings) lands
//! near the target, possibly past the keyframe its decoder needs, which
//! then delivers nothing until the next one (measured: a start at 2 s in an
//! H.264 TS with a keyframe every 1.6 s began with the frame from 3.2 s,
//! and in an MPEG-2 TS with a single keyframe gave no frame at all). From
//! the margin before, the decoder meets a keyframe before the cut in any
//! footage whose keyframes are closer together than the margin. A first
//! frame within the margin of the file's start is decoded from the start.
//! Then `trim` and `atrim` filters cut exactly at the instants above, on
//! the file's own timestamps, which `-copyts` keeps.
//!
//! Kept timestamps also matter to ffmpeg's constant-rate conversion (`-r`).
//! Shifted to start at the seek point instead, a first frame kept a little
//! after zero would be shown twice (measured: a start at 2.05 s in a 10 fps
//! file delivered frame 21 twice); kept, the conversion starts at that
//! frame.
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
//!   segment from the start adds only `-t`, so it starts as the whole file
//!   does.
//! * Both decoders of a file get the same cut, and the audio starts at the
//!   time of the video's first frame.
//! * Times reach ffmpeg as exact decimals of whole nanoseconds, never
//!   through a float's shortest form.

use std::ffi::OsString;
use std::time::Duration;

use ac_media::MediaError;

use crate::probe::ProbeInfo;

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

    /// Where the segment cuts the file `probe` describes; see the module
    /// docs.
    pub(crate) fn cut(&self, probe: &ProbeInfo) -> Cut {
        if self.start.is_zero() {
            return self.duration.map_or(Cut::Whole, Cut::Head);
        }
        let rate = probe.video.frame_rate;
        let (num, den) = (i128::from(rate.num()), i128::from(rate.den()));
        // `frames` frame intervals, rounded down to the nanosecond.
        let span = |frames: i128| frames * den * NANOS / num;
        let base = probe.start_time.map_or(0, secs_to_nanos);
        let grid = probe.video.start_time.map_or(base, secs_to_nanos);
        let wanted = base + self.start.as_nanos() as i128 - AT_TOLERANCE.as_nanos() as i128;
        let skipped = ceil_div((wanted - grid) * num, den * NANOS).max(0);
        let first = grid + span(skipped);
        let end = self.duration.map(|d| {
            let frames = ceil_div(d.as_nanos() as i128 * num, den * NANOS);
            first + span(frames)
        });
        let seek = first - base - SEEK_MARGIN.as_nanos() as i128;
        Cut::Inside {
            seek: (seek > 0).then(|| duration(seek)),
            first,
            end,
            half_frame: span(1) / 2,
        }
    }
}

/// How far before a segment's first frame the decoders seek: more than the
/// keyframe interval of the footage expected here (streams use 2 s; x264's
/// default of 250 frames is 10.4 s at 24 fps).
const SEEK_MARGIN: Duration = Duration::from_secs(15);

/// How far before `start_s` a frame may be and still count as at it.
const AT_TOLERANCE: Duration = Duration::from_millis(1);

/// Nanoseconds per second.
const NANOS: i128 = 1_000_000_000;

/// Where a [`Segment`] cuts one file; see the module docs. Times inside the
/// file are its own timestamps in nanoseconds, as `-copyts` keeps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cut {
    /// The whole file: no argument.
    Whole,
    /// The file's first part, this long: `-t` on the input.
    Head(Duration),
    /// From a frame inside the file.
    Inside {
        /// Where the input seek goes, from the container's start; `None`
        /// decodes from the start.
        seek: Option<Duration>,
        /// Timestamp of the segment's first frame, where its audio starts.
        first: i128,
        /// Timestamp where its last frame's interval ends, and its audio
        /// with it; `None` at the end of the file.
        end: Option<i128>,
        /// Half a frame interval: how far before the grid a frame's
        /// timestamp may be.
        half_frame: i128,
    },
}

impl Cut {
    /// Options for both decoders that go just before the `-i`.
    pub(crate) fn input_args(&self) -> Vec<OsString> {
        match *self {
            Cut::Whole => Vec::new(),
            Cut::Head(d) => vec!["-t".into(), exact(d.as_nanos() as i128).into()],
            Cut::Inside { seek, .. } => {
                let mut args: Vec<OsString> = vec!["-copyts".into()];
                if let Some(seek) = seek {
                    args.extend(["-ss".into(), exact(seek.as_nanos() as i128).into()]);
                }
                args
            }
        }
    }

    /// The video filter that comes first in the chain, if any: a `trim`
    /// keeping the segment's frames.
    pub(crate) fn video_filter(&self) -> Option<String> {
        let Cut::Inside {
            first,
            end,
            half_frame,
            ..
        } = *self
        else {
            return None;
        };
        let mut trim = format!("trim=start={}", exact(first - half_frame));
        if let Some(end) = end {
            trim.push_str(&format!(":end={}", exact(end - half_frame)));
        }
        Some(trim)
    }

    /// The audio filter, if any: an `atrim` to the segment's instants.
    pub(crate) fn audio_filter(&self) -> Option<String> {
        let Cut::Inside { first, end, .. } = *self else {
            return None;
        };
        let mut trim = format!("atrim=start={}", exact(first));
        if let Some(end) = end {
            trim.push_str(&format!(":end={}", exact(end)));
        }
        Some(trim)
    }
}

/// Largest probed time taken as it is, in seconds (about 30 million
/// years); larger ones are clamped, so no arithmetic on them overflows.
const MAX_PROBED_SECS: f64 = 1e15;

/// `secs` seconds (finite) as whole nanoseconds, rounded to the nearest.
fn secs_to_nanos(secs: f64) -> i128 {
    (secs.clamp(-MAX_PROBED_SECS, MAX_PROBED_SECS) * 1e9).round() as i128
}

/// `nanos` (at least 0) nanoseconds as a duration, saturating.
fn duration(nanos: i128) -> Duration {
    let secs = u64::try_from(nanos / NANOS).unwrap_or(u64::MAX);
    Duration::new(secs, (nanos % NANOS) as u32)
}

/// `a / b` rounded up, for `b > 0`.
fn ceil_div(a: i128, b: i128) -> i128 {
    a.div_euclid(b) + i128::from(a.rem_euclid(b) != 0)
}

/// `nanos` nanoseconds as an exact decimal number of seconds, such as
/// `2.050000000` or `-0.050000000`.
fn exact(nanos: i128) -> String {
    let sign = if nanos < 0 { "-" } else { "" };
    let n = nanos.unsigned_abs();
    format!("{sign}{}.{:09}", n / NANOS as u128, n % NANOS as u128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_rate::FrameRate;
    use crate::probe::VideoStreamInfo;

    /// `args` as strings.
    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
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

    /// What ffprobe finds in a file at `fps` whose container starts at
    /// `start` and whose video starts at `video` (seconds).
    fn probe(fps: &str, start: Option<f64>, video: Option<f64>) -> ProbeInfo {
        ProbeInfo {
            video: VideoStreamInfo {
                index: 0,
                width: 64,
                height: 36,
                frame_rate: FrameRate::parse(fps).unwrap(),
                rotation: 0,
                color_space: None,
                start_time: video,
            },
            audio: None,
            duration: None,
            start_time: start,
        }
    }

    /// The cut of `start_s`, `duration_s` on `probe`.
    fn cut(start_s: f64, duration_s: Option<f64>, probe: &ProbeInfo) -> Cut {
        Segment::from_seconds(Some(start_s), duration_s)
            .unwrap()
            .cut(probe)
    }

    /// The decoder options and filters of `cut`.
    fn parts(cut: &Cut) -> (Vec<String>, Option<String>, Option<String>) {
        (
            strings(&cut.input_args()),
            cut.video_filter(),
            cut.audio_filter(),
        )
    }

    #[test]
    fn the_whole_file_and_its_head_cut_only_on_the_input() {
        let p = probe("10", Some(1.4), Some(1.5));
        assert_eq!(Segment::WHOLE.cut(&p), Cut::Whole);
        assert_eq!(parts(&Cut::Whole), (vec![], None, None));
        let head = Segment::from_seconds(Some(0.0), Some(0.25)).unwrap();
        assert_eq!(head.cut(&p), Cut::Head(Duration::from_millis(250)));
        assert_eq!(
            parts(&head.cut(&p)),
            (vec!["-t".into(), "0.250000000".into()], None, None)
        );
    }

    #[test]
    fn a_start_on_a_frame_cuts_both_streams_at_that_frame() {
        let c = cut(2.0, Some(1.0), &probe("10", None, None));
        assert_eq!(
            c,
            Cut::Inside {
                seek: None,
                first: 2_000_000_000,
                end: Some(3_000_000_000),
                half_frame: 50_000_000,
            }
        );
        assert_eq!(
            parts(&c),
            (
                vec!["-copyts".into()],
                Some("trim=start=1.950000000:end=2.950000000".into()),
                Some("atrim=start=2.000000000:end=3.000000000".into()),
            )
        );
        // To the end of the file: no end on either filter.
        let open = cut(2.0, None, &probe("10", None, None));
        assert_eq!(open.video_filter().unwrap(), "trim=start=1.950000000");
        assert_eq!(open.audio_filter().unwrap(), "atrim=start=2.000000000");
    }

    #[test]
    fn a_start_between_frames_moves_both_streams_to_the_next_frame() {
        let p = probe("10", None, None);
        let c = cut(2.05, Some(0.3), &p);
        assert_eq!(
            c.audio_filter().unwrap(),
            "atrim=start=2.100000000:end=2.400000000"
        );
        assert_eq!(
            c.video_filter().unwrap(),
            "trim=start=2.050000000:end=2.350000000"
        );
        // Up to a millisecond after a frame counts as at it; more does not.
        let at = |s| match cut(s, None, &p) {
            Cut::Inside { first, .. } => first,
            other => panic!("{other:?}"),
        };
        assert_eq!(at(2.0009), 2_000_000_000);
        assert_eq!(at(2.0011), 2_100_000_000);
        assert_eq!(at(0.0005), 0, "the first frame");
    }

    #[test]
    fn the_grid_starts_where_the_video_does() {
        // An MPEG-TS recording: the container starts at 1.4 s, its video
        // at 1.421333 s, at 30000/1001 fps. 2 s in is 3.4 s; the first
        // frame at or after it is frame 60, 2.002 s after the video's
        // start.
        let c = cut(2.0, None, &probe("30000/1001", Some(1.4), Some(1.421333)));
        assert_eq!(
            c,
            Cut::Inside {
                seek: None,
                first: 3_423_333_000,
                end: None,
                half_frame: 16_683_333,
            }
        );
        assert_eq!(c.video_filter().unwrap(), "trim=start=3.406649667");
        assert_eq!(c.audio_filter().unwrap(), "atrim=start=3.423333000");
        // Video that starts after the requested time starts the segment.
        let late = cut(0.05, None, &probe("10", Some(1.4), Some(1.5)));
        assert!(matches!(
            late,
            Cut::Inside {
                first: 1_500_000_000,
                ..
            }
        ));
        // A container without a start time counts from zero.
        let bare = cut(1.0, None, &probe("10", None, Some(0.25)));
        assert!(matches!(
            bare,
            Cut::Inside {
                first: 1_050_000_000,
                ..
            }
        ));
    }

    #[test]
    fn a_far_start_seeks_to_the_margin_before_its_first_frame() {
        let p = probe("10", Some(1.4), Some(1.4));
        let c = cut(100.05, Some(2.0), &p);
        assert_eq!(
            strings(&c.input_args()),
            ["-copyts", "-ss", "85.100000000"],
            "15 s before the frame at 100.1 s, from the container's start"
        );
        assert_eq!(
            c.video_filter().unwrap(),
            "trim=start=101.450000000:end=103.450000000"
        );
        // A first frame within the margin decodes from the start.
        assert_eq!(strings(&cut(15.0, None, &p).input_args()), ["-copyts"]);
        assert_eq!(
            strings(&cut(15.1, None, &p).input_args()),
            ["-copyts", "-ss", "0.100000000"]
        );
    }

    #[test]
    fn a_duration_covers_the_frames_that_start_within_it() {
        let ten = probe("10", None, None);
        let end = |start, d, p: &ProbeInfo| match cut(start, Some(d), p) {
            Cut::Inside { first, end, .. } => end.unwrap() - first,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            end(2.0, 0.25, &ten),
            300_000_000,
            "3 frames start in 0.25 s"
        );
        assert_eq!(end(2.0, 0.3, &ten), 300_000_000);
        assert_eq!(end(2.0, 0.300_000_001, &ten), 400_000_000);
        assert_eq!(end(2.0, 1e-9, &ten), 100_000_000, "at least one frame");
        let ntsc = probe("30000/1001", None, None);
        assert_eq!(end(1.0, 1.0, &ntsc), 1_001_000_000, "30 frames");
        assert_eq!(end(1.0, 1.001, &ntsc), 1_001_000_000);
    }

    #[test]
    fn huge_times_neither_overflow_nor_panic() {
        let s = Segment::from_seconds(Some(1e18), Some(1e18)).unwrap();
        for p in [
            probe("1000", Some(-1e300), Some(1e300)),
            probe("1/1001", Some(1e15), None),
        ] {
            let Cut::Inside { first, end, .. } = s.cut(&p) else {
                panic!("a cut inside the file");
            };
            assert!(end.unwrap() > first);
            assert!(s.cut(&p).video_filter().is_some());
        }
        assert_eq!(exact(-50_000_000), "-0.050000000");
        assert_eq!(duration(i128::MAX).as_secs(), u64::MAX);
    }
}
