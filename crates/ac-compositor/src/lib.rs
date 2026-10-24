//! CPU reference compositor.
//!
//! The switcher decides *what* is on air ([`ac_switcher::Output`]: a base
//! shot, an optional overlay with an opacity, and a dip-to-black opacity); the
//! framing controller decides *where* each shot looks (a crop [`Rect`] in
//! normalised source coordinates). The compositor turns those into pixels:
//!
//! 1. **Crop and resample** each visible shot's source frame to the output
//!    size. Magnification uses bilinear interpolation; minification uses an
//!    area-average (box) filter so that shrinking 4K to 1080p, or any source
//!    to a multiview tile, does not alias. Crops that hang over the frame edge
//!    repeat the edge pixels ("clamp to edge"). The filters are separable and
//!    precomputed per axis; see the `filter` module for the exact mapping.
//! 2. **Mix** the overlay over the base with opacity `a`:
//!    `mix = base * (1 - a) + overlay * a`.
//! 3. **Dip** towards black with opacity `d`: `out = mix * (1 - d)`.
//!
//! Steps 2 and 3 are linear, so they are folded into one weight per layer
//! (`(1 - a)(1 - d)` for the base, `a (1 - d)` for the overlay), the weighted
//! layers are summed in `f32`, and the sum is rounded to RGBA8 exactly once.
//! Layers whose weight is zero are not sampled at all, so a cut or a finished
//! dissolve costs one layer and full black costs none.
//!
//! # Missing shots
//!
//! The program output must never stall, so [`Compositor::render`] always
//! produces a frame. A shot named by the switcher but absent from the layer
//! map, or whose frame or crop is unusable, is drawn as black and listed in
//! the [`RenderReport`]; the engine can then warn the operator or fall back to
//! a wide shot. `base: None` is the switcher's explicit "black" and is not an
//! issue. Opacities are clamped to `0.0..=1.0` and NaN counts as `0.0`.
//!
//! # Colour
//!
//! Frames are packed 8-bit RGBA holding sRGB-encoded (gamma-encoded) values,
//! and the compositor filters and blends those encoded values directly. For
//! dissolves and dips this matches broadcast switchers, which mix the
//! gamma-encoded Y'CbCr signal. For resampling it is an approximation: averaging
//! encoded values slightly darkens fine high-contrast detail compared with
//! averaging light. Gamma-correct (linear-light) filtering, by decoding through
//! a lookup table to linear `f32`, filtering, and re-encoding, or by using sRGB
//! texture formats on the GPU, is future work. The sources' alpha channel is
//! ignored and every output pixel is opaque (`A = 255`).
//!
//! # Performance and ownership
//!
//! Rows are rendered in parallel bands with rayon; each row is independent, so
//! the result does not depend on the thread count. [`Compositor`] owns all
//! scratch memory (filter tables and per-band row buffers) and a small pool of
//! output buffers that are recycled once consumers drop their frames, so
//! steady-state rendering does not allocate. A Metal/GPU compositor will
//! implement the same API later; this crate stays the bit-exact reference it
//! is tested against.

mod compositor;
mod filter;
mod kernel;
mod pool;

use std::collections::HashMap;

use ac_core::{Rect, ShotId, SourceId};
use ac_media::VideoFrame;
use thiserror::Error;

pub use compositor::{Compositor, TILE_GENERATIONS};

/// Largest accepted output or tile dimension in pixels.
pub const MAX_DIMENSION: u32 = 16_384;

/// Largest accepted magnitude of any crop coordinate or size, in normalised
/// source units. Anything larger is a bug upstream, not a framing decision.
pub const MAX_CROP_EXTENT: f64 = 1.0e4;

/// A program frame: the composited picture, tagged with the configured
/// program [`SourceId`], a sequence number counting rendered frames from zero,
/// and the presentation time the caller rendered it for.
pub type OutputFrame = VideoFrame;

/// The layers available to a render, keyed by shot.
pub type Layers<'a> = HashMap<ShotId, Layer<'a>>;

/// Why a frame, crop or size was refused.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum CompositorError {
    /// Output or tile dimensions outside `1..=MAX_DIMENSION`.
    #[error("size {width}x{height} is outside 1..={max} pixels", max = MAX_DIMENSION)]
    InvalidSize {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// The frame's pixel data does not match its dimensions.
    #[error("frame {width}x{height} with {len} bytes is not packed RGBA8")]
    BadFrame {
        /// Frame width.
        width: u32,
        /// Frame height.
        height: u32,
        /// Length of the pixel data.
        len: usize,
    },
    /// The crop is not finite, has no area, or is absurdly large.
    #[error("crop {0:?} is not a finite rectangle with positive size")]
    InvalidCrop(Rect),
}

/// Output configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositorConfig {
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Source id stamped on program frames.
    pub program: SourceId,
}

impl CompositorConfig {
    /// A `width` x `height` output tagged with the source id `"program"`.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            program: SourceId("program".into()),
        }
    }

    /// Output width divided by height.
    pub fn aspect(&self) -> f64 {
        f64::from(self.width) / f64::from(self.height)
    }
}

impl Default for CompositorConfig {
    /// 1920x1080, the minimum program resolution in the spec.
    fn default() -> Self {
        Self::new(1920, 1080)
    }
}

