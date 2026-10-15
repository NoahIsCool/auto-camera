//! Output configuration, validation and the choice of H.264 encoder.
//!
//! [`OutputConfig`] describes one program encode: picture size and rate, the
//! video and audio bitrates, the keyframe interval and where the result goes.
//! [`OutputConfig::validate`] is pure and runs before anything is spawned, so
//! a bad config is reported as a named field instead of an ffmpeg exit code.
//!
//! Invariants after validation:
//!
//! * width and height are even (4:2:0 chroma needs whole 2x2 blocks);
//! * the GOP, `keyframe_seconds * fps`, is at least one frame;
//! * at least one destination is set, the RTMP URL is `rtmp://` or
//!   `rtmps://`, and every path is UTF-8 (ffmpeg arguments are strings).
//!
//! RTMP URLs carry the stream key, which is a credential. The [`fmt::Debug`]
//! impl of [`OutputConfig`] and every ffmpeg log line surfaced by this crate
//! pass through [`redact`], which replaces the key with `<redacted>`.
//!
//! Encoder choice is a pure function of the request, the platform and the
//! `ffmpeg -encoders` listing ([`choose_encoder`]), so it is unit-tested
//! without ffmpeg; `detect_encoder` in the encoder module supplies the listing.

use std::fmt;
use std::path::PathBuf;

use crate::OutputError;

/// Text substituted for secrets in logs and debug output.
pub const REDACTED: &str = "<redacted>";

/// Requested H.264 encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoder {
    /// `h264_videotoolbox` on macOS when ffmpeg lists it, otherwise `libx264`.
    #[default]
    Auto,
    /// Apple's hardware encoder, `h264_videotoolbox`.
    VideoToolbox,
    /// The x264 software encoder, `libx264`, with preset `veryfast` and tune
    /// `zerolatency`.
    Libx264,
}

/// The encoder actually used, after resolving [`Encoder::Auto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderChoice {
    /// `h264_videotoolbox`.
    VideoToolbox,
    /// `libx264`.
    Libx264,
}

impl EncoderChoice {
    /// The encoder's name in ffmpeg (`-c:v` value).
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            EncoderChoice::VideoToolbox => "h264_videotoolbox",
            EncoderChoice::Libx264 => "libx264",
        }
    }
}

/// Format of the raw audio pushed into the encoder.
///
/// Samples are interleaved `f32` in `-1.0..=1.0`, as in
/// [`ac_media::AudioBlock`]. ffmpeg resamples to 48 kHz stereo AAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Interleaved channel count.
    pub channels: u16,
}

/// One program encode.
#[derive(Clone, PartialEq)]
pub struct OutputConfig {
    /// ffmpeg executable; a bare name is looked up on the `PATH`.
    pub ffmpeg: PathBuf,
    /// Picture width in pixels; even.
    pub width: u32,
    /// Picture height in pixels; even.
    pub height: u32,
    /// Frames per second; the engine pushes exactly this many frames per
    /// second of program time.
    pub fps: u32,
    /// Target video bitrate in kbit/s, also used as the VBV maximum rate.
    pub video_bitrate_kbps: u32,
    /// Seconds between keyframes (GOP length); 2 suits RTMP ingest.
    pub keyframe_seconds: u32,
    /// Requested H.264 encoder.
    pub encoder: Encoder,
    /// RTMP or RTMPS ingest URL including the stream key.
    pub rtmp_url: Option<String>,
    /// Local recording path; never overwritten if it already exists.
    pub record_path: Option<PathBuf>,
    /// Raw audio input format, or `None` for a silent (video-only) program.
    pub audio: Option<AudioFormat>,
    /// AAC bitrate in kbit/s; ignored without audio.
    pub audio_bitrate_kbps: u32,
}

impl Default for OutputConfig {
    /// 1080p30 at 6 Mbit/s with a 2 s GOP, automatic encoder, no
    /// destinations and no audio; AAC at 160 kbit/s once audio is set.
    fn default() -> Self {
        Self {
            ffmpeg: PathBuf::from("ffmpeg"),
            width: 1920,
            height: 1080,
            fps: 30,
            video_bitrate_kbps: 6000,
            keyframe_seconds: 2,
            encoder: Encoder::Auto,
            rtmp_url: None,
            record_path: None,
            audio: None,
            audio_bitrate_kbps: 160,
        }
    }
}

impl fmt::Debug for OutputConfig {
    /// Like a derived `Debug`, but with the stream key redacted.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutputConfig")
            .field("ffmpeg", &self.ffmpeg)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("fps", &self.fps)
            .field("video_bitrate_kbps", &self.video_bitrate_kbps)
            .field("keyframe_seconds", &self.keyframe_seconds)
            .field("encoder", &self.encoder)
            .field("rtmp_url", &self.rtmp_url.as_deref().map(redact_url))
            .field("record_path", &self.record_path)
            .field("audio", &self.audio)
            .field("audio_bitrate_kbps", &self.audio_bitrate_kbps)
            .finish()
    }
}

