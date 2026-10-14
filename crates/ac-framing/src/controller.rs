//! Per-shot framing controller.
//!
//! Each shot owns a [`ShotFramer`] that turns a stream of ideal crops (from
//! [`compose`](crate::compose)) into a smooth crop path:
//!
//! * **Dead zone with hysteresis.** The camera starts moving only when the
//!   ideal centre leaves an engage band (a fraction of the current crop size)
//!   and stops once the error is below a quarter of that band. Small sway gives
//!   a static shot, and the camera does not twitch at the band's edge.
//! * **Springs.** Centre x, centre y and log-width each follow their goal with
//!   a [`Spring`]. Zoom works in log space so zooming in and out by the same
//!   ratio feels equally fast.
//! * **Loss.** When no observation arrives, the shot holds its last framing
//!   instead of drifting.

use ac_core::Point;
use serde::{Deserialize, Serialize};

use crate::compose::Composition;
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
        }
    }
}

/// Fraction of the engage band below which a moving axis comes to rest.
const RELEASE_RATIO: f64 = 0.25;

/// One axis with dead-zone hysteresis.
#[derive(Debug, Clone, Copy)]
struct Axis {
    spring: Spring,
    goal: f64,
    moving: bool,
}

impl Axis {
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
    config: FramerConfig,
    source_aspect: f64,
    output_aspect: f64,
    x: Axis,
    y: Axis,
    log_w: Axis,
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
        }
    }

    /// The current crop.
    pub fn current(&self) -> Composition {
        Composition {
            center: Point {
                x: self.x.spring.value,
                y: self.y.spring.value,
            },
            width: self.log_w.spring.value.exp(),
        }
    }

    /// Advances by `dt` seconds. `target` is the ideal crop for this frame, or
    /// `None` when the subject was not observed.
    pub fn step(&mut self, target: Option<Composition>, dt: f64) -> Composition {
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
    use super::*;

    const A: f64 = 16.0 / 9.0;

    fn at(x: f64, y: f64, width: f64) -> Composition {
        Composition {
            center: Point { x, y },
            width,
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
}
