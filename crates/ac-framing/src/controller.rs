//! Per-shot framing controller.
//!
//! Each shot owns a [`ShotFramer`] that turns a stream of ideal crops into a
//! smooth crop path. [`ShotFramer::aim`] makes the ideal crop for a frame
//! from the subject's box and extent ([`compose_subject`]), and
//! [`ShotFramer::step`] follows it:
//!
//! * **Head memory.** The framer owns its shot's [`HeadMemory`] and is its
//!   only writer: every person box passed to `aim` is folded in (smoothed
//!   with [`FramerConfig::head_half_life`]; a box not measured on the canon
//!   can only raise it, toward what the box shows). A measured box is framed
//!   on its own proportions, so its Full is the whole box; the memory sizes
//!   only the other boxes, so a person whose legs go behind a lectern keeps
//!   their zoom instead of falling back to the fill rule (see
//!   [`crate::person`]). In a separate slot it smooths, with the same
//!   half-life, how many head heights a box cut by the bottom of the picture
//!   is assumed to show: a speaker never seen whole is framed down from the
//!   crown on that bounded guess, steady through gestures, a walk to the side
//!   of the picture and boxes that end just above the bottom edge, until a
//!   box ends clear of the release band
//!   ([`HeadMemory::assumed_heads_in_force`]). The guess never becomes a
//!   remembered head. The memory lives and dies with the shot.
//! * **Dead zone with hysteresis.** The camera starts moving only when the
//!   ideal centre leaves an engage band (a fraction of the current crop size)
//!   and stops once the error is below a quarter of that band. Small sway gives
//!   a static shot, and the camera does not twitch at the band's edge.
//! * **Springs.** Centre x, centre y and log-width each follow their goal with
//!   a [`Spring`]. Zoom works in log space so zooming in and out by the same
//!   ratio feels equally fast.
//! * **Loss.** When no observation arrives, the shot holds its last framing
//!   instead of drifting.
//! * **Zoom honesty.** [`ShotFramer::current`] says whether the shot is held
//!   back by its zoom ceiling ([`Composition::limited_by_zoom`]), held while
//!   the subject is unobserved. The verdict has hysteresis: it turns on when
//!   a target needs [`LIMIT_HYSTERESIS`] (5%) more zoom than the ceiling
//!   allows ([`Composition::zoom_fit`] below 0.95) and off when a target
//!   fits with 5% to spare (above 1.05); in between it keeps its last value.
//!   Box jitter from the tracker's scale search (±5% steps) or a new
//!   detection near the ceiling therefore does not flip the "(limited)"
//!   label on and off. A new framer starts from its initial crop's exact
//!   verdict.

use ac_core::{Extent, Point, Preset, Rect};
use serde::{Deserialize, Serialize};

use crate::compose::{Composition, SubjectGeometry, compose_subject};
use crate::person::HeadMemory;
use crate::spring::Spring;

/// Tuning for a [`ShotFramer`].
///
/// Defaults follow the rebuild specification's framing ranges.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FramerConfig {
    /// Half-life of pan and tilt, in seconds.
    pub pan_half_life: f64,
    /// Half-life of zoom, in seconds.
    pub zoom_half_life: f64,
    /// Peak pan/tilt speed in crop widths per second.
    pub max_pan_speed: f64,
    /// Peak zoom speed in natural-log width per second.
    pub max_zoom_speed: f64,
    /// Horizontal engage band as a fraction of crop width.
    pub dead_zone_x: f64,
    /// Vertical engage band as a fraction of crop height.
    pub dead_zone_y: f64,
    /// Zoom engage band as a fraction of crop width.
    pub dead_zone_zoom: f64,
    /// Half-life of a shot's remembered head height, in seconds. Long, so
    /// box jitter, raised hands and an instrument above the head barely move
    /// the scale (an inferred default; tune on footage).
    #[serde(default = "default_head_half_life")]
    pub head_half_life: f64,
}

/// Default [`FramerConfig::head_half_life`], in seconds.
const HEAD_HALF_LIFE: f64 = 5.0;

/// [`HEAD_HALF_LIFE`], for serde.
fn default_head_half_life() -> f64 {
    HEAD_HALF_LIFE
}

impl Default for FramerConfig {
    fn default() -> Self {
        Self {
            pan_half_life: 0.45,
            zoom_half_life: 0.8,
            max_pan_speed: 0.4,
            max_zoom_speed: 0.25,
            dead_zone_x: 0.08,
            dead_zone_y: 0.05,
            dead_zone_zoom: 0.07,
            head_half_life: HEAD_HALF_LIFE,
        }
    }
}

/// Fraction of the engage band below which a moving axis comes to rest.
const RELEASE_RATIO: f64 = 0.25;

/// Relative band around the zoom ceiling in which a shot's "limited by
/// zoom" verdict keeps its last value; see the module docs.
pub const LIMIT_HYSTERESIS: f64 = 0.05;

/// One axis with dead-zone hysteresis.
#[derive(Debug, Clone, Copy)]
struct Axis {
    /// The critically damped spring that moves the value.
    spring: Spring,
    /// Where the spring is heading; it follows the target only while moving.
    goal: f64,
    /// The target left the dead band and the axis is travelling to it.
    moving: bool,
}

