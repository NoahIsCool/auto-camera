//! Smooth virtual-camera framing.
//!
//! The framing pipeline for one shot runs once per output frame:
//!
//! 1. [`compose`] turns the subject's latest box and the shot's [`Preset`]
//!    into an ideal crop: shot size, headroom, zoom limits and source bounds.
//! 2. [`ShotFramer`] decides whether the camera should move at all (a dead zone
//!    with hysteresis, so a subject that sways slightly gets a static shot),
//!    then follows the ideal crop with critically damped [`Spring`]s on centre
//!    and log-width, with explicit speed caps.
//!
//! Everything here is a pure function of its inputs and `dt`, so it can be
//! tested deterministically and evaluated offline on recorded observations.
//!
//! [`Preset`]: ac_core::Preset

pub mod compose;
pub mod controller;
pub mod spring;

pub use compose::{Composition, compose};
pub use controller::{FramerConfig, ShotFramer};
pub use spring::Spring;
