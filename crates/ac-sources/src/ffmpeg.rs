//! ffmpeg command lines for decoding a file to raw video and raw audio.
//!
//! Video and audio are decoded by two separate ffmpeg processes, each writing
//! one raw stream to its stdout. Two processes keep both pipes simple
//! (fixed-size records, no demuxing on our side) and let either stream stall
//! without blocking the other.
//!
//! Video output is guaranteed to be exactly `width * height * 4` bytes per
//! frame: the filter chain always ends in an explicit `scale` to the size we
//! computed and a conversion to RGBA, so neither rotation metadata nor odd
//! source sizes can make the byte stream disagree with our framing. `-r` makes
//! the output constant-rate at the probed frame rate (duplicating or dropping
//! frames of variable-rate input), which is what lets PTS be derived from the
//! frame number.
//!
//! A file's [`Segment`](crate::segment::Segment), worked out on the file as
//! a `Cut`, adds its options just before the `-i` and its filter first in
//! the chain, so frames outside the segment are dropped before they are
//! scaled.
//!
//! Colour: YUV-to-RGB conversion needs the right matrix. Tagged files use their
//! tag; untagged files follow the broadcast convention of BT.709 for HD and
//! larger (at least 1280 wide or 720 high) and BT.601 below that, instead of
//! always assuming BT.601.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use ac_media::MediaError;

use crate::frame_rate::FrameRate;
use crate::probe::{AudioStreamInfo, FfmpegTools, VideoStreamInfo, file_url};
use crate::segment::Cut;

/// Checks a file's `max_width` on its own, before any file is probed: below
/// 2 it is invalid (no even width fits), whatever the picture.
/// [`output_size`] applies the same rule, so a config this accepts never
/// fails there for its `max_width`.
pub fn check_max_width(max_width: Option<u32>) -> Result<(), MediaError> {
    match max_width {
        Some(m) if m < 2 => Err(MediaError::Open(format!(
            "max_width must be at least 2, got {m}"
        ))),
        _ => Ok(()),
    }
}

/// Size of decoded frames: the upright picture, scaled down to at most
/// `max_width` wide with the aspect ratio kept.
///
/// When scaling, both sides are rounded to even numbers so frames can be
/// encoded as 4:2:0 later without cropping. A `max_width` below 2 is invalid
/// ([`check_max_width`]).
pub fn output_size(display: (u32, u32), max_width: Option<u32>) -> Result<(u32, u32), MediaError> {
    check_max_width(max_width)?;
    let (w, h) = display;
    match max_width {
        Some(m) if w > m => {
            let out_w = m & !1;
            let half_h = (h as f64 * out_w as f64 / w as f64 / 2.0).round() as u32;
            Ok((out_w, (half_h * 2).max(2)))
        }
        _ => Ok((w, h)),
    }
}

/// The swscale `in_color_matrix` name for a stream; see the module docs.
pub(crate) fn color_matrix(video: &VideoStreamInfo) -> &'static str {
    match video.color_space.as_deref() {
        Some("bt709") => "bt709",
        Some("smpte170m") => "smpte170m",
        Some("bt470bg") => "bt470",
        Some("fcc") => "fcc",
        Some("smpte240m") => "smpte240m",
        Some("bt2020nc" | "bt2020c") => "bt2020",
        _ => untagged_matrix(video.display_size()),
    }
}

/// The swscale `in_color_matrix` name for an untagged picture of `size`
/// (width, height): BT.709 for HD and larger, BT.601 below; see the module
/// docs. Capture devices tag nothing, so they always take this rule
/// ([`crate::capture`]).
pub(crate) fn untagged_matrix((w, h): (u32, u32)) -> &'static str {
    if w >= 1280 || h >= 720 {
        "bt709"
    } else {
        "bt601"
    }
}

/// Options shared by every decoder: quiet, never reading the terminal.
pub(crate) const COMMON: [&str; 4] = ["-nostdin", "-hide_banner", "-loglevel", "error"];