/// One shot's picture: the source frame and the crop to show from it.
#[derive(Debug, Clone, Copy)]
pub struct Layer<'a> {
    /// The newest frame of the shot's source.
    pub frame: &'a VideoFrame,
    /// The shot's crop in normalised source coordinates. Parts outside
    /// `0.0..=1.0` repeat the edge pixels. The crop is stretched to the output
    /// size, so its aspect should match the output's (framing ensures this).
    pub crop: Rect,
}

impl<'a> Layer<'a> {
    /// A layer showing `crop` of `frame`.
    pub fn new(frame: &'a VideoFrame, crop: Rect) -> Self {
        Self { frame, crop }
    }
}

/// A problem with one shot during a render. The shot was drawn as black.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerIssue {
    /// No layer was supplied for the shot.
    Missing(ShotId),
    /// The shot's layer was supplied but unusable.
    Invalid {
        /// The affected shot.
        shot: ShotId,
        /// What was wrong with its frame or crop.
        reason: CompositorError,
    },
}

impl LayerIssue {
    /// The affected shot.
    pub fn shot(&self) -> &ShotId {
        match self {
            LayerIssue::Missing(shot) | LayerIssue::Invalid { shot, .. } => shot,
        }
    }
}

/// What happened during a render, beyond the picture itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderReport {
    /// Shots that should have been visible but were drawn as black, at most
    /// one entry per shot.
    pub issues: Vec<LayerIssue>,
}

impl RenderReport {
    /// True when every requested shot was drawn.
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
}

/// A rendered program frame and its report.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// The composited picture.
    pub frame: OutputFrame,
    /// Shots that could not be drawn.
    pub report: RenderReport,
}

/// Checks that `width` x `height` is a usable output or tile size.
fn check_size(width: u32, height: u32) -> Result<(), CompositorError> {
    let ok = |v: u32| (1..=MAX_DIMENSION).contains(&v);
    if ok(width) && ok(height) {
        Ok(())
    } else {
        Err(CompositorError::InvalidSize { width, height })
    }
}

/// Checks that a frame's data matches its dimensions. `VideoFrame`'s fields
/// are public, so a frame built without [`VideoFrame::new`] may be malformed,
/// and sampling it would read out of bounds.
fn check_frame(frame: &VideoFrame) -> Result<(), CompositorError> {
    let expected = frame.width as usize * frame.height as usize * 4;
    if frame.width == 0 || frame.height == 0 || frame.data.len() != expected {
        Err(CompositorError::BadFrame {
            width: frame.width,
            height: frame.height,
            len: frame.data.len(),
        })
    } else {
        Ok(())
    }
}

/// Checks that a crop is finite, has positive size and is not absurdly large.
fn check_crop(crop: &Rect) -> Result<(), CompositorError> {
    let sane = |v: f64| v.is_finite() && v.abs() <= MAX_CROP_EXTENT;
    if [crop.x, crop.y, crop.w, crop.h].into_iter().all(sane) && crop.w > 0.0 && crop.h > 0.0 {
        Ok(())
    } else {
        Err(CompositorError::InvalidCrop(*crop))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn sizes_are_bounded() {
        assert!(check_size(1, 1).is_ok());
        assert!(check_size(MAX_DIMENSION, MAX_DIMENSION).is_ok());
        assert_eq!(
            check_size(0, 10),
            Err(CompositorError::InvalidSize {
                width: 0,
                height: 10
            })
        );
        assert!(check_size(10, MAX_DIMENSION + 1).is_err());
    }

    #[test]
    fn crops_must_be_finite_and_positive() {
        assert!(check_crop(&Rect::FULL).is_ok());
        assert!(check_crop(&Rect::new(-0.5, 1.2, 0.1, 0.1)).is_ok());
        for bad in [
            Rect::new(f64::NAN, 0.0, 1.0, 1.0),
            Rect::new(0.0, f64::INFINITY, 1.0, 1.0),
            Rect::new(0.0, 0.0, 0.0, 1.0),
            Rect::new(0.0, 0.0, 1.0, -1.0),
            Rect::new(0.0, 0.0, 2.0 * MAX_CROP_EXTENT, 1.0),
        ] {
            // NaN != NaN, so match rather than compare.
            assert!(matches!(
                check_crop(&bad),
                Err(CompositorError::InvalidCrop(_))
            ));
        }
    }

    #[test]
    fn malformed_frames_are_detected() {
        let mut frame = VideoFrame::new(
            SourceId("cam".into()),
            0,
            Duration::ZERO,
            2,
            2,
            Arc::from(vec![0u8; 16]),
        )
        .unwrap();
        assert!(check_frame(&frame).is_ok());
        frame.width = 3;
        assert!(matches!(
            check_frame(&frame),
            Err(CompositorError::BadFrame { width: 3, .. })
        ));
        frame.width = 0;
        assert!(check_frame(&frame).is_err());
    }

    #[test]
    fn config_defaults_to_1080p_program() {
        let c = CompositorConfig::default();
        assert_eq!((c.width, c.height), (1920, 1080));
        assert_eq!(c.program, SourceId("program".into()));
        assert!((c.aspect() - 16.0 / 9.0).abs() < 1e-12);
    }

    #[test]
    fn issues_name_their_shot() {
        let shot = ShotId("wide".into());
        assert_eq!(LayerIssue::Missing(shot.clone()).shot(), &shot);
        let invalid = LayerIssue::Invalid {
            shot: shot.clone(),
            reason: CompositorError::InvalidCrop(Rect::default()),
        };
        assert_eq!(invalid.shot(), &shot);
        assert!(RenderReport::default().is_clean());
    }
}
