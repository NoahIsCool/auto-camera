//! The ffmpeg command line for one program encode.
//!
//! [`build_ffmpeg_args`] is a pure function of the config, the resolved
//! encoder, the audio input URL and whether ffmpeg can write mux
//! statistics, so the whole contract with ffmpeg is unit-tested here
//! without running it. The command has four parts:
//!
//! 1. **Reporting.** `-progress pipe:1` writes `key=value` blocks to stdout
//!    every half second (parsed by the progress module); `-nostdin` because
//!    stdin carries video. `-loglevel repeat+level+info` tags every stderr
//!    line with its severity and keeps repeated lines: the stderr reader
//!    keeps the errors and gives every line to the stream monitor, whose
//!    reconnection messages are info lines (see the logline and stream
//!    modules). When the encode streams and ffmpeg can, `-stats_mux_pre`
//!    adds a line per muxed packet to stdout, from which the stream's
//!    bitrate is measured (see the progress module).
//! 2. **Inputs.** Input 0 is `rawvideo` RGBA on `pipe:0` with a fixed
//!    `-framerate`, so the frame count is the clock. Input 1, when audio is
//!    configured, is `f32le` from the audio URL. Both use the minimum probe
//!    (`-probesize 32 -analyzeduration 0`): the formats are fully specified,
//!    and the default 5 s analysis would hold ffmpeg on the audio input while
//!    the video pipe backs up. Small thread queues push back-pressure into
//!    this crate's queues, where drops are counted, instead of buffering
//!    seconds of frames inside ffmpeg.
//! 3. **Encode.** `scale` converts full-range RGB to limited-range BT.709
//!    `yuv420p` (swscale defaults to the BT.601 matrix); the stream is tagged
//!    BT.709 both in the codec parameters and, via `h264_metadata`, in the SPS
//!    VUI (VideoToolbox can omit it). No B-frames, a fixed GOP of
//!    `keyframe_seconds * fps` and VBV-capped bitrate suit live ingest. Audio
//!    is AAC at 48 kHz stereo. `+global_header` is set explicitly because the
//!    tee muxer cannot negotiate it for its FLV and MP4 slaves.
//! 4. **Outputs.** One `tee` output so a single encode feeds every
//!    destination. The MP4 slave comes first and is fragmented
//!    (`+frag_keyframe+empty_moov+default_base_moof`): the file is playable
//!    up to the last keyframe even if the process dies, and the tee closes
//!    slaves in order, so the recording is finalised before a slow RTMP slave
//!    is shut down. The RTMP slave is FLV wrapped in the `fifo` muxer with
//!    `onfail=ignore`: the fifo runs the network I/O on its own thread with a
//!    bounded queue that drops packets on overflow and reconnects with
//!    `restart_with_keyframe`, so an unreachable, slow or dropped ingest never
//!    blocks or ends the recording. Its queue length is set explicitly
//!    ([`FIFO_QUEUE_PACKETS`], ffmpeg's default) because the stream monitor
//!    infers a working connection from the queue not overflowing.
//!
//! Tee slave strings are escaped for ffmpeg's two parsing levels: each slave
//! is wrapped in single quotes for the `|`-separated list, and the `:` inside
//! `fifo_options` is backslash-escaped for the slave option parser. Recording
//! paths get a `file:` prefix so a `:` in a file name is never taken for a
//! protocol.

use crate::config::{EncoderChoice, OutputConfig};
use crate::progress::MUX_STATS_FORMAT;

/// Seconds between reconnection attempts to a failed RTMP ingest.
pub const RTMP_RECOVERY_WAIT_SECONDS: u32 = 2;

/// Packets the fifo muxer queues for the RTMP slave before it drops them.
pub const FIFO_QUEUE_PACKETS: u32 = 60;

/// ffmpeg's log level and flags: severity tags, no folding of repeated
/// lines, and the info-level reconnection messages (see the module docs).
const LOG_LEVEL: &str = "repeat+level+info";

/// Video packets buffered by ffmpeg's input thread.
const VIDEO_THREAD_QUEUE: &str = "4";

/// Audio packets (about 21 ms each) buffered by ffmpeg's input thread.
const AUDIO_THREAD_QUEUE: &str = "128";