/// Arguments that decode `video` from `path`, cut to `cut`, to raw RGBA
/// frames of `size` at `rate` on stdout.
pub(crate) fn video_args(
    path: &Path,
    video: &VideoStreamInfo,
    size: (u32, u32),
    rate: FrameRate,
    cut: &Cut,
) -> Vec<OsString> {
    let scale = format!(
        "scale=w={}:h={}:in_color_matrix={},format=rgba",
        size.0,
        size.1,
        color_matrix(video)
    );
    let filters = match cut.video_filter() {
        Some(trim) => format!("{trim},{scale}"),
        None => scale,
    };
    let mut args: Vec<OsString> = COMMON.iter().map(OsString::from).collect();
    args.extend(cut.input_args());
    args.push("-i".into());
    args.push(file_url(path));
    for a in [
        "-map".to_string(),
        format!("0:{}", video.index),
        "-vf".into(),
        filters,
        "-pix_fmt".into(),
        "rgba".into(),
        "-r".into(),
        rate.to_string(),
        "-f".into(),
        "rawvideo".into(),
        "pipe:1".into(),
    ] {
        args.push(a.into());
    }
    args
}

/// Arguments that decode `audio` from `path`, cut to `cut`, to interleaved
/// little-endian `f32` samples at the stream's own rate and channel count on
/// stdout.
pub(crate) fn audio_args(path: &Path, audio: &AudioStreamInfo, cut: &Cut) -> Vec<OsString> {
    let mut args: Vec<OsString> = COMMON.iter().map(OsString::from).collect();
    args.extend(cut.input_args());
    args.push("-i".into());
    args.push(file_url(path));
    args.extend(["-map".into(), format!("0:{}", audio.index).into()]);
    if let Some(trim) = cut.audio_filter() {
        args.extend(["-af".into(), trim.into()]);
    }
    for a in [
        "-f".to_string(),
        "f32le".into(),
        "-acodec".into(),
        "pcm_f32le".into(),
        "-ar".into(),
        audio.sample_rate.to_string(),
        "-ac".into(),
        audio.channels.to_string(),
        "pipe:1".into(),
    ] {
        args.push(a.into());
    }
    args
}

