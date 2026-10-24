//! Exact frame rates and frame timestamps.
//!
//! A frame rate is kept as a reduced fraction of frames per second
//! (`30000/1001`, not `29.97`). The presentation time of frame `n` is then
//! computed directly as `n * den / num` seconds in integer nanoseconds, so it
//! never accumulates floating-point drift, however long a source runs, and two
//! different frame numbers can never map to the same or a decreasing PTS.
//!
//! Invariants: `num > 0`, `den > 0`, the fraction is fully reduced, and the
//! rate is at most [`FrameRate::MAX_FPS`].

use std::fmt;
use std::time::Duration;

/// Nanoseconds per second.
const NANOS: u128 = 1_000_000_000;

/// A frame rate as a reduced fraction of frames per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameRate {
    /// Numerator: frames.
    num: u32,
    /// Denominator: seconds.
    den: u32,
}

impl FrameRate {
    /// Highest accepted rate, in frames per second.
    pub const MAX_FPS: f64 = 1000.0;

    /// `num / den` frames per second; `None` when either part is zero or the
    /// rate exceeds [`FrameRate::MAX_FPS`].
    pub fn new(num: u32, den: u32) -> Option<Self> {
        if num == 0 || den == 0 || num as f64 / den as f64 > Self::MAX_FPS {
            return None;
        }
        let g = gcd(num, den);
        Some(Self {
            num: num / g,
            den: den / g,
        })
    }

    /// Parses `"30000/1001"`, `"30"` or `"29.97"`; `None` for anything else,
    /// including ffprobe's `"0/0"` placeholder.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some((n, d)) = text.split_once('/') {
            return Self::new(n.trim().parse().ok()?, d.trim().parse().ok()?);
        }
        if let Ok(n) = text.parse::<u32>() {
            return Self::new(n, 1);
        }
        Self::from_f64(text.parse().ok()?)
    }

    /// The closest conventional fraction for a decimal rate.
    ///
    /// Whole numbers map to `n/1`, the NTSC family (`23.976`, `29.97`,
    /// `59.94`, …) to `n*1000/1001`, and anything else to the nearest
    /// thousandth. `None` for non-finite, non-positive or excessive rates.
    pub fn from_f64(fps: f64) -> Option<Self> {
        if !fps.is_finite() || fps <= 0.0 || fps > Self::MAX_FPS {
            return None;
        }
        let whole = fps.round();
        if (fps - whole).abs() < 1e-9 && whole >= 1.0 {
            return Self::new(whole as u32, 1);
        }
        let ntsc = (fps * 1.001).round();
        if ntsc >= 1.0 && (ntsc * 1000.0 / 1001.0 - fps).abs() < 5e-4 {
            return Self::new(ntsc as u32 * 1000, 1001);
        }
        let millis = (fps * 1000.0).round();
        if millis < 1.0 {
            return None;
        }
        Self::new(millis as u32, 1000)
    }

    /// Numerator of the reduced fraction.
    pub fn num(self) -> u32 {
        self.num
    }

    /// Denominator of the reduced fraction.
    pub fn den(self) -> u32 {
        self.den
    }

    /// Frames per second as a float, for display and [`ac_media::SourceInfo`].
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Presentation time of frame `frame`, rounded down to the nanosecond.
    pub fn pts(self, frame: u64) -> Duration {
        let nanos = frame as u128 * self.den as u128 * NANOS / self.num as u128;
        let secs = u64::try_from(nanos / NANOS).unwrap_or(u64::MAX);
        Duration::new(secs, (nanos % NANOS) as u32)
    }

    /// Duration of one frame, rounded down to the nanosecond.
    pub fn frame_interval(self) -> Duration {
        self.pts(1)
    }
}

impl fmt::Display for FrameRate {
    /// Formats as `num/den`, the syntax ffmpeg's `-r` option accepts.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// Greatest common divisor by Euclid's algorithm.
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_fractions_and_reduces_them() {
        let r = FrameRate::parse("60000/2002").unwrap();
        assert_eq!((r.num(), r.den()), (30000, 1001));
        assert_eq!(FrameRate::parse("25"), FrameRate::new(25, 1));
        assert_eq!(FrameRate::parse(" 30/1 "), FrameRate::new(30, 1));
        assert_eq!(FrameRate::parse("29.97"), FrameRate::new(30000, 1001));
    }

    #[test]
    fn rejects_placeholder_and_garbage() {
        for bad in ["0/0", "30/0", "0/1", "", "abc", "1/x", "-30", "5000/1"] {
            assert_eq!(FrameRate::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn pts_is_exact_for_ntsc_rates() {
        let r = FrameRate::new(30000, 1001).unwrap();
        assert_eq!(r.pts(0), Duration::ZERO);
        assert_eq!(r.pts(30_000), Duration::from_secs(1001));
        assert_eq!(r.frame_interval(), Duration::from_nanos(33_366_666));
    }

    #[test]
    fn pts_strictly_increases() {
        let r = FrameRate::new(1000, 1).unwrap();
        let pts: Vec<Duration> = (0..2000).map(|n| r.pts(n)).collect();
        assert!(pts.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn pts_saturates_instead_of_overflowing() {
        let r = FrameRate::new(1, 1000).unwrap();
        assert!(r.pts(u64::MAX) > Duration::from_secs(1 << 60));
    }

    #[test]
    fn decimal_rates_map_to_conventional_fractions() {
        assert_eq!(FrameRate::from_f64(30.0), FrameRate::new(30, 1));
        assert_eq!(FrameRate::from_f64(23.976), FrameRate::new(24000, 1001));
        assert_eq!(FrameRate::from_f64(59.94), FrameRate::new(60000, 1001));
        assert_eq!(FrameRate::from_f64(12.5), FrameRate::new(25, 2));
        assert_eq!(FrameRate::from_f64(0.5), FrameRate::new(1, 2));
    }

    #[test]
    fn decimal_rates_reject_invalid_values() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e6, 1e-4] {
            assert_eq!(FrameRate::from_f64(bad), None, "{bad}");
        }
    }

    #[test]
    fn displays_as_ffmpeg_fraction() {
        assert_eq!(
            FrameRate::new(30000, 1001).unwrap().to_string(),
            "30000/1001"
        );
        assert!((FrameRate::new(30000, 1001).unwrap().as_f64() - 29.97).abs() < 1e-3);
    }
}