/// Filter converting full-range RGB to limited-range BT.709 4:2:0
/// (swscale would otherwise use the BT.601 matrix).
const RGB_TO_BT709: &str = "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p";

/// Seconds between `-progress` reports.
const STATS_PERIOD: &str = "0.5";

/// Bitstream filter stamping BT.709 limited range into the SPS VUI.
const H264_VUI_BT709: &str = "h264_metadata=colour_primaries=1:transfer_characteristics=1:\
                              matrix_coefficients=1:video_full_range_flag=0";

/// Wraps `token` in single quotes for ffmpeg's `av_get_token`, the first
/// parsing level of a tee slave list. Inside quotes every character is
/// literal; an embedded quote closes, escapes and reopens (`'\''`).
pub fn quote_tee_token(token: &str) -> String {
    format!("'{}'", token.replace('\'', r"'\''"))
}

/// The MP4 recording slave: options plus `file:` target.
fn mp4_slave(path: &str) -> String {
    format!("[f=mp4:movflags=+frag_keyframe+empty_moov+default_base_moof]file:{path}")
}

/// The RTMP slave: FLV behind a dropping, self-recovering fifo.
fn rtmp_slave(url: &str) -> String {
    // `\:` survives the quoting level and is unescaped by the option parser,
    // leaving `:`-separated options for the fifo muxer.
    let fifo = [
        "drop_pkts_on_overflow=1".to_owned(),
        "attempt_recovery=1".to_owned(),
        "recover_any_error=1".to_owned(),
        format!("recovery_wait_time={RTMP_RECOVERY_WAIT_SECONDS}"),
        "restart_with_keyframe=1".to_owned(),
        format!("queue_size={FIFO_QUEUE_PACKETS}"),
    ]
    .join(r"\:");
    format!("[f=flv:onfail=ignore:use_fifo=1:fifo_options={fifo}]{url}")
}

/// The index of the RTMP slave in the tee ([`tee_outputs`] puts the MP4
/// slave first when there is one), which ffmpeg names when it abandons it.
pub fn rtmp_slave_index(config: &OutputConfig) -> usize {
    usize::from(config.record_path.is_some())
}

/// The tee output string: MP4 first, then RTMP, each quoted.
///
/// Empty when the config has no destination (which validation rejects).
pub fn tee_outputs(config: &OutputConfig) -> String {
    let mut slaves = Vec::new();
    if let Some(path) = &config.record_path {
        slaves.push(quote_tee_token(&mp4_slave(&path.to_string_lossy())));
    }
    if let Some(url) = &config.rtmp_url {
        slaves.push(quote_tee_token(&rtmp_slave(url)));
    }
    slaves.join("|")
}

