//! The format a capture source asks its device for, and the ffmpeg command
//! line that captures it.
//!
//! # Theory
//!
//! A camera offers modes: picture sizes, each at some frame-rate ranges,
//! in some pixel formats. ffmpeg's avfoundation input takes the wanted mode
//! as input options (`-video_size`, `-framerate`, `-pixel_format`) and
//! refuses a size or a rate the device does not offer: it says so on
//! stderr, lists the modes the device has, and exits without a frame. So a
//! request is checked twice:
//!
//! * when the config is read, for what no device could deliver
//!   ([`request_problems`], [`CaptureFormat::new`]): a size outside
//!   `1..=`[`CaptureFormat::MAX_SIDE`], a rate that is not a usable
//!   [`FrameRate`], a pixel format ffmpeg's avfoundation input does not
//!   know ([`PIXEL_FORMATS`]), an empty unique ID;
//! * by the device, at every open: a mode it lacks makes the connection
//!   fail at once, with ffmpeg's explanation as the source's error
//!   ([`crate::capture`]).
//!
//! Size and rate are required. The rate also defines the source's PTS, and
//! avfoundation's own default (29.97) is refused by the many webcams that
//! offer exactly 30. The pixel format is optional, but the device is always
//! asked for one: avfoundation's own default is yuv420p, which macOS's
//! capture output does not offer for a webcam or a dongle, so leaving it to
//! ffmpeg would answer every open with error-level lines ("Selected pixel
//! format (yuv420p) is not supported by the input device" and the formats it
//! has) that reach the log as warnings and, read as the first stderr lines of
//! the connection, would head the error of any later failure. Without one in
//! the config the source asks for [`DEFAULT_PIXEL_FORMAT`], the first of the
//! formats macOS offers and the one ffmpeg would fall back to. A device that
//! lacks the format asked for is still given one it has (after those lines),
//! since the picture is converted to RGBA anyway.
//!
//! # Invariants
//!
//! * The command line always delivers exactly the requested size as packed
//!   RGBA at the requested rate, as for files ([`crate::ffmpeg`]): the
//!   filter chain ends in an explicit `scale` and `format=rgba`, and `-r`
//!   makes the rate constant, so PTS is `seq / fps` and every read of
//!   `width * height * 4` bytes is one frame.
//! * Capture devices tag no colour matrix, so the untagged rule applies:
//!   BT.709 at HD and above, BT.601 below.
//! * A pixel format is always asked for: the configured one, else
//!   [`DEFAULT_PIXEL_FORMAT`]; never avfoundation's yuv420p by omission.
//! * Only the video is captured (`-i "<index>:none"`); camera audio is not
//!   part of a capture source yet.

use std::ffi::OsString;

use ac_media::MediaError;

use super::devices::ResolvedDevice;
use crate::ffmpeg::{COMMON, untagged_matrix};
use crate::frame_rate::FrameRate;

/// The pixel formats ffmpeg's avfoundation input can ask a device for, by
/// ffmpeg's names (libavdevice's `avf_pixel_formats`).
pub const PIXEL_FORMATS: [&str; 23] = [
    "monob",
    "rgb555be",
    "rgb555le",
    "rgb565be",
    "rgb565le",
    "rgb24",
    "bgr24",
    "0rgb",
    "bgr0",
    "0bgr",
    "rgb0",
    "bgr48be",
    "uyvy422",
    "yuva444p",
    "yuva444p16le",
    "yuv444p",
    "yuv422p16",
    "yuv422p10",
    "yuv444p10",
    "yuv420p",
    "nv12",
    "yuyv422",
    "gray",
];

/// The pixel format asked for when the config names none: packed 4:2:2,
/// which macOS's capture output lists first for webcams and capture dongles
/// (see the module docs).
pub const DEFAULT_PIXEL_FORMAT: &str = "uyvy422";

/// A checked capture request: the mode asked of the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFormat {
    /// Picture width in pixels.
    width: u32,
    /// Picture height in pixels.
    height: u32,
    /// Frames per second; also the source's PTS rate.
    rate: FrameRate,
    /// The pixel format the config names, if any (one of
    /// [`PIXEL_FORMATS`]); [`DEFAULT_PIXEL_FORMAT`] is asked for without.
    pixel_format: Option<String>,
}

impl CaptureFormat {
    /// Largest width or height accepted, in pixels (8K).
    pub const MAX_SIDE: u32 = 8192;