/// Builds an [`OutputError::InvalidConfig`].
fn invalid(msg: impl Into<String>) -> OutputError {
    OutputError::InvalidConfig(msg.into())
}

/// Checks that `value` lies in `range`, naming `field` in the error.
fn check_range(
    field: &str,
    value: u32,
    range: std::ops::RangeInclusive<u32>,
) -> Result<(), OutputError> {
    if range.contains(&value) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{field} = {value} is outside {}..={}",
            range.start(),
            range.end()
        )))
    }
}

impl OutputConfig {
    /// Checks every field; see the module docs for the invariants.
    ///
    /// Pure: it does not touch the filesystem. `ProgramEncoder::start`
    /// additionally refuses a recording path whose parent directory is
    /// missing or which already exists.
    pub fn validate(&self) -> Result<(), OutputError> {
        if self.ffmpeg.as_os_str().is_empty() {
            return Err(invalid("ffmpeg path is empty"));
        }
        check_range("width", self.width, 16..=7680)?;
        check_range("height", self.height, 16..=4320)?;
        if !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) {
            return Err(invalid(format!(
                "{}x{} is odd; yuv420p needs even dimensions",
                self.width, self.height
            )));
        }
        check_range("fps", self.fps, 1..=120)?;
        check_range("video_bitrate_kbps", self.video_bitrate_kbps, 100..=100_000)?;
        check_range("keyframe_seconds", self.keyframe_seconds, 1..=10)?;
        if self.rtmp_url.is_none() && self.record_path.is_none() {
            return Err(invalid("no destination: set rtmp_url, record_path or both"));
        }
        if let Some(url) = &self.rtmp_url {
            validate_rtmp_url(url)?;
        }
        if let Some(path) = &self.record_path {
            match path.to_str() {
                None => return Err(invalid("record_path is not valid UTF-8")),
                Some("") => return Err(invalid("record_path is empty")),
                Some(_) => {}
            }
        }
        if let Some(audio) = self.audio {
            check_range("audio.sample_rate", audio.sample_rate, 8_000..=192_000)?;
            check_range("audio.channels", u32::from(audio.channels), 1..=8)?;
            check_range("audio_bitrate_kbps", self.audio_bitrate_kbps, 32..=512)?;
        }
        Ok(())
    }

    /// Frames between keyframes.
    pub fn gop_frames(&self) -> u32 {
        self.keyframe_seconds.saturating_mul(self.fps).max(1)
    }

    /// Bytes in one RGBA frame.
    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// Accepts `rtmp://` and `rtmps://` URLs with a host and no whitespace.
fn validate_rtmp_url(url: &str) -> Result<(), OutputError> {
    let lower = url.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix("rtmp://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("rtmps://") {
        rest
    } else {
        return Err(invalid("rtmp_url must start with rtmp:// or rtmps://"));
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err(invalid("rtmp_url has no host"));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(
            "rtmp_url contains whitespace or control characters",
        ));
    }
    Ok(())
}

/// An RTMP URL split around its secrets: `scheme://[user@]host/dir/key`.
struct UrlParts<'a> {
    /// `rtmp` or `rtmps`.
    scheme: &'a str,
    /// User info before `@`, a secret.
    user: Option<&'a str>,
    /// Host and optional port.
    host: &'a str,
    /// Path up to (excluding) the final `/`, such as `/live2`.
    dir: &'a str,
    /// Last path segment with any query: the stream key, a secret. Present
    /// only when the path has at least two segments.
    key: Option<&'a str>,
}

impl<'a> UrlParts<'a> {
    /// Splits `url`; `None` without a `://`.
    fn parse(url: &'a str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let (user, host) = match authority.rsplit_once('@') {
            Some((user, host)) if !user.is_empty() => (Some(user), host),
            _ => (None, authority),
        };
        let (dir, key) = match path.rsplit_once('/') {
            Some((dir, key)) if !dir.is_empty() && !key.is_empty() => (dir, Some(key)),
            _ => (path, None),
        };
        Some(Self {
            scheme,
            user,
            host,
            dir,
            key,
        })
    }
}

/// `url` with its stream key and user info replaced by [`REDACTED`].
pub fn redact_url(url: &str) -> String {
    match UrlParts::parse(url) {
        Some(p) => {
            let user = p.user.map_or(String::new(), |_| format!("{REDACTED}@"));
            let key = p.key.map_or(String::new(), |_| format!("/{REDACTED}"));
            format!("{}://{user}{}{}{key}", p.scheme, p.host, p.dir)
        }
        None => url.to_owned(),
    }
}