impl Axis {
    /// An axis at rest at `value`.
    fn new(value: f64, half_life: f64, max_speed: f64) -> Self {
        Self {
            spring: Spring::new(value, half_life, max_speed),
            goal: value,
            moving: false,
        }
    }

    /// Updates the goal if the target left the band, then steps the spring.
    fn step(&mut self, target: Option<f64>, band: f64, max_speed: f64, dt: f64) {
        if let Some(target) = target {
            let error = (target - self.spring.value).abs();
            if !self.moving && error > band {
                self.moving = true;
            }
            if self.moving {
                self.goal = target;
            }
        }
        self.spring.max_speed = max_speed;
        self.spring.step(self.goal, dt);
        if self.moving && (self.goal - self.spring.value).abs() < band * RELEASE_RATIO {
            self.moving = false;
        }
    }
}

/// Smooth framing state of one shot.
#[derive(Debug, Clone)]
pub struct ShotFramer {
    /// Tuning.
    config: FramerConfig,
    /// Width divided by height of the source picture.
    source_aspect: f64,
    /// Width divided by height of the output picture.
    output_aspect: f64,
    /// Horizontal centre of the crop.
    x: Axis,
    /// Vertical centre of the crop.
    y: Axis,
    /// Natural log of the crop width, so zoom moves evenly in scale.
    log_w: Axis,
    /// The shot is held back by its zoom ceiling, with hysteresis; see the
    /// module docs.
    limited_by_zoom: bool,
    /// [`Composition::zoom_fit`] of the newest target.
    zoom_fit: f64,
    /// The shot's remembered head height and assumed head count; written
    /// only by [`Self::aim`].
    head: HeadMemory,
}

impl ShotFramer {
    /// A framer resting at `initial`.
    pub fn new(
        config: FramerConfig,
        initial: Composition,
        source_aspect: f64,
        output_aspect: f64,
    ) -> Self {
        Self {
            config,
            source_aspect,
            output_aspect,
            x: Axis::new(initial.center.x, config.pan_half_life, f64::INFINITY),
            y: Axis::new(initial.center.y, config.pan_half_life, f64::INFINITY),
            log_w: Axis::new(
                initial.width.ln(),
                config.zoom_half_life,
                config.max_zoom_speed,
            ),
            limited_by_zoom: initial.limited_by_zoom,
            zoom_fit: initial.zoom_fit,
            head: HeadMemory::new(),
        }
    }

    /// The ideal crop for a subject seen at `bbox` with `extent`, `dt`
    /// seconds after the previous call, under `preset`, for this framer's
    /// aspect ratios.
    ///
    /// A person box first updates the shot's head memory, which then sizes
    /// the shot only if the box is not measured on the canon
    /// ([`compose_subject`]): the remembered head first, then, for a box cut
    /// by the bottom or read as cut by it, the assumed head count in force
    /// ([`HeadMemory::assumed_heads_in_force`]). Objects never touch the
    /// memory.
    pub fn aim(&mut self, bbox: &Rect, extent: Extent, preset: &Preset, dt: f64) -> Composition {
        let (remembered_head, assumed_heads) = match extent {
            Extent::Person => {
                let half_life = self.config.head_half_life;
                let head = self.head.observe(bbox, self.source_aspect, half_life, dt);
                (head, self.head.assumed_heads_in_force())
            }
            Extent::Object => (None, None),
        };
        let subject = SubjectGeometry {
            bbox: *bbox,
            extent,
            remembered_head,
            assumed_heads,
        };
        compose_subject(&subject, preset, self.source_aspect, self.output_aspect)
    }

    /// The shot's remembered head height, as a fraction of the source
    /// height, once a person box could be measured.
    pub fn head_height(&self) -> Option<f64> {
        self.head.get()
    }

    /// The current crop.
    pub fn current(&self) -> Composition {
        Composition {
            center: Point {
                x: self.x.spring.value,
                y: self.y.spring.value,
            },
            width: self.log_w.spring.value.exp(),
            limited_by_zoom: self.limited_by_zoom,
            zoom_fit: self.zoom_fit,
        }
    }

    /// Advances by `dt` seconds. `target` is the ideal crop for this frame, or
    /// `None` when the subject was not observed.
    pub fn step(&mut self, target: Option<Composition>, dt: f64) -> Composition {
        if let Some(t) = target {
            self.zoom_fit = t.zoom_fit;
            if t.zoom_fit < 1.0 - LIMIT_HYSTERESIS {
                self.limited_by_zoom = true;
            } else if t.zoom_fit > 1.0 + LIMIT_HYSTERESIS {
                self.limited_by_zoom = false;
            }
        }
        let width = self.current().width;
        let height = width * self.source_aspect / self.output_aspect;
        let pan_speed = self.config.max_pan_speed * width;
        let c = &self.config;
        self.x.step(
            target.map(|t| t.center.x),
            c.dead_zone_x * width,
            pan_speed,
            dt,
        );
        self.y.step(
            target.map(|t| t.center.y),
            c.dead_zone_y * height,
            pan_speed,
            dt,
        );
        self.log_w.step(
            target.map(|t| t.width.ln()),
            (1.0 + c.dead_zone_zoom).ln(),
            c.max_zoom_speed,
            dt,
        );
        self.current()
    }
}

#[cfg(test)]
mod tests;