    /// A request for `width` x `height` at `fps`, in `pixel_format` or else
    /// [`DEFAULT_PIXEL_FORMAT`]; an error naming the first of
    /// [`request_problems`] (with no unique ID in question).
    pub fn new(
        width: u32,
        height: u32,
        fps: f64,
        pixel_format: Option<&str>,
    ) -> Result<Self, MediaError> {
        let problems = format_problems(width, height, fps, pixel_format);
        if let Some(first) = problems.into_iter().next() {
            return Err(MediaError::Open(first));
        }
        let rate = FrameRate::from_f64(fps)
            .ok_or_else(|| MediaError::Open(format!("capture fps {fps} is unusable")))?;
        Ok(Self {
            width,
            height,
            rate,
            pixel_format: pixel_format.map(|p| p.trim().to_owned()),
        })
    }

    /// The picture size (width, height).
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The frame rate.
    pub fn rate(&self) -> FrameRate {
        self.rate
    }

    /// The pixel format the config names, if any.
    pub fn pixel_format(&self) -> Option<&str> {
        self.pixel_format.as_deref()
    }

    /// The pixel format the device is asked for: the configured one, else
    /// [`DEFAULT_PIXEL_FORMAT`].
    pub fn requested_pixel_format(&self) -> &str {
        self.pixel_format().unwrap_or(DEFAULT_PIXEL_FORMAT)
    }
}

/// Every problem with a capture request that no device could mend (see
/// the module docs), in the order of the config keys; empty when it is
/// valid. `SourceConfig::validate` fails on the first, the engine's
/// config check lists them all.
pub fn request_problems(
    unique_id: &str,
    width: u32,
    height: u32,
    fps: f64,
    pixel_format: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    if unique_id.trim().is_empty() {
        out.push(
            "capture unique_id is empty; system_profiler SPCameraDataType lists each \
             camera's Unique ID"
                .into(),
        );
    } else if unique_id.chars().any(char::is_control) {
        out.push(format!(
            "capture unique_id {unique_id:?} has control characters"
        ));
    }
    out.extend(format_problems(width, height, fps, pixel_format));
    out
}

/// [`request_problems`] of the format alone.
fn format_problems(width: u32, height: u32, fps: f64, pixel_format: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let side = 1..=CaptureFormat::MAX_SIDE;
    if !side.contains(&width) || !side.contains(&height) {
        out.push(format!(
            "capture size {width}x{height} is outside 1..={}",
            CaptureFormat::MAX_SIDE
        ));
    }
    if FrameRate::from_f64(fps).is_none() {
        out.push(format!(
            "capture fps {fps} is not a usable frame rate (more than 0, at most {})",
            FrameRate::MAX_FPS
        ));
    }
    if let Some(p) = pixel_format
        && !PIXEL_FORMATS.contains(&p.trim())
    {
        out.push(format!(
            "capture pixel_format {p:?} is not one ffmpeg's avfoundation input knows ({})",
            PIXEL_FORMATS.join(", ")
        ));
    }
    out
}

/// The ffmpeg input options that open `device` in `format`: avfoundation,
/// the requested mode (always with a pixel format), and the device's video
/// only.
pub(crate) fn avfoundation_input(device: &ResolvedDevice, format: &CaptureFormat) -> Vec<OsString> {
    let args: Vec<String> = vec![
        "-f".into(),
        "avfoundation".into(),
        "-video_size".into(),
        format!("{}x{}", format.width, format.height),
        "-framerate".into(),
        format.rate.to_string(),
        "-pixel_format".into(),
        format.requested_pixel_format().into(),
        "-i".into(),
        format!("{}:none", device.index),
    ];
    args.into_iter().map(OsString::from).collect()
}

