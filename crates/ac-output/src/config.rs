//! Output configuration, validation and the choice of H.264 encoder.
//!
//! [`OutputConfig`] describes one program encode: picture size and rate, the
//! video and audio bitrates, the keyframe interval and where the result goes.
//! [`OutputConfig::validate`] is pure and runs before anything is spawned, so
//! a bad config is reported as a named field instead of an ffmpeg exit code;
//! [`OutputConfig::problems`] lists every bad field at once, for a check
//! that should not stop at the first.
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
//! without ffmpeg; `detect_encoder` in the encoder module supplies the
//! listing. A listed VideoToolbox may still be unable to open a hardware
//! session (virtual machines), so an automatic choice of it is confirmed by
//! a one-frame test encode and falls back to libx264
//! ([`choose_working_encoder`]).

use std::fmt;
use std::path::PathBuf;

use crate::OutputError;

/// Text substituted for secrets in logs and debug output.
pub const REDACTED: &str = "<redacted>";

/// Requested H.264 encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoder {
    /// `h264_videotoolbox` on macOS when ffmpeg lists it and a test encode
    /// succeeds, otherwise `libx264`.
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

/// The problem with `value` when it lies outside `range`, naming `field`.
fn range_problem(field: &str, value: u32, range: std::ops::RangeInclusive<u32>) -> Option<String> {
    (!range.contains(&value)).then(|| {
        format!(
            "{field} = {value} is outside {}..={}",
            range.start(),
            range.end()
        )
    })
}

impl OutputConfig {
    /// Checks every field, failing on the first of [`Self::problems`]; see
    /// the module docs for the invariants.
    ///
    /// Pure: it does not touch the filesystem. `ProgramEncoder::start`
    /// additionally refuses a recording path whose parent directory is
    /// missing or which already exists.
    pub fn validate(&self) -> Result<(), OutputError> {
        match self.problems().into_iter().next() {
            Some(problem) => Err(OutputError::InvalidConfig(problem)),
            None => Ok(()),
        }
    }

    /// Every field's problem, each naming the field, in the order
    /// [`Self::validate`] checks them; empty when the config is valid. A
    /// side outside its range is named by its range alone, and oddness is a
    /// problem of the sides within range. Pure, like `validate`.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.ffmpeg.as_os_str().is_empty() {
            out.push("ffmpeg path is empty".into());
        }
        let width = range_problem("width", self.width, 16..=7680);
        let height = range_problem("height", self.height, 16..=4320);
        let odd = (width.is_none() && !self.width.is_multiple_of(2))
            || (height.is_none() && !self.height.is_multiple_of(2));
        out.extend(width);
        out.extend(height);
        if odd {
            out.push(format!(
                "{}x{} is odd; yuv420p needs even dimensions",
                self.width, self.height
            ));
        }
        out.extend(range_problem("fps", self.fps, 1..=120));
        out.extend(range_problem(
            "video_bitrate_kbps",
            self.video_bitrate_kbps,
            100..=100_000,
        ));
        out.extend(range_problem(
            "keyframe_seconds",
            self.keyframe_seconds,
            1..=10,
        ));
        if self.rtmp_url.is_none() && self.record_path.is_none() {
            out.push("no destination: set rtmp_url, record_path or both".into());
        }
        if let Some(url) = &self.rtmp_url {
            out.extend(validate_rtmp_url(url).err());
        }
        match self.record_path.as_deref().map(|p| p.to_str()) {
            Some(None) => out.push("record_path is not valid UTF-8".into()),
            Some(Some("")) => out.push("record_path is empty".into()),
            _ => {}
        }
        if let Some(audio) = self.audio {
            out.extend(range_problem(
                "audio.sample_rate",
                audio.sample_rate,
                8_000..=192_000,
            ));
            out.extend(range_problem(
                "audio.channels",
                u32::from(audio.channels),
                1..=8,
            ));
            out.extend(range_problem(
                "audio_bitrate_kbps",
                self.audio_bitrate_kbps,
                32..=512,
            ));
        }
        out
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

/// Accepts `rtmp://` and `rtmps://` URLs with a host and no whitespace;
/// the error names `rtmp_url` and never contains the URL, whose stream key
/// is a secret.
pub fn validate_rtmp_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix("rtmp://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("rtmps://") {
        rest
    } else {
        return Err("rtmp_url must start with rtmp:// or rtmps://".into());
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err("rtmp_url has no host".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("rtmp_url contains whitespace or control characters".into());
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

/// Resolves `requested` like [`choose_encoder`], then makes sure an
/// automatic VideoToolbox choice can actually encode.
///
/// ffmpeg lists `h264_videotoolbox` whenever it was built with it (every
/// Homebrew build), but without `-allow_sw` it needs a hardware encoder
/// session, which a virtual machine or a busy media engine cannot give; the
/// encode would then fail right after start and the operator could not go
/// live. So when [`Encoder::Auto`] resolves to VideoToolbox, `vt_works` is
/// asked (the engine runs a one-frame test encode) and libx264 is used if
/// it says no. An explicit request is never second-guessed.
pub fn choose_working_encoder(
    requested: Encoder,
    listing: &str,
    macos: bool,
    vt_works: impl FnOnce() -> bool,
) -> Result<EncoderChoice, OutputError> {
    let choice = choose_encoder(requested, listing, macos)?;
    if requested == Encoder::Auto && choice == EncoderChoice::VideoToolbox && !vt_works() {
        return choose_encoder(Encoder::Libx264, listing, macos);
    }
    Ok(choice)
}

#[cfg(test)]
mod tests;
