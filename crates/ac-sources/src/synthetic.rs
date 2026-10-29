//! A deterministic moving test pattern with exact ground truth.
//!
//! [`SyntheticPattern`] draws a dark background with a faint grid (so pans and
//! zooms are visible in demos) and a number of solid-coloured rectangles that
//! drift across it like people on a stage. Both the picture and the subject
//! boxes are pure functions of PTS, so any frame can be recomputed at any time,
//! two patterns with the same parameters agree byte for byte, and the boxes
//! returned by [`SyntheticPattern::ground_truth`] are the exact truth for
//! testing detectors, trackers and framing downstream.
//!
//! Motion: subject `i` of `n` moves horizontally on a triangle wave (constant
//! speed, bouncing at the frame edges) with period `8 + 3i` seconds, starting
//! at `i/n` of the way across, and bobs vertically by 5% of the frame height
//! with period `5 + 2i` seconds. The boxes always lie inside the frame. Paths
//! may cross; later subjects are drawn on top of earlier ones.
//!
//! Rasterization: a pixel belongs to a box when the pixel's centre lies inside
//! it (left/top edges inclusive, right/bottom exclusive), so the drawn
//! rectangle is within half a pixel of the true box on every side.
//!
//! [`SyntheticSource`] plays a pattern as a live [`VideoSource`], paced by a
//! [`Clock`] exactly like a file source, with `pts = seq / fps`. For tests of
//! supervision, [`crate::fault`] starts one that fails at a set clock
//! reading, as a source does when its input is lost.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_core::{Rect, SourceId, SubjectId};
use ac_media::{
    AudioBlock, LatestFrame, MediaError, SourceInfo, SourceState, VideoFrame, VideoSource,
};

use crate::clock::{Clock, Timeline, WallClock};
use crate::frame_rate::FrameRate;
use crate::pool::FramePool;
use crate::status::Status;

/// One moving subject's exact position at some PTS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroundTruth {
    /// Subject identifier: its index in the pattern.
    pub id: SubjectId,
    /// Exact box in normalized source coordinates.
    pub bbox: Rect,
    /// The subject's solid RGBA colour.
    pub color: [u8; 4],
}

/// Motion parameters of one subject.
#[derive(Debug, Clone, PartialEq)]
struct Mover {
    /// Fill colour.
    color: [u8; 4],
    /// Box width as a fraction of the frame width.
    w: f64,
    /// Box height as a fraction of the frame height.
    h: f64,
    /// Seconds per horizontal round trip.
    period_x: f64,
    /// Horizontal phase in cycles.
    phase_x: f64,
    /// Seconds per vertical bob.
    period_y: f64,
    /// Vertical phase in cycles.
    phase_y: f64,
    /// Top edge at the low point of the bob.
    y_base: f64,
}

/// Vertical bob amplitude as a fraction of the frame height.
const BOB: f64 = 0.05;

impl Mover {
    /// Subject `i` of `n`; see the module docs for the motion.
    fn new(i: u32, n: u32) -> Self {
        let fi = i as f64;
        let w = 0.06 + 0.01 * (i % 4) as f64;
        let h = 0.25 + 0.05 * (i % 3) as f64;
        Self {
            color: hsv_color((0.08 + 0.618_034 * fi).fract(), 0.85, 0.95),
            w,
            h,
            period_x: 8.0 + 3.0 * fi,
            phase_x: fi / (2.0 * n as f64),
            period_y: 5.0 + 2.0 * fi,
            phase_y: (0.37 * fi).fract(),
            y_base: 0.5 - h / 2.0 - BOB / 2.0,
        }
    }

    /// Exact box at `t` seconds.
    fn bbox(&self, t: f64) -> Rect {
        let x = (1.0 - self.w) * triangle(t / self.period_x + self.phase_x);
        let y = self.y_base + BOB * triangle(t / self.period_y + self.phase_y);
        Rect::new(x, y, self.w, self.h)
    }
}