/// The whole ffmpeg command line of a capture connection: `input` (the
/// options that open the device, ending in its `-i`), then raw RGBA frames
/// of the requested size at the requested rate on stdout.
pub(crate) fn capture_args(input: Vec<OsString>, format: &CaptureFormat) -> Vec<OsString> {
    let size = format.size();
    let mut args: Vec<OsString> = COMMON.iter().map(OsString::from).collect();
    args.extend(input);
    for a in [
        "-map".to_string(),
        "0:v:0".into(),
        "-vf".into(),
        format!(
            "scale=w={}:h={}:in_color_matrix={},format=rgba",
            size.0,
            size.1,
            untagged_matrix(size)
        ),
        "-pix_fmt".into(),
        "rgba".into(),
        "-r".into(),
        format.rate.to_string(),
        "-f".into(),
        "rawvideo".into(),
        "pipe:1".into(),
    ] {
        args.push(a.into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `args` as strings.
    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into()).collect()
    }

    /// The argument after `flag` in `args`.
    fn after<'a>(args: &'a [String], flag: &str) -> &'a str {
        &args[args.iter().position(|a| a == flag).unwrap() + 1]
    }

    /// A resolved device at `index`.
    fn at(index: u32) -> ResolvedDevice {
        ResolvedDevice {
            unique_id: "0x1".into(),
            name: "Cam".into(),
            index,
            same_name: 1,
        }
    }

    #[test]
    fn a_request_keeps_its_mode() {
        let f = CaptureFormat::new(1920, 1080, 29.97, Some(" uyvy422 ")).unwrap();
        assert_eq!(f.size(), (1920, 1080));
        assert_eq!(f.rate(), FrameRate::new(30000, 1001).unwrap());
        assert_eq!(f.pixel_format(), Some("uyvy422"));
        assert_eq!(f.requested_pixel_format(), "uyvy422");
        let f = CaptureFormat::new(1, 1, 1000.0, Some("nv12")).unwrap();
        assert_eq!(f.requested_pixel_format(), "nv12");
        let f = CaptureFormat::new(1, 1, 1000.0, None).unwrap();
        assert_eq!((f.size(), f.pixel_format()), ((1, 1), None));
        assert_eq!(f.requested_pixel_format(), DEFAULT_PIXEL_FORMAT);
        assert!(PIXEL_FORMATS.contains(&DEFAULT_PIXEL_FORMAT));
    }

    #[test]
    fn requests_no_device_could_deliver_are_refused() {
        let cases: [(u32, u32, f64, Option<&str>, &str); 7] = [
            (0, 720, 30.0, None, "size 0x720"),
            (1280, 8193, 30.0, None, "size 1280x8193"),
            (1280, 720, 0.0, None, "fps 0"),
            (1280, 720, f64::NAN, None, "fps NaN"),
            (1280, 720, 1001.0, None, "fps 1001"),
            (1280, 720, 30.0, Some("yuv420"), "pixel_format \"yuv420\""),
            (1280, 720, 30.0, Some(""), "pixel_format \"\""),
        ];
        for (w, h, fps, pf, needle) in cases {
            let e = CaptureFormat::new(w, h, fps, pf).unwrap_err().to_string();
            assert!(e.contains(needle), "{needle}: {e}");
        }
    }

    #[test]
    fn every_problem_is_listed_with_the_unique_id_first() {
        let all = request_problems(" ", 0, 0, -1.0, Some("rgba"));
        assert_eq!(all.len(), 4, "{all:?}");
        assert!(all[0].contains("unique_id is empty"), "{all:?}");
        assert!(all[1].contains("size 0x0"));
        assert!(all[2].contains("fps -1"));
        assert!(all[3].contains("\"rgba\"") && all[3].contains("nv12"));
        let control = request_problems("0x1\n", 64, 36, 30.0, None);
        assert_eq!(control.len(), 1);
        assert!(control[0].contains("control characters"), "{control:?}");
        assert!(request_problems("0x1", 64, 36, 30.0, Some("nv12")).is_empty());
    }

    #[test]
    fn the_device_is_opened_by_index_in_the_requested_mode_without_audio() {
        let f = CaptureFormat::new(1280, 720, 30.0, Some("nv12")).unwrap();
        let args = strings(&avfoundation_input(&at(3), &f));
        assert_eq!(
            args,
            [
                "-f",
                "avfoundation",
                "-video_size",
                "1280x720",
                "-framerate",
                "30/1",
                "-pixel_format",
                "nv12",
                "-i",
                "3:none"
            ]
        );
        // Without a pixel format in the config one the camera has is asked
        // for, not left to avfoundation's yuv420p.
        let f = CaptureFormat::new(640, 480, 29.97, None).unwrap();
        let args = strings(&avfoundation_input(&at(0), &f));
        assert_eq!(after(&args, "-framerate"), "30000/1001");
        assert_eq!(after(&args, "-pixel_format"), "uyvy422");
        assert_eq!(args.last().unwrap(), "0:none");
    }

    #[test]
    fn the_capture_command_forces_size_format_and_rate() {
        let f = CaptureFormat::new(1920, 1080, 59.94, None).unwrap();
        let args = strings(&capture_args(avfoundation_input(&at(1), &f), &f));
        assert_eq!(
            &args[..4],
            ["-nostdin", "-hide_banner", "-loglevel", "error"]
        );
        assert_eq!(after(&args, "-i"), "1:none");
        assert_eq!(after(&args, "-map"), "0:v:0");
        assert_eq!(
            after(&args, "-vf"),
            "scale=w=1920:h=1080:in_color_matrix=bt709,format=rgba"
        );
        assert_eq!(after(&args, "-r"), "60000/1001");
        assert_eq!(after(&args, "-f"), "avfoundation", "the input's format");
        assert_eq!(args[args.len() - 3..], ["-f", "rawvideo", "pipe:1"]);
        let sd = CaptureFormat::new(640, 480, 30.0, None).unwrap();
        let args = strings(&capture_args(Vec::new(), &sd));
        assert!(after(&args, "-vf").contains("in_color_matrix=bt601"));
    }
}