/// The full ffmpeg argument list (without the program name).
///
/// Audio is encoded only when both `config.audio` and `audio_input` (an
/// ffmpeg input URL such as `tcp://127.0.0.1:5000`) are present; otherwise
/// the program is video-only (`-an`). Mux statistics are written when
/// `mux_stats` (this ffmpeg supports them) and the config streams. The
/// config should have passed [`OutputConfig::validate`]; this function
/// never panics either way.
pub fn build_ffmpeg_args(
    config: &OutputConfig,
    encoder: &EncoderChoice,
    audio_input: Option<&str>,
    mux_stats: bool,
) -> Vec<String> {
    let audio = config.audio.zip(audio_input);
    let gop = config.gop_frames().to_string();
    let vkbps = config.video_bitrate_kbps;
    let (rate, buffer) = (format!("{vkbps}k"), format!("{}k", vkbps.saturating_mul(2)));
    let mut a = ArgList::default();

    a.put(&["-hide_banner", "-nostdin", "-nostats"]);
    a.put(&["-loglevel", LOG_LEVEL]);
    a.put(&["-progress", "pipe:1", "-stats_period", STATS_PERIOD]);

    // Input 0: raw RGBA video at a constant frame rate.
    a.put(&["-thread_queue_size", VIDEO_THREAD_QUEUE]);
    a.put(&["-probesize", "32", "-analyzeduration", "0"]);
    a.put(&["-f", "rawvideo", "-pixel_format", "rgba"]);
    let size = format!("{}x{}", config.width, config.height);
    a.put(&["-video_size", &size, "-framerate", &config.fps.to_string()]);
    a.put(&["-i", "pipe:0"]);

    // Input 1: interleaved f32 audio.
    if let Some((format, url)) = audio {
        a.put(&["-thread_queue_size", AUDIO_THREAD_QUEUE]);
        a.put(&["-probesize", "32", "-analyzeduration", "0"]);
        a.put(&["-f", "f32le", "-ar", &format.sample_rate.to_string()]);
        a.put(&["-ac", &format.channels.to_string(), "-i", url]);
    }

    a.put(&["-map", "0:v:0"]);
    if audio.is_some() {
        a.put(&["-map", "1:a:0"]);
    }

    // Video: BT.709 limited range 4:2:0, tagged in codec and bitstream.
    a.put(&["-vf", RGB_TO_BT709]);
    a.put(&["-c:v", encoder.ffmpeg_name()]);
    match encoder {
        EncoderChoice::Libx264 => {
            a.put(&["-preset", "veryfast", "-tune", "zerolatency"]);
            // Fixed GOP: no extra keyframes on scene cuts.
            a.put(&["-keyint_min", &gop, "-sc_threshold", "0"]);
        }
        EncoderChoice::VideoToolbox => a.put(&["-realtime", "1"]),
    }
    a.put(&["-profile:v", "high", "-pix_fmt", "yuv420p"]);
    a.put(&["-b:v", &rate, "-maxrate", &rate, "-bufsize", &buffer]);
    a.put(&["-g", &gop, "-bf", "0"]);
    a.put(&["-colorspace", "bt709", "-color_primaries", "bt709"]);
    a.put(&["-color_trc", "bt709", "-color_range", "tv"]);
    a.put(&["-bsf:v", H264_VUI_BT709]);

    // Audio: AAC 48 kHz stereo, resampled and remixed by ffmpeg.
    if audio.is_some() {
        let akbps = format!("{}k", config.audio_bitrate_kbps);
        a.put(&["-c:a", "aac", "-b:a", &akbps, "-ar", "48000", "-ac", "2"]);
    } else {
        a.put(&["-an"]);
    }

    a.put(&["-flags", "+global_header", "-max_muxing_queue_size", "1024"]);
    if mux_stats && config.rtmp_url.is_some() {
        a.put(&["-stats_mux_pre", "pipe:1"]);
        a.put(&["-stats_mux_pre_fmt", MUX_STATS_FORMAT]);
    }
    a.put(&["-f", "tee", &tee_outputs(config)]);
    a.0
}

/// An argument list under construction.
#[derive(Default)]
struct ArgList(Vec<String>);