/// Triangle wave: 0 at whole cycles, 1 at half cycles, linear in between.
fn triangle(cycles: f64) -> f64 {
    1.0 - (2.0 * cycles.rem_euclid(1.0) - 1.0).abs()
}

/// An opaque RGBA colour from hue, saturation and value in `0..=1`.
fn hsv_color(h: f64, s: f64, v: f64) -> [u8; 4] {
    let sector = (h * 6.0).rem_euclid(6.0);
    let f = sector.fract();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match sector as u32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let byte = |c: f64| (c * 255.0).round().clamp(0.0, 255.0) as u8;
    [byte(r), byte(g), byte(b), 255]
}

/// Half-open pixel range whose centres lie in `[start, start + len)` of an
/// axis `size` pixels long.
fn pixel_span(start: f64, len: f64, size: u32) -> (usize, usize) {
    let to_px = |edge: f64| (edge * size as f64 - 0.5).ceil().clamp(0.0, size as f64) as usize;
    (to_px(start), to_px(start + len))
}

/// A deterministic moving test pattern; see [`crate::synthetic`].
#[derive(Clone, PartialEq)]
pub struct SyntheticPattern {
    /// Width in pixels.
    width: u32,
    /// Height in pixels.
    height: u32,
    /// Frame rate; defines PTS.
    rate: FrameRate,
    /// The moving subjects, in drawing order.
    movers: Vec<Mover>,
    /// Pre-rendered background and grid.
    background: Arc<[u8]>,
}

impl fmt::Debug for SyntheticPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyntheticPattern")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rate", &self.rate)
            .field("subjects", &self.movers.len())
            .finish_non_exhaustive()
    }
}

impl SyntheticPattern {
    /// Most subjects a pattern may have.
    pub const MAX_SUBJECTS: u32 = 32;
    /// Largest accepted width or height in pixels.
    pub const MAX_SIDE: u32 = 8192;
    /// Background colour.
    pub const BACKGROUND: [u8; 4] = [24, 30, 44, 255];
    /// Grid line colour.
    pub const GRID: [u8; 4] = [52, 60, 78, 255];
    /// Grid cells per side; the grid is drawn only when cells are at least two
    /// pixels.
    pub const GRID_DIVISIONS: u32 = 8;

    /// A `width` x `height` pattern at `fps` with `subjects` moving boxes.
    pub fn new(width: u32, height: u32, fps: f64, subjects: u32) -> Result<Self, MediaError> {
        let rate = Self::check(width, height, fps, subjects)?;
        Ok(Self {
            width,
            height,
            rate,
            movers: (0..subjects).map(|i| Mover::new(i, subjects)).collect(),
            background: render_background(width, height),
        })
    }

