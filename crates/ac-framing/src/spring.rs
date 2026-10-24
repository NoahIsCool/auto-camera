//! Critically damped springs.
//!
//! A critically damped spring moves a value `x` towards a goal `g` as fast as
//! possible without overshoot. With `y = 2 ln 2 / half_life` the closed-form
//! solution for a constant goal over a step `dt` is
//!
//! ```text
//! j0 = x - g
//! j1 = v + j0 * y
//! x' = g + e^(-y dt) (j0 + j1 dt)
//! v' = e^(-y dt) (v - j1 y dt)
//! ```
//!
//! which is exact for any `dt`, so frame-rate changes do not change the motion.
//! `half_life` is the time for the remaining error to halve once the spring is
//! moving. An optional speed cap limits how far one step may move, which keeps
//! long reframes at a steady, broadcast-like pace.

use serde::{Deserialize, Serialize};

/// A one-dimensional critically damped spring.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spring {
    /// Current value.
    pub value: f64,
    /// Current velocity, in units per second.
    pub velocity: f64,
    /// Seconds for the remaining error to halve.
    pub half_life: f64,
    /// Largest speed in units per second, or `f64::INFINITY` for no cap.
    pub max_speed: f64,
}

impl Spring {
    /// A spring at rest at `value`.
    pub fn new(value: f64, half_life: f64, max_speed: f64) -> Self {
        Self {
            value,
            velocity: 0.0,
            half_life,
            max_speed,
        }
    }

    /// Advances the spring by `dt` seconds towards `goal`.
    pub fn step(&mut self, goal: f64, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        if self.half_life <= 0.0 {
            self.velocity = 0.0;
            self.value = goal;
            return;
        }
        let y = 2.0 * std::f64::consts::LN_2 / self.half_life;
        let j0 = self.value - goal;
        let j1 = self.velocity + j0 * y;
        let decay = (-y * dt).exp();
        let next = goal + decay * (j0 + j1 * dt);
        let next_velocity = decay * (self.velocity - j1 * y * dt);

        let max_step = self.max_speed * dt;
        let step = next - self.value;
        if step.abs() > max_step {
            self.value += max_step.copysign(step);
            self.velocity = self.max_speed.copysign(step);
        } else {
            self.value = next;
            self.velocity = next_velocity;
        }
    }

    /// Puts the spring at rest at `value`.
    pub fn reset(&mut self, value: f64) {
        self.value = value;
        self.velocity = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_halves_after_one_half_life_from_rest_roughly() {
        let mut s = Spring::new(0.0, 0.5, f64::INFINITY);
        for _ in 0..30 {
            s.step(1.0, 1.0 / 60.0);
        }
        // From rest a critically damped spring starts slowly, so after one
        // half-life the error is between 50% and 80% of the original.
        let error = 1.0 - s.value;
        assert!(error > 0.3 && error < 0.8, "error {error}");
    }

    #[test]
    fn never_overshoots_a_constant_goal() {
        let mut s = Spring::new(0.0, 0.3, f64::INFINITY);
        for _ in 0..600 {
            s.step(1.0, 1.0 / 60.0);
            assert!(s.value <= 1.0 + 1e-12);
        }
        assert!((s.value - 1.0).abs() < 1e-6);
    }

    #[test]
    fn result_does_not_depend_on_frame_rate() {
        let mut a = Spring::new(0.0, 0.4, f64::INFINITY);
        let mut b = a;
        for _ in 0..30 {
            a.step(1.0, 1.0 / 30.0);
        }
        for _ in 0..60 {
            b.step(1.0, 1.0 / 60.0);
        }
        assert!((a.value - b.value).abs() < 1e-9);
    }

    #[test]
    fn speed_cap_limits_each_step() {
        let mut s = Spring::new(0.0, 0.05, 0.5);
        s.step(10.0, 0.1);
        assert!((s.value - 0.05).abs() < 1e-12);
        assert!((s.velocity - 0.5).abs() < 1e-12);
    }

    #[test]
    fn zero_half_life_snaps_to_goal() {
        let mut s = Spring::new(0.0, 0.0, f64::INFINITY);
        s.step(3.0, 0.01);
        assert_eq!(s.value, 3.0);
    }
}