/// An ffmpeg command with `args`.
pub(crate) fn command(tools: &FfmpegTools, args: &[OsString]) -> Command {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(args);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A video stream of the given size and colour tag.
    fn video(w: u32, h: u32, color_space: Option<&str>) -> VideoStreamInfo {
        VideoStreamInfo {
            index: 3,
            width: w,
            height: h,
            frame_rate: FrameRate::new(30000, 1001).unwrap(),
            rotation: 0,
            color_space: color_space.map(str::to_string),
            start_time: None,
        }
    }

    #[test]
    fn small_pictures_keep_their_size() {
        assert_eq!(output_size((640, 360), None).unwrap(), (640, 360));
        assert_eq!(output_size((640, 360), Some(640)).unwrap(), (640, 360));
        assert_eq!(output_size((641, 361), Some(1280)).unwrap(), (641, 361));
    }

    #[test]
    fn large_pictures_scale_to_even_sizes_keeping_aspect() {
        assert_eq!(output_size((3840, 2160), Some(1280)).unwrap(), (1280, 720));
        assert_eq!(output_size((1920, 1080), Some(641)).unwrap(), (640, 360));
        assert_eq!(output_size((1080, 1920), Some(540)).unwrap(), (540, 960));
        assert_eq!(output_size((4000, 10), Some(2)).unwrap(), (2, 2));
    }

    #[test]
    fn tiny_max_width_is_rejected() {
        for m in [0, 1] {
            assert!(matches!(
                output_size((640, 360), Some(m)),
                Err(MediaError::Open(_))
            ));
            let err = check_max_width(Some(m)).unwrap_err().to_string();
            assert!(err.contains(&format!("at least 2, got {m}")), "{err}");
        }
        for ok in [None, Some(2), Some(u32::MAX)] {
            assert!(check_max_width(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn colour_matrix_follows_tag_then_resolution() {
        assert_eq!(color_matrix(&video(640, 480, Some("bt709"))), "bt709");
        assert_eq!(color_matrix(&video(1920, 1080, Some("bt470bg"))), "bt470");
        assert_eq!(color_matrix(&video(1920, 1080, Some("bt2020nc"))), "bt2020");
        assert_eq!(color_matrix(&video(1920, 1080, None)), "bt709");
        assert_eq!(color_matrix(&video(1280, 720, Some("unknown"))), "bt709");
        assert_eq!(color_matrix(&video(1280, 544, None)), "bt709");
        assert_eq!(color_matrix(&video(720, 480, None)), "bt601");
    }

    #[test]
    fn video_command_forces_size_format_and_rate() {
        let args = video_args(
            Path::new("/v/a b.mp4"),
            &video(1920, 1080, None),
            (1280, 720),
            FrameRate::new(30000, 1001).unwrap(),
            &Cut::Whole,
        );
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into()).collect();
        let at = |flag: &str| &joined[joined.iter().position(|a| a == flag).unwrap() + 1];
        assert_eq!(at("-i"), "file:/v/a b.mp4");
        assert_eq!(at("-map"), "0:3");
        assert_eq!(
            at("-vf"),
            "scale=w=1280:h=720:in_color_matrix=bt709,format=rgba"
        );
        assert_eq!(at("-r"), "30000/1001");
        assert_eq!(at("-f"), "rawvideo");
        assert!(joined.contains(&"-nostdin".to_string()));
        assert_eq!(joined.last().unwrap(), "pipe:1");
    }

    #[test]
    fn audio_command_keeps_source_rate_and_channels() {
        let a = AudioStreamInfo {
            index: 1,
            sample_rate: 44_100,
            channels: 6,
        };
        let joined = strings(&audio_args(Path::new("/a.mkv"), &a, &Cut::Whole));
        let at = |flag: &str| &joined[joined.iter().position(|a| a == flag).unwrap() + 1];
        assert_eq!(at("-map"), "0:1");
        assert_eq!(at("-f"), "f32le");
        assert_eq!(at("-ar"), "44100");
        assert_eq!(at("-ac"), "6");
        assert!(!joined.contains(&"-af".to_string()));
    }

    /// `args` as strings.
    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into()).collect()
    }

    #[test]
    fn a_cut_seeks_both_decoders_alike_and_trims_before_scaling() {
        let cut = Cut::Inside {
            seek: Some(std::time::Duration::from_millis(85_100)),
            first: 101_500_000_000,
            end: Some(103_500_000_000),
            half_frame: 50_000_000,
        };
        let rate = FrameRate::new(10, 1).unwrap();
        let video = strings(&video_args(
            Path::new("/a.ts"),
            &video(64, 36, None),
            (64, 36),
            rate,
            &cut,
        ));
        let a = AudioStreamInfo {
            index: 1,
            sample_rate: 48_000,
            channels: 2,
        };
        let audio = strings(&audio_args(Path::new("/a.ts"), &a, &cut));
        for args in [&video, &audio] {
            let i = args.iter().position(|a| a == "-i").unwrap();
            assert_eq!(&args[i - 3..=i], ["-copyts", "-ss", "85.100000000", "-i"]);
        }
        let after = |args: &[String], flag: &str| {
            args[args.iter().position(|a| a == flag).unwrap() + 1].clone()
        };
        assert_eq!(
            after(&video, "-vf"),
            "trim=start=101.450000000:end=103.450000000,\
             scale=w=64:h=36:in_color_matrix=bt601,format=rgba"
        );
        assert_eq!(
            after(&audio, "-af"),
            "atrim=start=101.500000000:end=103.500000000"
        );
        assert!(!video.contains(&"-t".to_string()));
    }
}