    /// Checks the parameters of [`SyntheticPattern::new`] without rendering
    /// anything, returning the frame rate; `new` accepts exactly what this
    /// accepts. A config is checked with it before a source is opened, since
    /// a pattern that cannot be made is a mistake no reconnect can mend.
    pub fn check(
        width: u32,
        height: u32,
        fps: f64,
        subjects: u32,
    ) -> Result<FrameRate, MediaError> {
        let invalid = |msg: String| Err(MediaError::Open(format!("synthetic source: {msg}")));
        if !(1..=Self::MAX_SIDE).contains(&width) || !(1..=Self::MAX_SIDE).contains(&height) {
            return invalid(format!(
                "size {width}x{height} outside 1..={}",
                Self::MAX_SIDE
            ));
        }
        let Some(rate) = FrameRate::from_f64(fps) else {
            return invalid(format!("unusable frame rate {fps}"));
        };
        if subjects > Self::MAX_SUBJECTS {
            return invalid(format!(
                "{subjects} subjects, at most {} allowed",
                Self::MAX_SUBJECTS
            ));
        }
        Ok(rate)
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Frame rate; frame `seq` has PTS `frame_rate().pts(seq)`.
    pub fn frame_rate(&self) -> FrameRate {
        self.rate
    }

    /// Number of moving subjects.
    pub fn subject_count(&self) -> usize {
        self.movers.len()
    }

    /// Exact boxes of all subjects at `pts`, in drawing order.
    pub fn ground_truth(&self, pts: Duration) -> Vec<GroundTruth> {
        let t = pts.as_secs_f64();
        self.movers
            .iter()
            .enumerate()
            .map(|(i, m)| GroundTruth {
                id: SubjectId(i as u32),
                bbox: m.bbox(t),
                color: m.color,
            })
            .collect()
    }

    /// Renders the picture at `pts` into `out` (packed RGBA, no padding).
    pub fn render_into(&self, pts: Duration, out: &mut [u8]) -> Result<(), MediaError> {
        if out.len() != self.background.len() {
            return Err(MediaError::BadFrameSize {
                width: self.width,
                height: self.height,
                len: out.len(),
            });
        }
        out.copy_from_slice(&self.background);
        let stride = self.width as usize * 4;
        for gt in self.ground_truth(pts) {
            let (x0, x1) = pixel_span(gt.bbox.x, gt.bbox.w, self.width);
            let (y0, y1) = pixel_span(gt.bbox.y, gt.bbox.h, self.height);
            if x1 <= x0 {
                continue;
            }
            let run: Vec<u8> = gt.color.repeat(x1 - x0);
            for row in out[y0 * stride..y1 * stride].chunks_exact_mut(stride) {
                row[x0 * 4..x1 * 4].copy_from_slice(&run);
            }
        }
        Ok(())
    }

    /// Frame `seq` of the pattern, tagged with `source`.
    pub fn frame(&self, source: &SourceId, seq: u64) -> VideoFrame {
        let pts = self.rate.pts(seq);
        let mut data = vec![0u8; self.background.len()];
        self.render_into(pts, &mut data)
            .expect("buffer is sized for the pattern");
        VideoFrame::new(
            source.clone(),
            seq,
            pts,
            self.width,
            self.height,
            data.into(),
        )
        .expect("pattern sizes are validated and non-zero")
    }
}

/// The background colour with grid lines at every `1/GRID_DIVISIONS` of each
/// side, when cells are at least two pixels.
fn render_background(width: u32, height: u32) -> Arc<[u8]> {
    let (w, h) = (width as usize, height as usize);
    let mut px = SyntheticPattern::BACKGROUND.repeat(w * h);
    let div = SyntheticPattern::GRID_DIVISIONS as usize;
    let lines = |size: usize| -> Vec<usize> {
        if size < 2 * div {
            return Vec::new();
        }
        (1..div).map(|k| k * size / div).collect()
    };
    let grid = SyntheticPattern::GRID;
    for x in lines(w) {
        for y in 0..h {
            px[(y * w + x) * 4..][..4].copy_from_slice(&grid);
        }
    }
    for y in lines(h) {
        for x in 0..w {
            px[(y * w + x) * 4..][..4].copy_from_slice(&grid);
        }
    }
    px.into()
}

/// State shared by a [`SyntheticSource`] and its worker.
struct Shared {
    /// Raised once by `stop`.
    stop: AtomicBool,
    /// Health.
    status: Status,
    /// Pacing.
    timeline: Timeline,
    /// The newest frame.
    latest: LatestFrame,
    /// Clock reading at which the source fails as if its input were lost
    /// (fault injection, [`crate::fault`]); the first frame due at or after
    /// it is not published.
    fail_at: Option<Duration>,
}

/// A [`SyntheticPattern`] played as a live source; see [`crate::synthetic`].
pub struct SyntheticSource {
    /// Static description.
    info: SourceInfo,
    /// The pattern being played.
    pattern: Arc<SyntheticPattern>,
    /// State shared with the worker.
    shared: Arc<Shared>,
    /// The rendering worker, joined by `stop`.
    worker: Option<JoinHandle<()>>,
}

impl fmt::Debug for SyntheticSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyntheticSource")
            .field("info", &self.info)
            .field("state", &self.shared.status.get())
            .finish_non_exhaustive()
    }
}

