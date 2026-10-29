//! Locating the ffmpeg tools and probing media files with `ffprobe`.
//!
//! The tools are found once, the same way for decoding and for the program
//! encoder: the `AC_FFMPEG` / `AC_FFPROBE` variables, else the bare name on
//! `PATH`. On macOS an app started from Finder, the Dock or launchd gets a
//! `PATH` without Homebrew's directories, so a name that is not on `PATH`
//! is also looked for in `/opt/homebrew/bin` and `/usr/local/bin`.
//!
//! Probing runs `ffprobe -print_format json -show_streams -show_format` once
//! when a file is opened and reads the first real video stream and the first
//! audio stream. Cover art (a video stream with the `attached_pic`
//! disposition) is not a picture source and is skipped. An audio-only input
//! ([`crate::audio_input`]) reads the first audio stream alone
//! ([`probe_audio`]), whatever else the file holds.
//!
//! Parsing is separate from running the tool ([`parse_probe`]), so it is
//! tested against canned ffprobe output without ffmpeg installed.
//!
//! Start times: a container's timestamps need not start at zero (an MPEG-TS
//! recording often starts a second or more in), and its video need not start
//! where the container does. Both are read, as seconds, so a
//! [`Segment`](crate::segment::Segment) can find its frames on the file's own
//! timeline.
//!
//! Frame rate: ffprobe reports `avg_frame_rate` (the measured average) and
//! `r_frame_rate` (the timebase-derived "real" rate, which for variable-rate
//! phone footage can be a meaningless 90000/1). The average is preferred; the
//! decoder later resamples to this rate with constant frame timing, so PTS can
//! be derived from frame numbers.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use ac_media::MediaError;
use serde::Deserialize;

use crate::frame_rate::FrameRate;

/// Environment variable overriding the `ffmpeg` executable.
pub const FFMPEG_ENV: &str = "AC_FFMPEG";
/// Environment variable overriding the `ffprobe` executable.
pub const FFPROBE_ENV: &str = "AC_FFPROBE";

/// Where a tool missing from `PATH` is also looked for on macOS (Homebrew on
/// Apple Silicon, then on Intel).
const MAC_TOOL_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// `name` when a directory of `search_path` holds it (or nothing does),
/// else the first of `fallbacks` that holds it, as a full path.
fn locate(name: &str, search_path: Option<&std::ffi::OsStr>, fallbacks: &[&str]) -> PathBuf {
    let on_path =
        search_path.is_some_and(|p| std::env::split_paths(p).any(|d| d.join(name).is_file()));
    if on_path {
        return PathBuf::from(name);
    }
    fallbacks
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// The default location of tool `name`; see the module docs.
fn default_tool(name: &str) -> PathBuf {
    let fallbacks: &[&str] = if cfg!(target_os = "macos") {
        &MAC_TOOL_DIRS
    } else {
        &[]
    };
    locate(name, std::env::var_os("PATH").as_deref(), fallbacks)
}

/// Locations of the `ffmpeg` and `ffprobe` executables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegTools {
    /// The `ffmpeg` executable (a bare name is looked up on `PATH`).
    pub ffmpeg: PathBuf,
    /// The `ffprobe` executable (a bare name is looked up on `PATH`).
    pub ffprobe: PathBuf,
}

impl FfmpegTools {
    /// The tools named by [`FFMPEG_ENV`] and [`FFPROBE_ENV`], defaulting to
    /// `ffmpeg` and `ffprobe` on `PATH` (and, on macOS, in Homebrew's
    /// directories when they are not on `PATH`).
    pub fn from_env() -> Self {
        let pick = |var: &str, default: &str| {
            std::env::var_os(var)
                .filter(|v| !v.is_empty())
                .map_or_else(|| default_tool(default), PathBuf::from)
        };
        Self {
            ffmpeg: pick(FFMPEG_ENV, "ffmpeg"),
            ffprobe: pick(FFPROBE_ENV, "ffprobe"),
        }
    }