impl ArgList {
    /// Appends `items`.
    fn put(&mut self, items: &[&str]) {
        self.0.extend(items.iter().map(|s| (*s).to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AudioFormat;

    /// A 720p60 config with both destinations and stereo audio.
    fn config() -> OutputConfig {
        OutputConfig {
            width: 1280,
            height: 720,
            fps: 60,
            video_bitrate_kbps: 4500,
            keyframe_seconds: 2,
            rtmp_url: Some("rtmp://live.example.com/app/key".into()),
            record_path: Some("/rec/program.mp4".into()),
            audio: Some(AudioFormat {
                sample_rate: 44_100,
                channels: 2,
            }),
            audio_bitrate_kbps: 192,
            ..OutputConfig::default()
        }
    }

    /// The value following `flag`, asserting the flag occurs exactly once.
    fn value<'a>(args: &'a [String], flag: &str) -> &'a str {
        let hits: Vec<usize> = (0..args.len()).filter(|&i| args[i] == flag).collect();
        assert_eq!(hits.len(), 1, "{flag} occurs {} times", hits.len());
        &args[hits[0] + 1]
    }

    /// Every value following `flag`, in order.
    fn values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn video_input_is_constant_rate_raw_rgba_on_stdin() {
        let args = build_ffmpeg_args(&config(), &EncoderChoice::Libx264, None, false);
        assert_eq!(value(&args, "-pixel_format"), "rgba");
        assert_eq!(value(&args, "-video_size"), "1280x720");
        assert_eq!(value(&args, "-framerate"), "60");
        assert_eq!(values(&args, "-i"), ["pipe:0"]);
        assert_eq!(values(&args, "-f")[0], "rawvideo");
        assert_eq!(value(&args, "-progress"), "pipe:1");
        assert!(args.contains(&"-nostdin".to_owned()));
    }

    #[test]
    fn x264_is_low_latency_with_fixed_gop_and_no_b_frames() {
        let args = build_ffmpeg_args(&config(), &EncoderChoice::Libx264, None, false);
        assert_eq!(value(&args, "-c:v"), "libx264");
        assert_eq!(value(&args, "-preset"), "veryfast");
        assert_eq!(value(&args, "-tune"), "zerolatency");
        assert_eq!(value(&args, "-g"), "120");
        assert_eq!(value(&args, "-keyint_min"), "120");
        assert_eq!(value(&args, "-sc_threshold"), "0");
        assert_eq!(value(&args, "-bf"), "0");
        assert_eq!(value(&args, "-b:v"), "4500k");
        assert_eq!(value(&args, "-maxrate"), "4500k");
        assert_eq!(value(&args, "-bufsize"), "9000k");
        assert!(!args.contains(&"-realtime".to_owned()));
    }

    #[test]
    fn videotoolbox_is_realtime_without_x264_options() {
        let args = build_ffmpeg_args(&config(), &EncoderChoice::VideoToolbox, None, false);
        assert_eq!(value(&args, "-c:v"), "h264_videotoolbox");
        assert_eq!(value(&args, "-realtime"), "1");
        assert_eq!(value(&args, "-bf"), "0");
        for x264_only in ["-preset", "-tune", "-sc_threshold", "-keyint_min"] {
            assert!(!args.contains(&x264_only.to_owned()), "{x264_only}");
        }
    }

    #[test]
    fn picture_is_tagged_bt709_limited_range() {
        let args = build_ffmpeg_args(&config(), &EncoderChoice::VideoToolbox, None, false);
        let vf = value(&args, "-vf");
        assert!(vf.contains("out_color_matrix=bt709"), "{vf}");
        assert!(vf.contains("out_range=tv"), "{vf}");
        assert!(vf.ends_with("format=yuv420p"), "{vf}");
        assert_eq!(value(&args, "-pix_fmt"), "yuv420p");
        for flag in ["-colorspace", "-color_primaries", "-color_trc"] {
            assert_eq!(value(&args, flag), "bt709");
        }
        assert_eq!(value(&args, "-color_range"), "tv");
        assert!(value(&args, "-bsf:v").starts_with("h264_metadata=colour_primaries=1:"));
        assert_eq!(value(&args, "-flags"), "+global_header");
    }

    #[test]
    fn audio_input_and_encode_when_configured() {
        let url = "tcp://127.0.0.1:5000";
        let args = build_ffmpeg_args(&config(), &EncoderChoice::Libx264, Some(url), false);
        assert_eq!(values(&args, "-i"), ["pipe:0", url]);
        assert_eq!(values(&args, "-f")[..2], ["rawvideo", "f32le"]);
        assert_eq!(values(&args, "-ar"), ["44100", "48000"]);
        assert_eq!(values(&args, "-ac"), ["2", "2"]);
        assert_eq!(values(&args, "-map"), ["0:v:0", "1:a:0"]);
        assert_eq!(value(&args, "-c:a"), "aac");
        assert_eq!(value(&args, "-b:a"), "192k");
        assert!(!args.contains(&"-an".to_owned()));
        // Both inputs skip probing so neither holds the other up.
        assert_eq!(values(&args, "-probesize"), ["32", "32"]);
        assert_eq!(values(&args, "-analyzeduration"), ["0", "0"]);
    }

    #[test]
    fn no_audio_without_format_or_input() {
        let url = Some("tcp://127.0.0.1:5000");
        let silent = OutputConfig {
            audio: None,
            ..config()
        };
        for (c, input) in [(&silent, url), (&config(), None)] {
            let args = build_ffmpeg_args(c, &EncoderChoice::Libx264, input, false);
            assert_eq!(values(&args, "-i"), ["pipe:0"]);
            assert_eq!(values(&args, "-map"), ["0:v:0"]);
            assert!(args.contains(&"-an".to_owned()));
            assert!(!args.contains(&"-c:a".to_owned()));
        }
    }

    #[test]
    fn tee_records_first_and_isolates_rtmp() {
        let args = build_ffmpeg_args(&config(), &EncoderChoice::Libx264, None, false);
        assert_eq!(values(&args, "-f").last(), Some(&"tee"));
        let tee = args.last().unwrap();
        let (mp4, rtmp) = tee.split_once('|').unwrap();
        assert_eq!(
            mp4,
            "'[f=mp4:movflags=+frag_keyframe+empty_moov+default_base_moof]\
             file:/rec/program.mp4'"
        );
        assert!(rtmp.starts_with("'[f=flv:onfail=ignore:use_fifo=1:fifo_options="));
        assert!(rtmp.contains(r"drop_pkts_on_overflow=1\:attempt_recovery=1\:"));
        assert!(rtmp.contains(r"\:restart_with_keyframe=1\:queue_size=60]"));
        assert!(rtmp.ends_with("]rtmp://live.example.com/app/key'"));
    }

    #[test]
    fn stderr_is_tagged_and_unfolded() {
        let args = build_ffmpeg_args(&config(), &EncoderChoice::Libx264, None, false);
        assert_eq!(value(&args, "-loglevel"), "repeat+level+info");
        assert!(args.contains(&"-nostats".to_owned()));
    }

    #[test]
    fn mux_statistics_only_when_supported_and_streaming() {
        let args = build_ffmpeg_args(&config(), &EncoderChoice::Libx264, None, true);
        assert_eq!(value(&args, "-stats_mux_pre"), "pipe:1");
        assert_eq!(value(&args, "-stats_mux_pre_fmt"), "mux {sidx} {t} {size}");
        // An output option: before the tee output, which stays last.
        let at = args.iter().position(|a| a == "-stats_mux_pre").unwrap();
        assert!(at < args.iter().position(|a| a == "tee").unwrap());
        assert!(args.last().unwrap().contains("rtmp://"));
        let unsupported = build_ffmpeg_args(&config(), &EncoderChoice::Libx264, None, false);
        let record_only = OutputConfig {
            rtmp_url: None,
            ..config()
        };
        let not_streaming = build_ffmpeg_args(&record_only, &EncoderChoice::Libx264, None, true);
        for args in [unsupported, not_streaming] {
            assert!(!args.contains(&"-stats_mux_pre".to_owned()));
            assert!(!args.contains(&"-stats_mux_pre_fmt".to_owned()));
        }
    }

    #[test]
    fn the_rtmp_slave_follows_the_recording() {
        assert_eq!(rtmp_slave_index(&config()), 1);
        let stream_only = OutputConfig {
            record_path: None,
            ..config()
        };
        assert_eq!(rtmp_slave_index(&stream_only), 0);
        assert!(tee_outputs(&stream_only).starts_with("'[f=flv"));
    }

    #[test]
    fn tee_with_a_single_destination() {
        let record_only = OutputConfig {
            rtmp_url: None,
            ..config()
        };
        assert!(!tee_outputs(&record_only).contains('|'));
        assert!(tee_outputs(&record_only).contains("f=mp4"));
        let stream_only = OutputConfig {
            record_path: None,
            ..config()
        };
        assert!(!tee_outputs(&stream_only).contains('|'));
        assert!(tee_outputs(&stream_only).contains("f=flv"));
        let nothing = OutputConfig {
            rtmp_url: None,
            record_path: None,
            ..config()
        };
        assert_eq!(tee_outputs(&nothing), "");
    }

    #[test]
    fn tee_escapes_awkward_paths() {
        let c = OutputConfig {
            rtmp_url: None,
            record_path: Some("/rec/it's a|b [1]:x.mp4".into()),
            ..config()
        };
        let tee = tee_outputs(&c);
        assert_eq!(
            tee,
            "'[f=mp4:movflags=+frag_keyframe+empty_moov+default_base_moof]\
             file:/rec/it'\\''s a|b [1]:x.mp4'"
        );
        assert_eq!(quote_tee_token(""), "''");
        assert_eq!(quote_tee_token("'"), r"''\'''");
    }

    #[test]
    fn gop_follows_keyframe_seconds() {
        let c = OutputConfig {
            fps: 30,
            keyframe_seconds: 1,
            ..config()
        };
        let args = build_ffmpeg_args(&c, &EncoderChoice::Libx264, None, false);
        assert_eq!(value(&args, "-g"), "30");
    }
}
