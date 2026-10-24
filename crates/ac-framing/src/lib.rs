//! Smooth virtual-camera framing.
//!
//! The framing pipeline for one shot runs once per output frame:
//!
//! 1. [`compose_subject`] turns the subject's latest box, what that box
//!    covers ([`Extent`]) and the shot's [`Preset`] into an ideal crop: shot
//!    size, headroom, zoom limits and source bounds. An object box is sized
//!    by how much of the frame it fills ([`compose()`], the fill rule); a
//!    whole-person box is cut at a body line measured in head heights from
//!    the crown ([`person`]). A crop the zoom ceiling keeps from its size
//!    says so ([`Composition::limited_by_zoom`]).
//! 2. [`ShotFramer`] remembers the person's head height for its shot
//!    ([`HeadMemory`]), so a box truncated by a lectern keeps its scale; it
//!    decides whether the camera should move at all (a dead zone with
//!    hysteresis, so a subject that sways slightly gets a static shot), then
//!    follows the ideal crop with critically damped [`Spring`]s on centre
//!    and log-width, with explicit speed caps.
//!
//! Everything here is a pure function of its inputs and `dt`, so it can be
//! tested deterministically and evaluated offline on recorded observations.
//!
//! [`Extent`]: ac_core::Extent
//! [`Preset`]: ac_core::Preset

pub mod compose;
pub mod controller;
pub mod person;
pub mod spring;

pub use compose::{Composition, SubjectGeometry, compose, compose_subject};
pub use controller::{FramerConfig, ShotFramer};
pub use person::HeadMemory;
pub use spring::Spring;