    /// True when both tools run. Tests use this to skip cleanly.
    pub fn is_available(&self) -> bool {
        [&self.ffmpeg, &self.ffprobe].iter().all(|tool| {
            Command::new(tool)
                .arg("-version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
    }
}

impl Default for FfmpegTools {
    fn default() -> Self {
        Self::from_env()
    }
}

/// The video stream a file source decodes.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoStreamInfo {
    /// Absolute stream index in the container.
    pub index: u32,
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Frame rate the stream is decoded at.
    pub frame_rate: FrameRate,
    /// Display rotation in degrees, normalized to `0..360`.
    pub rotation: u32,
    /// ffprobe's `color_space` (the YUV matrix), such as `bt709`, if tagged.
    pub color_space: Option<String>,
    /// Timestamp of the stream's first frame in seconds, when known.
    pub start_time: Option<f64>,
}

impl VideoStreamInfo {
    /// Size of the upright picture: width and height swap for a quarter turn,
    /// matching ffmpeg's automatic rotation.
    pub fn display_size(&self) -> (u32, u32) {
        if self.rotation % 180 == 90 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

/// The audio stream a file source decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStreamInfo {
    /// Absolute stream index in the container.
    pub index: u32,
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u16,
}

/// What a file contains, as far as a source needs to know.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeInfo {
    /// The first video stream that is not cover art.
    pub video: VideoStreamInfo,
    /// The first audio stream, if any.
    pub audio: Option<AudioStreamInfo>,
    /// Container duration, when known.
    pub duration: Option<Duration>,
    /// Timestamp the container starts at in seconds, when known: where
    /// ffmpeg's `-ss` counts from.
    pub start_time: Option<f64>,
}

/// Runs ffprobe on `path` and parses the result.
pub fn probe(tools: &FfmpegTools, path: &Path) -> Result<ProbeInfo, MediaError> {
    let json = run_ffprobe(tools, path)?;
    parse_probe(&json).map_err(|e| in_file(path, e))
}

/// Runs ffprobe on `path` and parses its first audio stream, for an
/// audio-only input; a file without one is an error.
pub fn probe_audio(tools: &FfmpegTools, path: &Path) -> Result<AudioStreamInfo, MediaError> {
    let json = run_ffprobe(tools, path)?;
    parse_audio_probe(&json).map_err(|e| in_file(path, e))
}

/// `e` with the name of the file it is about.
fn in_file(path: &Path, e: MediaError) -> MediaError {
    match e {
        MediaError::Open(msg) => MediaError::Open(format!("{}: {msg}", path.display())),
        other => other,
    }
}

/// ffprobe's JSON description of `path`.
fn run_ffprobe(tools: &FfmpegTools, path: &Path) -> Result<String, MediaError> {
    let output = Command::new(&tools.ffprobe)
        .args(["-v", "error", "-print_format", "json"])
        .args(["-show_streams", "-show_format"])
        .arg(file_url(path))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            MediaError::Open(format!(
                "cannot run {} (install ffmpeg or set {FFPROBE_ENV}): {e}",
                tools.ffprobe.display()
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MediaError::Open(format!(
            "ffprobe failed on {}: {}",
            path.display(),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `path` as an ffmpeg `file:` URL, so names that look like options or other
/// protocols (`-x.mp4`, `rtmp:foo`) are read as plain local files.
pub(crate) fn file_url(path: &Path) -> OsString {
    let mut url = OsString::from("file:");
    url.push(path.as_os_str());
    url
}

/// Top level of ffprobe's JSON output.
#[derive(Debug, Deserialize)]
struct RawProbe {
    /// All streams in the container.
    #[serde(default)]
    streams: Vec<RawStream>,
    /// Container-level information.
    format: Option<RawFormat>,
}

/// Container-level fields used here.
#[derive(Debug, Deserialize)]
struct RawFormat {
    /// Duration in seconds, as a decimal string.
    duration: Option<String>,
    /// Start timestamp in seconds, as a decimal string.
    start_time: Option<String>,
}

/// Stream fields used here; every field is optional in ffprobe's output.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawStream {
    /// Absolute stream index.
    index: u32,
    /// `video`, `audio`, `subtitle`, `data`, …
    codec_type: Option<String>,
    /// Coded width.
    width: Option<u32>,
    /// Coded height.
    height: Option<u32>,
    /// Average frame rate, such as `30000/1001`.
    avg_frame_rate: Option<String>,
    /// Timebase-derived frame rate.
    r_frame_rate: Option<String>,
    /// YUV matrix tag.
    color_space: Option<String>,
    /// Start timestamp in seconds, as a decimal string.
    start_time: Option<String>,
    /// Sample rate as a decimal string.
    sample_rate: Option<String>,
    /// Channel count.
    channels: Option<u16>,
    /// Stream disposition flags.
    disposition: Option<RawDisposition>,
    /// Side data, including the display matrix.
    side_data_list: Vec<RawSideData>,
    /// Free-form tags; older muxers store rotation as `rotate`.
    tags: Option<RawTags>,
}

/// Disposition flags used here.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawDisposition {
    /// 1 for cover art.
    attached_pic: u8,
}

/// Side data entries; only the display matrix rotation is used.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawSideData {
    /// Counter-clockwise rotation in degrees.
    rotation: Option<f64>,
}

/// Stream tags used here.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawTags {
    /// Clockwise rotation in degrees, as a decimal string.
    rotate: Option<String>,
}

/// Reads ffprobe's JSON output.
fn read_raw(json: &str) -> Result<RawProbe, MediaError> {
    serde_json::from_str(json)
        .map_err(|e| MediaError::Open(format!("unreadable ffprobe output: {e}")))
}

/// Parses the first audio stream of ffprobe JSON (from `-show_streams`);
/// one without an audio stream is an error.
pub fn parse_audio_probe(json: &str) -> Result<AudioStreamInfo, MediaError> {
    let raw = read_raw(json)?;
    let audio = raw
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("audio"))
        .ok_or_else(|| MediaError::Open("no audio stream".into()))?;
    parse_audio(audio)
}

/// Parses ffprobe JSON (from `-show_streams -show_format`).
pub fn parse_probe(json: &str) -> Result<ProbeInfo, MediaError> {
    let raw = read_raw(json)?;
    let is = |s: &RawStream, kind: &str| s.codec_type.as_deref() == Some(kind);
    let video = raw
        .streams
        .iter()
        .find(|s| is(s, "video") && s.disposition.as_ref().is_none_or(|d| d.attached_pic == 0))
        .ok_or_else(|| MediaError::Open("no video stream".into()))?;
    let audio = raw
        .streams
        .iter()
        .find(|s| is(s, "audio"))
        .map(parse_audio)
        .transpose()?;
    let duration = raw
        .format
        .as_ref()
        .and_then(|f| seconds(f.duration.as_deref()))
        .and_then(|d| Duration::try_from_secs_f64(d).ok());
    let start_time = raw
        .format
        .as_ref()
        .and_then(|f| seconds(f.start_time.as_deref()));
    Ok(ProbeInfo {
        video: parse_video(video)?,
        audio,
        duration,
        start_time,
    })
}

/// A decimal number of seconds; `None` when absent, unreadable or not
/// finite.
fn seconds(text: Option<&str>) -> Option<f64> {
    text?.trim().parse::<f64>().ok().filter(|s| s.is_finite())
}

/// Validates and converts a video stream.
fn parse_video(s: &RawStream) -> Result<VideoStreamInfo, MediaError> {
    let (width, height) = match (s.width, s.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
        _ => return Err(MediaError::Open("video stream has no picture size".into())),
    };
    let frame_rate = [&s.avg_frame_rate, &s.r_frame_rate]
        .into_iter()
        .flatten()
        .find_map(|r| FrameRate::parse(r))
        .ok_or_else(|| MediaError::Open("video stream has no usable frame rate".into()))?;
    let rotation = s
        .side_data_list
        .iter()
        .find_map(|d| d.rotation)
        .map(|ccw| -ccw)
        .or_else(|| s.tags.as_ref()?.rotate.as_ref()?.trim().parse().ok())
        .filter(|r: &f64| r.is_finite())
        .map_or(0, |r| (r.round() as i64).rem_euclid(360) as u32);
    Ok(VideoStreamInfo {
        index: s.index,
        width,
        height,
        frame_rate,
        rotation,
        color_space: s.color_space.clone(),
        start_time: seconds(s.start_time.as_deref()),
    })
}

/// Validates and converts an audio stream.
fn parse_audio(s: &RawStream) -> Result<AudioStreamInfo, MediaError> {
    let sample_rate = s
        .sample_rate
        .as_deref()
        .and_then(|r| r.trim().parse::<u32>().ok())
        .filter(|&r| r > 0);
    match (sample_rate, s.channels) {
        (Some(sample_rate), Some(channels)) if channels > 0 => Ok(AudioStreamInfo {
            index: s.index,
            sample_rate,
            channels,
        }),
        _ => Err(MediaError::Open(
            "audio stream has no sample rate or channel count".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_off_the_path_are_found_in_fallback_directories() {
        let root = std::env::temp_dir().join(format!("ac-sources-locate-{}", std::process::id()));
        let (on_path, brew) = (root.join("bin"), root.join("brew"));
        std::fs::create_dir_all(&on_path).unwrap();
        std::fs::create_dir_all(&brew).unwrap();
        std::fs::write(brew.join("ffmpeg"), b"").unwrap();
        let brew_dir = brew.to_str().unwrap();
        let path = std::env::join_paths([&on_path]).unwrap();
        // Not on PATH: the fallback's full path.
        assert_eq!(
            locate("ffmpeg", Some(&path), &[brew_dir]),
            brew.join("ffmpeg")
        );
        // On PATH: the bare name, so PATH order still decides.
        std::fs::write(on_path.join("ffmpeg"), b"").unwrap();
        assert_eq!(
            locate("ffmpeg", Some(&path), &[brew_dir]),
            PathBuf::from("ffmpeg")
        );
        // Nowhere: the bare name, so the error names the tool.
        assert_eq!(
            locate("ffprobe", Some(&path), &[brew_dir]),
            PathBuf::from("ffprobe")
        );
        assert_eq!(locate("ffprobe", None, &[]), PathBuf::from("ffprobe"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Trimmed real ffprobe output for a 160x120 30 fps clip with stereo audio.
    const CLIP: &str = r#"{
        "streams": [
            {"index": 0, "codec_type": "video", "width": 160, "height": 120,
             "r_frame_rate": "30/1", "avg_frame_rate": "30/1",
             "disposition": {"default": 0, "attached_pic": 0},
             "tags": {"DURATION": "00:00:01.000000000"}},
            {"index": 1, "codec_type": "audio", "sample_rate": "48000",
             "channels": 2, "r_frame_rate": "0/0", "avg_frame_rate": "0/0"}
        ],
        "format": {"filename": "t.mkv", "duration": "1.000000"}
    }"#;

    #[test]
    fn parses_video_audio_and_duration() {
        let p = parse_probe(CLIP).unwrap();
        assert_eq!(
            (p.video.index, p.video.width, p.video.height),
            (0, 160, 120)
        );
        assert_eq!(p.video.frame_rate, FrameRate::new(30, 1).unwrap());
        assert_eq!(p.video.rotation, 0);
        assert_eq!(p.video.color_space, None);
        let a = p.audio.unwrap();
        assert_eq!((a.index, a.sample_rate, a.channels), (1, 48_000, 2));
        assert_eq!(p.duration, Some(Duration::from_secs(1)));
        assert_eq!((p.start_time, p.video.start_time), (None, None));
    }

    #[test]
    fn reads_where_the_container_and_its_video_start() {
        // Trimmed real ffprobe output for an MPEG-TS recording: audio from
        // 1.4 s, video from 1.421333 s.
        let json = r#"{"streams": [
            {"index": 0, "codec_type": "video", "width": 64, "height": 36,
             "avg_frame_rate": "30000/1001", "start_time": "1.421333"},
            {"index": 1, "codec_type": "audio", "sample_rate": "48000", "channels": 2,
             "start_time": "1.400000"}
        ], "format": {"format_name": "mpegts", "start_time": "1.400000", "duration": "6.0"}}"#;
        let p = parse_probe(json).unwrap();
        assert_eq!(p.start_time, Some(1.4));
        assert_eq!(p.video.start_time, Some(1.421333));
        // Unreadable or missing times are unknown, not zero.
        let json = r#"{"streams": [{"index": 0, "codec_type": "video", "width": 8,
            "height": 8, "avg_frame_rate": "25/1", "start_time": "N/A"}],
            "format": {"start_time": "nan"}}"#;
        let p = parse_probe(json).unwrap();
        assert_eq!((p.start_time, p.video.start_time), (None, None));
    }

    #[test]
    fn video_only_file_has_no_audio() {
        let json = r#"{"streams": [{"index": 0, "codec_type": "video", "width": 64,
            "height": 48, "avg_frame_rate": "25/1", "color_space": "bt709"}]}"#;
        let p = parse_probe(json).unwrap();
        assert_eq!(p.audio, None);
        assert_eq!(p.duration, None);
        assert_eq!(p.video.color_space.as_deref(), Some("bt709"));
    }

    #[test]
    fn skips_cover_art_and_takes_the_real_video_stream() {
        let json = r#"{"streams": [
            {"index": 0, "codec_type": "video", "width": 600, "height": 600,
             "avg_frame_rate": "0/0", "r_frame_rate": "90000/1",
             "disposition": {"attached_pic": 1}},
            {"index": 1, "codec_type": "audio", "sample_rate": "44100", "channels": 1},
            {"index": 2, "codec_type": "video", "width": 1920, "height": 1080,
             "avg_frame_rate": "30000/1001"}
        ]}"#;
        let p = parse_probe(json).unwrap();
        assert_eq!(p.video.index, 2);
        assert_eq!(p.video.frame_rate, FrameRate::new(30000, 1001).unwrap());
    }

    #[test]
    fn falls_back_to_r_frame_rate() {
        let json = r#"{"streams": [{"index": 0, "codec_type": "video", "width": 8,
            "height": 8, "avg_frame_rate": "0/0", "r_frame_rate": "24/1"}]}"#;
        assert_eq!(
            parse_probe(json).unwrap().video.frame_rate,
            FrameRate::new(24, 1).unwrap()
        );
    }

    #[test]
    fn reads_rotation_from_display_matrix_or_tag() {
        let matrix = r#"{"streams": [{"index": 0, "codec_type": "video", "width": 1920,
            "height": 1080, "avg_frame_rate": "30/1",
            "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}]}]}"#;
        let v = parse_probe(matrix).unwrap().video;
        assert_eq!(v.rotation, 90);
        assert_eq!(v.display_size(), (1080, 1920));
        let tag = r#"{"streams": [{"index": 0, "codec_type": "video", "width": 1920,
            "height": 1080, "avg_frame_rate": "30/1", "tags": {"rotate": "180"}}]}"#;
        let v = parse_probe(tag).unwrap().video;
        assert_eq!(v.rotation, 180);
        assert_eq!(v.display_size(), (1920, 1080));
    }

    #[test]
    fn rejects_unusable_files() {
        let cases = [
            ("not json", "unreadable"),
            (r#"{"streams": []}"#, "no video stream"),
            (
                r#"{"streams": [{"codec_type": "audio", "sample_rate": "48000", "channels": 2}]}"#,
                "no video stream",
            ),
            (
                r#"{"streams": [{"codec_type": "video", "width": 0, "height": 8, "avg_frame_rate": "30/1"}]}"#,
                "picture size",
            ),
            (
                r#"{"streams": [{"codec_type": "video", "width": 8, "height": 8, "avg_frame_rate": "0/0"}]}"#,
                "frame rate",
            ),
            (
                r#"{"streams": [{"codec_type": "video", "width": 8, "height": 8, "avg_frame_rate": "30/1"},
                    {"codec_type": "audio", "sample_rate": "0", "channels": 2}]}"#,
                "sample rate",
            ),
        ];
        for (json, needle) in cases {
            match parse_probe(json) {
                Err(MediaError::Open(msg)) => assert!(msg.contains(needle), "{msg}"),
                other => panic!("{json}: {other:?}"),
            }
        }
    }

    #[test]
    fn file_urls_protect_odd_names() {
        assert_eq!(
            file_url(Path::new("/a/-x.mp4")),
            OsString::from("file:/a/-x.mp4")
        );
    }

    #[test]
    fn missing_ffprobe_is_an_open_error() {
        let tools = FfmpegTools {
            ffmpeg: "/nonexistent/ffmpeg".into(),
            ffprobe: "/nonexistent/ffprobe".into(),
        };
        assert!(!tools.is_available());
        let err = probe(&tools, Path::new("/tmp/x.mp4")).unwrap_err();
        assert!(matches!(err, MediaError::Open(m) if m.contains("ffprobe")));
    }
}
