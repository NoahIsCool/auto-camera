//! Per-shot framing controller.
//!
//! Each shot owns a [`ShotFramer`] that turns a stream of ideal crops into a
//! smooth crop path. [`ShotFramer::aim`] makes the ideal crop for a frame
//! from the subject's box and extent ([`compose_subject`]), and
//! [`ShotFramer::step`] follows it:
//!
//! * **Head memory.** The framer owns its shot's [`HeadMemory`] and is its
//!   only writer: every person box passed to `aim` is folded in (untruncated
//!   ones only, smoothed with [`FramerConfig::head_half_life`]), and the
//!   remembered head height sizes the shot, so a person whose legs go behind
//!   a lectern keeps their zoom instead of falling back to the fill rule
//!   (see [`crate::person`]). The memory lives and dies with the shot.
//! * **Dead zone with hysteresis.** The camera starts moving only when the
//!   ideal centre leaves an engage band (a fraction of the current crop size)
//!   and stops once the error is below a quarter of that band. Small sway gives
//!   a static shot, and the camera does not twitch at the band's edge.
//! * **Springs.** Centre x, centre y and log-width each follow their goal with
//!   a [`Spring`]. Zoom works in log space so zooming in and out by the same
//!   ratio feels equally fast.
//! * **Loss.** When no observation arrives, the shot holds its last framing
//!   instead of drifting.
//! * **Zoom honesty.** [`ShotFramer::current`] carries the newest target's
//!   [`Composition::limited_by_zoom`], held while the subject is unobserved,
//!   so the shot is labelled as limited for exactly as long as it is.

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
    /// The newest target could not reach its size within the zoom ceiling.
    limited_by_zoom: bool,
    /// The shot's remembered head height; written only by [`Self::aim`].
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
            head: HeadMemory::new(),
        }
    }

    /// The ideal crop for a subject seen at `bbox` with `extent`, `dt`
    /// seconds after the previous call, under `preset`, for this framer's
    /// aspect ratios.
    ///
    /// A person box first updates the shot's head memory; the remembered
    /// head height then sizes the shot ([`compose_subject`]). Objects never
    /// touch the memory.
    pub fn aim(&mut self, bbox: &Rect, extent: Extent, preset: &Preset, dt: f64) -> Composition {
        let head_height = match extent {
            Extent::Person => {
                let half_life = self.config.head_half_life;
                self.head.observe(bbox, self.source_aspect, half_life, dt)
            }
            Extent::Object => None,
        };
        let subject = SubjectGeometry {
            bbox: *bbox,
            extent,
            head_height,
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
        }
    }

    /// Advances by `dt` seconds. `target` is the ideal crop for this frame, or
    /// `None` when the subject was not observed.
    pub fn step(&mut self, target: Option<Composition>, dt: f64) -> Composition {
        if let Some(t) = target {
            self.limited_by_zoom = t.limited_by_zoom;
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
mod tests {
    use ac_core::ShotSize;

    use super::*;

    const A: f64 = 16.0 / 9.0;

    fn at(x: f64, y: f64, width: f64) -> Composition {
        Composition {
            center: Point { x, y },
            width,
            limited_by_zoom: false,
        }
    }

    fn run(framer: &mut ShotFramer, target: Composition, seconds: f64) -> Composition {
        let dt = 1.0 / 60.0;
        let mut out = framer.current();
        for _ in 0..(seconds / dt) as usize {
            out = framer.step(Some(target), dt);
        }
        out
    }

    #[test]
    fn small_sway_inside_dead_zone_keeps_shot_static() {
        let start = at(0.5, 0.5, 0.5);
        let mut f = ShotFramer::new(FramerConfig::default(), start, A, A);
        let out = run(&mut f, at(0.5 + 0.03 * 0.5, 0.5, 0.5), 2.0);
        assert_eq!(out, start);
    }

    #[test]
    fn large_move_is_followed_and_settles() {
        let mut f = ShotFramer::new(FramerConfig::default(), at(0.3, 0.5, 0.5), A, A);
        let out = run(&mut f, at(0.7, 0.5, 0.5), 6.0);
        assert!((out.center.x - 0.7).abs() < 0.5 * 0.08 * RELEASE_RATIO + 1e-9);
    }

    #[test]
    fn pan_speed_never_exceeds_cap() {
        let config = FramerConfig::default();
        let mut f = ShotFramer::new(config, at(0.2, 0.5, 0.5), A, A);
        let dt = 1.0 / 60.0;
        let mut previous = f.current().center.x;
        for _ in 0..240 {
            let x = f.step(Some(at(0.8, 0.5, 0.5)), dt).center.x;
            assert!((x - previous).abs() / dt <= config.max_pan_speed * 0.5 + 1e-9);
            previous = x;
        }
    }

    #[test]
    fn zoom_follows_in_log_space() {
        let mut f = ShotFramer::new(FramerConfig::default(), at(0.5, 0.5, 0.8), A, A);
        let out = run(&mut f, at(0.5, 0.5, 0.4), 10.0);
        assert!((out.width / 0.4 - 1.0).abs() < 0.03);
    }

    #[test]
    fn missing_observations_hold_the_framing() {
        let start = at(0.4, 0.5, 0.5);
        let mut f = ShotFramer::new(FramerConfig::default(), start, A, A);
        for _ in 0..120 {
            f.step(None, 1.0 / 60.0);
        }
        assert_eq!(f.current(), start);
    }

    /// A whole person half the frame high, legs in view.
    fn standing() -> Rect {
        Rect::new(0.45, 0.3, 0.1, 0.5)
    }

    /// The same person with the legs hidden: the box ends at the frame
    /// bottom.
    fn behind_lectern() -> Rect {
        Rect::new(0.45, 0.3, 0.1, 0.7)
    }

    /// A close preset that can reach its size on `standing()`.
    fn close() -> Preset {
        Preset {
            max_zoom: 20.0,
            ..Preset::new("c", "Close", ShotSize::Close)
        }
    }

    /// A framer for `close()` at rest on its first target.
    fn framer() -> ShotFramer {
        ShotFramer::new(FramerConfig::default(), at(0.5, 0.5, 1.0), A, A)
    }

    #[test]
    fn person_boxes_seed_then_smooth_the_head_memory() {
        let mut f = framer();
        assert_eq!(f.head_height(), None);
        f.aim(&standing(), Extent::Person, &close(), 0.0);
        assert!((f.head_height().unwrap() - 0.5 / 7.5).abs() < 1e-12);
        // A taller box (walking toward the camera) moves the memory half
        // way in one half-life, in any number of steps.
        let taller = Rect::new(0.45, 0.2, 0.12, 0.7);
        let half_life = FramerConfig::default().head_half_life;
        for _ in 0..50 {
            f.aim(&taller, Extent::Person, &close(), half_life / 50.0);
        }
        let halfway = (0.5 / 7.5 + 0.7 / 7.5) / 2.0;
        assert!((f.head_height().unwrap() - halfway).abs() < 1e-9);
    }

    #[test]
    fn a_lectern_hiding_the_legs_keeps_the_zoom() {
        let mut f = framer();
        let seen = f.aim(&standing(), Extent::Person, &close(), 1.0 / 30.0);
        let hidden = f.aim(&behind_lectern(), Extent::Person, &close(), 1.0 / 30.0);
        assert!((hidden.width - seen.width).abs() < 1e-12);
        assert!((hidden.center.y - seen.center.y).abs() < 1e-12);
        assert!((f.head_height().unwrap() - 0.5 / 7.5).abs() < 1e-12);
        // A shot that never saw the legs has nothing to remember: fill rule.
        let mut fresh = framer();
        let fallback = fresh.aim(&behind_lectern(), Extent::Person, &close(), 1.0 / 30.0);
        assert_eq!(fresh.head_height(), None);
        assert_eq!(fallback, crate::compose(&behind_lectern(), &close(), A, A));
    }

    #[test]
    fn objects_never_touch_the_memory() {
        let mut f = framer();
        let target = f.aim(&standing(), Extent::Object, &close(), 1.0 / 30.0);
        assert_eq!(f.head_height(), None);
        assert_eq!(target, crate::compose(&standing(), &close(), A, A));
    }

    #[test]
    fn bad_time_steps_never_poison_the_memory() {
        let mut f = framer();
        f.aim(&standing(), Extent::Person, &close(), 1.0 / 30.0);
        let bigger = Rect::new(0.4, 0.1, 0.2, 0.8);
        for dt in [f64::NAN, -1.0, 0.0] {
            f.aim(&bigger, Extent::Person, &close(), dt);
            assert!((f.head_height().unwrap() - 0.5 / 7.5).abs() < 1e-12, "{dt}");
        }
        let target = f.aim(&bigger, Extent::Person, &close(), f64::INFINITY);
        assert!((f.head_height().unwrap() - 0.8 / 7.5).abs() < 1e-12);
        assert!(target.width.is_finite() && target.center.y.is_finite());
    }

    #[test]
    fn the_zoom_flag_follows_the_target_and_holds_while_unobserved() {
        let mut f = framer();
        let capped = Preset::new("c", "Close", ShotSize::Close);
        let target = f.aim(&standing(), Extent::Person, &capped, 1.0 / 30.0);
        assert!(target.limited_by_zoom, "a close-up of this person needs 9x");
        assert!(f.step(Some(target), 1.0 / 30.0).limited_by_zoom);
        assert!(f.step(None, 1.0 / 30.0).limited_by_zoom, "held");
        let reachable = f.aim(&standing(), Extent::Person, &close(), 1.0 / 30.0);
        assert!(!reachable.limited_by_zoom);
        assert!(!f.step(Some(reachable), 1.0 / 30.0).limited_by_zoom);
    }
}
