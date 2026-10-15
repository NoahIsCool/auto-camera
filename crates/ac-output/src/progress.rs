//! Parsing ffmpeg's `-progress` reports.
//!
//! With `-progress pipe:1` ffmpeg writes a block of `key=value` lines every
//! stats period and terminates each block with `progress=continue`, or
//! `progress=end` for the last one:
//!
//! ```text
//! frame=44
//! fps=28.59
//! bitrate=N/A
//! total_size=N/A
//! out_time_us=1433333
//! dup_frames=0
//! drop_frames=0
//! speed=0.931x
//! progress=continue
//! ```
//!
//! [`ProgressParser`] accumulates one block and publishes a [`Progress`]
//! snapshot at its terminator, so readers never see a half-updated report.
//! Unknown keys, malformed lines and `N/A` values are ignored; a missing
//! value keeps the previous one, because ffmpeg omits nothing it knows.
//!
//! The tee muxer has no single output byte stream, so ffmpeg reports
//! `bitrate=N/A` and `total_size=N/A` for it. The encoder fills
//! [`Progress::bitrate_kbps`] from the recording's size instead (see
//! [`average_kbps`]), which is the average since the start.

use std::time::Duration;

/// One `-progress` report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    /// Frames encoded so far.
    pub frame: u64,
    /// Encoding rate in frames per second, as ffmpeg measures it.
    pub fps: Option<f64>,
    /// Output bitrate in kbit/s: ffmpeg's figure, or the recording's average
    /// when ffmpeg cannot report one (always the case with the tee muxer).
    pub bitrate_kbps: Option<f64>,
    /// Bytes written, when ffmpeg knows it.
    pub total_size: Option<u64>,
    /// Media time of the newest output packet.
    pub out_time: Option<Duration>,
    /// Frames ffmpeg duplicated to keep a constant rate (expected 0).
    pub dup_frames: u64,
    /// Frames ffmpeg dropped to keep a constant rate (expected 0).
    pub drop_frames: u64,
    /// Encoding speed relative to real time (1.0 keeps up).
    pub speed: Option<f64>,
    /// True once ffmpeg reported its final block.
    pub ended: bool,
}

/// Accumulates `-progress` lines into [`Progress`] snapshots.
#[derive(Debug, Default)]
pub struct ProgressParser {
    /// The block being assembled, starting from the previous snapshot.
    current: Progress,
}

/// Parses a decimal with an optional unit suffix (`"1.5x"`, `"800kbits/s"`).
fn number_with_suffix(value: &str, suffix: &str) -> Option<f64> {
    let v: f64 = value.trim().strip_suffix(suffix)?.trim().parse().ok()?;
    v.is_finite().then_some(v)
}

impl ProgressParser {
    /// A parser with an empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one line; returns the completed snapshot at a block terminator.
    pub fn feed(&mut self, line: &str) -> Option<Progress> {
        let (key, value) = line.trim().split_once('=')?;
        let value = value.trim();
        let p = &mut self.current;
        match key.trim() {
            "frame" => p.frame = value.parse().unwrap_or(p.frame),
            "fps" => p.fps = value.parse().ok().filter(|v: &f64| v.is_finite()).or(p.fps),
            "bitrate" => p.bitrate_kbps = number_with_suffix(value, "kbits/s").or(p.bitrate_kbps),
            "total_size" => p.total_size = value.parse().ok().or(p.total_size),
            "out_time_us" => {
                // Negative before the first packet; N/A when unknown.
                if let Ok(us) = value.parse::<u64>() {
                    p.out_time = Some(Duration::from_micros(us));
                }
            }
            "dup_frames" => p.dup_frames = value.parse().unwrap_or(p.dup_frames),
            "drop_frames" => p.drop_frames = value.parse().unwrap_or(p.drop_frames),
            "speed" => p.speed = number_with_suffix(value, "x").or(p.speed),
            "progress" => {
                p.ended = value == "end";
                return Some(p.clone());
            }
            _ => {}
        }
        None
    }
}

/// Average bitrate in kbit/s of `bytes` written over `elapsed` media time.
///
/// `None` for less than 100 ms of media, where the estimate is noise.
pub fn average_kbps(bytes: u64, elapsed: Duration) -> Option<f64> {
    let secs = elapsed.as_secs_f64();
    (secs >= 0.1).then(|| bytes as f64 * 8.0 / 1000.0 / secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds every line of `text`, returning the snapshots produced.
    fn feed_all(parser: &mut ProgressParser, text: &str) -> Vec<Progress> {
        text.lines().filter_map(|l| parser.feed(l)).collect()
    }

    #[test]
    fn parses_a_tee_block() {
        let mut p = ProgressParser::new();
        let out = feed_all(
            &mut p,
            "frame=44\nfps=28.59\nstream_0_0_q=0.0\nbitrate=N/A\ntotal_size=N/A\n\
             out_time_us=1433333\nout_time_ms=1433333\nout_time=00:00:01.433333\n\
             dup_frames=0\ndrop_frames=1\nspeed=0.931x\nprogress=continue\n",
        );
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.frame, 44);
        assert_eq!(r.fps, Some(28.59));
        assert_eq!(r.bitrate_kbps, None);
        assert_eq!(r.total_size, None);
        assert_eq!(r.out_time, Some(Duration::from_micros(1_433_333)));
        assert_eq!(r.drop_frames, 1);
        assert_eq!(r.speed, Some(0.931));
        assert!(!r.ended);
    }

    #[test]
    fn parses_a_file_block_and_end() {
        let mut p = ProgressParser::new();
        let out = feed_all(
            &mut p,
            "frame=60\nfps=0.00\nbitrate= 158.3kbits/s\ntotal_size=39174\n\
             out_time_us=1984000\nspeed=   0x\nprogress=end\n",
        );
        let r = &out[0];
        assert_eq!(r.bitrate_kbps, Some(158.3));
        assert_eq!(r.total_size, Some(39_174));
        assert_eq!(r.speed, Some(0.0));
        assert!(r.ended);
    }

    #[test]
    fn nothing_is_published_mid_block() {
        let mut p = ProgressParser::new();
        assert!(p.feed("frame=1").is_none());
        assert!(p.feed("fps=1").is_none());
        assert_eq!(p.feed("progress=continue").unwrap().frame, 1);
    }

    #[test]
    fn ignores_garbage_and_keeps_known_values() {
        let mut p = ProgressParser::new();
        feed_all(&mut p, "frame=10\nfps=30\nspeed=1.0x\nprogress=continue\n");
        let out = feed_all(
            &mut p,
            "no equals sign\n=\nframe=abc\nfps=nan\nspeed=fast\nout_time_us=-9223372036854775807\n\
             unknown=1\nprogress=continue\n",
        );
        let r = &out[0];
        assert_eq!(r.frame, 10);
        assert_eq!(r.fps, Some(30.0));
        assert_eq!(r.speed, Some(1.0));
        assert_eq!(r.out_time, None);
    }

    #[test]
    fn average_bitrate() {
        assert_eq!(average_kbps(1_000, Duration::from_secs(1)), Some(8.0));
        assert_eq!(average_kbps(1_000, Duration::from_millis(50)), None);
        assert_eq!(average_kbps(0, Duration::from_secs(2)), Some(0.0));
    }
}