impl SyntheticSource {
    /// Starts playing `pattern` in real time.
    pub fn start(id: SourceId, pattern: SyntheticPattern) -> Result<Self, MediaError> {
        Self::start_with_clock(id, pattern, Arc::new(WallClock::new()))
    }

    /// Starts playing `pattern` paced by `clock`; with a
    /// [`crate::VirtualClock`] it renders as fast as it can.
    pub fn start_with_clock(
        id: SourceId,
        pattern: SyntheticPattern,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, MediaError> {
        Self::start_failing(id, pattern, clock, None)
    }

    /// Starts playing `pattern` paced by `clock`, failing with an error once
    /// the clock reaches `fail_at`, if given.
    pub(crate) fn start_failing(
        id: SourceId,
        pattern: SyntheticPattern,
        clock: Arc<dyn Clock>,
        fail_at: Option<Duration>,
    ) -> Result<Self, MediaError> {
        let info = SourceInfo {
            id: id.clone(),
            width: pattern.width,
            height: pattern.height,
            fps: pattern.rate.as_f64(),
            audio: None,
        };
        let pattern = Arc::new(pattern);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            status: Status::new(),
            timeline: Timeline::new(clock),
            latest: LatestFrame::new(),
            fail_at,
        });
        let (p, s) = (pattern.clone(), shared.clone());
        let worker = std::thread::Builder::new()
            .name(format!("ac-synthetic-{}", id.0))
            .spawn(move || render_loop(&id, &p, &s))?;
        Ok(Self {
            info,
            pattern,
            shared,
            worker: Some(worker),
        })
    }

    /// The pattern being played.
    pub fn pattern(&self) -> &SyntheticPattern {
        &self.pattern
    }

    /// Exact subject boxes at `pts`; pass a frame's PTS to get its truth.
    pub fn ground_truth(&self, pts: Duration) -> Vec<GroundTruth> {
        self.pattern.ground_truth(pts)
    }
}

/// Renders, paces and publishes frames until stopped.
fn render_loop(id: &SourceId, pattern: &SyntheticPattern, shared: &Shared) {
    let mut pool = FramePool::new(pattern.background.len());
    let mut seq = 0u64;
    while !shared.stop.load(Ordering::Acquire) {
        let pts = pattern.rate.pts(seq);
        let (data, rendered) = pool.fill(|buf| pattern.render_into(pts, buf));
        if let Err(e) = rendered {
            shared.status.fail(e.to_string());
            break;
        }
        if !shared.timeline.wait_until(pts, &shared.stop) {
            break;
        }
        if shared.fail_at.is_some_and(|t| shared.timeline.now() >= t) {
            shared.status.fail("injected fault: the input was lost");
            break;
        }
        let (w, h) = (pattern.width, pattern.height);
        match VideoFrame::new(id.clone(), seq, pts, w, h, data.clone()) {
            Ok(frame) => shared.latest.publish(frame),
            Err(e) => {
                shared.status.fail(e.to_string());
                break;
            }
        }
        pool.recycle(data);
        shared.status.advance(SourceState::Running);
        seq += 1;
    }
    shared.status.advance(SourceState::Stopped);
}

impl VideoSource for SyntheticSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn state(&self) -> SourceState {
        self.shared.status.get()
    }

    fn latest(&self) -> Option<VideoFrame> {
        self.shared.latest.get()
    }

    fn wait_newer(&self, after: Option<u64>, timeout: Duration) -> Option<VideoFrame> {
        self.shared.latest.wait_newer(after, timeout)
    }

    fn drain_audio(&self) -> Vec<AudioBlock> {
        Vec::new()
    }

    fn error(&self) -> Option<String> {
        self.shared.status.error()
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.shared.status.fail("synthetic worker panicked");
        }
        self.shared.status.advance(SourceState::Stopped);
    }
}

impl Drop for SyntheticSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[path = "synthetic_tests.rs"]
mod tests;