/// `text` with every secret of `url` (see [`redact_url`]) replaced.
///
/// The whole URL is replaced first, so short keys are redacted where they
/// appear inside it; elsewhere only keys of four or more characters are
/// replaced, which avoids mangling ordinary words with a one-letter "key".
pub fn redact(text: &str, url: Option<&str>) -> String {
    let Some((url, parts)) = url.and_then(|u| Some((u, UrlParts::parse(u)?))) else {
        return text.to_owned();
    };
    let mut out = text.replace(url, &redact_url(url));
    for secret in [parts.user, parts.key].into_iter().flatten() {
        if secret.len() >= 4 {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

/// True when the `ffmpeg -encoders` listing contains a video encoder `name`.
///
/// Listing lines look like ` V....D libx264   libx264 H.264 / AVC ...`: a
/// capability field whose first letter is the media type, then the name.
/// Legend lines (` V..... = Video`) have `=` where the name would be.
pub fn encoder_listed(listing: &str, name: &str) -> bool {
    name != "="
        && listing.lines().any(|line| {
            let mut fields = line.split_whitespace();
            matches!(
                (fields.next(), fields.next()),
                (Some(caps), Some(n)) if caps.starts_with('V') && caps.len() == 6 && n == name
            )
        })
}

/// Resolves `requested` against an `ffmpeg -encoders` listing.
///
/// [`Encoder::Auto`] prefers VideoToolbox only when `macos` is true and the
/// listing has it, otherwise libx264. An explicit request must be listed.
pub fn choose_encoder(
    requested: Encoder,
    listing: &str,
    macos: bool,
) -> Result<EncoderChoice, OutputError> {
    let has = |c: EncoderChoice| encoder_listed(listing, c.ffmpeg_name());
    let want = match requested {
        Encoder::VideoToolbox => EncoderChoice::VideoToolbox,
        Encoder::Libx264 => EncoderChoice::Libx264,
        Encoder::Auto if macos && has(EncoderChoice::VideoToolbox) => {
            return Ok(EncoderChoice::VideoToolbox);
        }
        Encoder::Auto => EncoderChoice::Libx264,
    };
    if has(want) {
        Ok(want)
    } else {
        Err(OutputError::EncoderUnavailable(want.ffmpeg_name().into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid config recording to a file.
    fn config() -> OutputConfig {
        OutputConfig {
            record_path: Some("/tmp/program.mp4".into()),
            ..OutputConfig::default()
        }
    }

    /// Asserts that `c` fails validation with a message containing `needle`.
    fn rejects(c: OutputConfig, needle: &str) {
        match c.validate() {
            Err(OutputError::InvalidConfig(msg)) => {
                assert!(msg.contains(needle), "{msg:?} lacks {needle:?}")
            }
            other => panic!("expected InvalidConfig({needle}), got {other:?}"),
        }
    }

    const LISTING: &str = "Encoders:\n V..... = Video\n ------\n \
        V....D libx264              libx264 H.264 / AVC\n \
        V....D h264_videotoolbox    VideoToolbox H.264 Encoder\n \
        A....D aac                  AAC (Advanced Audio Coding)\n";

    #[test]
    fn default_with_destination_is_valid() {
        config().validate().unwrap();
        let c = OutputConfig {
            rtmp_url: Some("rtmps://live.example.com:443/app/key".into()),
            record_path: None,
            audio: Some(AudioFormat {
                sample_rate: 44_100,
                channels: 1,
            }),
            ..OutputConfig::default()
        };
        c.validate().unwrap();
    }

    #[test]
    fn rejects_bad_fields() {
        rejects(
            OutputConfig {
                width: 1921,
                ..config()
            },
            "odd",
        );
        rejects(
            OutputConfig {
                height: 0,
                ..config()
            },
            "height",
        );
        rejects(OutputConfig { fps: 0, ..config() }, "fps");
        rejects(
            OutputConfig {
                video_bitrate_kbps: 10,
                ..config()
            },
            "video_bitrate_kbps",
        );
        rejects(
            OutputConfig {
                keyframe_seconds: 0,
                ..config()
            },
            "keyframe_seconds",
        );
        rejects(
            OutputConfig {
                record_path: None,
                ..config()
            },
            "no destination",
        );
        rejects(
            OutputConfig {
                record_path: Some(PathBuf::new()),
                ..config()
            },
            "empty",
        );
        rejects(
            OutputConfig {
                ffmpeg: PathBuf::new(),
                ..config()
            },
            "ffmpeg",
        );
    }

    #[test]
    fn rejects_bad_audio() {
        let with = |sample_rate, channels, kbps| OutputConfig {
            audio: Some(AudioFormat {
                sample_rate,
                channels,
            }),
            audio_bitrate_kbps: kbps,
            ..config()
        };
        with(48_000, 2, 160).validate().unwrap();
        rejects(with(48_000, 0, 160), "channels");
        rejects(with(1_000, 2, 160), "sample_rate");
        rejects(with(48_000, 2, 1), "audio_bitrate_kbps");
        // The audio bitrate is irrelevant without audio.
        OutputConfig {
            audio_bitrate_kbps: 0,
            ..config()
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn rejects_bad_rtmp_urls() {
        for (url, needle) in [
            ("http://example.com/live/key", "rtmp://"),
            ("rtmp://", "no host"),
            ("rtmp:///live/key", "no host"),
            ("rtmp://host/live/key with space", "whitespace"),
        ] {
            rejects(
                OutputConfig {
                    rtmp_url: Some(url.into()),
                    ..config()
                },
                needle,
            );
        }
        OutputConfig {
            rtmp_url: Some("RTMP://Host/live/key".into()),
            ..config()
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn gop_and_frame_size() {
        let c = OutputConfig {
            fps: 60,
            keyframe_seconds: 2,
            width: 4,
            height: 2,
            ..config()
        };
        assert_eq!(c.gop_frames(), 120);
        assert_eq!(c.frame_bytes(), 32);
    }

    #[test]
    fn redacts_stream_keys() {
        let url = "rtmp://a.rtmp.youtube.com/live2/abcd-efgh-ijkl";
        assert_eq!(
            redact_url(url),
            "rtmp://a.rtmp.youtube.com/live2/<redacted>"
        );
        let line = format!("[fifo @ 0x1] Error opening {url}: refused; key abcd-efgh-ijkl");
        let safe = redact(&line, Some(url));
        assert!(!safe.contains("abcd-efgh-ijkl"), "{safe}");
        assert!(safe.contains("a.rtmp.youtube.com/live2/<redacted>"));
        // Query strings are part of the key; user info is a secret too.
        let fb = "rtmps://user:pw@live.example.com:443/rtmp/FB-123?s_bl=1&a=b";
        assert_eq!(
            redact_url(fb),
            "rtmps://<redacted>@live.example.com:443/rtmp/<redacted>"
        );
    }

    #[test]
    fn redaction_edge_cases() {
        // No key segment: nothing to hide.
        assert_eq!(redact_url("rtmp://host/app"), "rtmp://host/app");
        assert_eq!(redact("text", None), "text");
        // A one-letter key is hidden inside the URL but not elsewhere.
        let url = "rtmp://host/app/k";
        assert_eq!(
            redact("open rtmp://host/app/k: kaput", Some(url)),
            "open rtmp://host/app/<redacted>: kaput"
        );
        let c = OutputConfig {
            rtmp_url: Some("rtmp://host/app/secret-key".into()),
            ..config()
        };
        let debug = format!("{c:?}");
        assert!(!debug.contains("secret-key"), "{debug}");
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn parses_encoder_listing() {
        assert!(encoder_listed(LISTING, "libx264"));
        assert!(encoder_listed(LISTING, "h264_videotoolbox"));
        // Audio encoders and the legend do not count as video encoders.
        assert!(!encoder_listed(LISTING, "aac"));
        assert!(!encoder_listed(LISTING, "="));
        assert!(!encoder_listed("", "libx264"));
        assert!(!encoder_listed(LISTING, "libx26"));
    }

    #[test]
    fn auto_prefers_videotoolbox_on_macos_only() {
        let c = |req, listing, mac| choose_encoder(req, listing, mac).unwrap();
        assert_eq!(c(Encoder::Auto, LISTING, true), EncoderChoice::VideoToolbox);
        assert_eq!(c(Encoder::Auto, LISTING, false), EncoderChoice::Libx264);
        let x264_only = " V....D libx264 libx264 H.264\n";
        assert_eq!(c(Encoder::Auto, x264_only, true), EncoderChoice::Libx264);
        assert_eq!(c(Encoder::Libx264, LISTING, true), EncoderChoice::Libx264);
        assert_eq!(
            c(Encoder::VideoToolbox, LISTING, false),
            EncoderChoice::VideoToolbox
        );
    }

    #[test]
    fn missing_encoders_are_errors() {
        let x264_only = " V....D libx264 libx264 H.264\n";
        assert!(matches!(
            choose_encoder(Encoder::VideoToolbox, x264_only, true),
            Err(OutputError::EncoderUnavailable(name)) if name == "h264_videotoolbox"
        ));
        assert!(matches!(
            choose_encoder(Encoder::Auto, "", true),
            Err(OutputError::EncoderUnavailable(name)) if name == "libx264"
        ));
    }
}
