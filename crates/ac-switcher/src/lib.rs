//! Preview/program switcher.
//!
//! The switcher follows broadcast conventions (as on an ATEM or vMix):
//!
//! * **Program (PGM)** is on air; **Preview (PVW)** is the shot staged next.
//! * **Cut** swaps them instantly. **Auto** runs the selected transition over
//!   its duration. The **T-bar** runs the same transition by hand.
//! * When a transition completes, the old program shot moves to preview
//!   ("preview follows program"), so pressing Auto again returns to it.
//!
//! The switcher only decides *what* to show; [`Switcher::output`] describes
//! the picture as a base shot, an optional overlay with opacity, and a dip
//! colour opacity, which the compositor renders.

use ac_core::ShotId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Transition styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Instant change.
    Cut,
    /// Cross-dissolve.
    Mix,
    /// Fade to black, then up on the new shot.
    Dip,
}

/// A transition style and its duration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    /// Style.
    pub kind: TransitionKind,
    /// Duration in seconds (ignored for [`TransitionKind::Cut`]).
    pub duration: f64,
}

impl Default for Transition {
    fn default() -> Self {
        Self {
            kind: TransitionKind::Mix,
            duration: 0.8,
        }
    }
}

/// Why a command was refused.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SwitchError {
    /// Nothing is staged on preview.
    #[error("no shot on preview")]
    NoPreview,
    /// The preview shot cannot be shown right now.
    #[error("shot {0:?} is not available")]
    Unavailable(ShotId),
    /// A transition is already running.
    #[error("a transition is already in progress")]
    Busy,
    /// The duration is not a positive number of seconds.
    #[error("invalid transition duration {0}")]
    InvalidDuration(f64),
}

/// What the compositor should draw this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    /// Shot drawn first, fully opaque, or `None` for black.
    pub base: Option<ShotId>,
    /// Shot drawn over the base with the given opacity.
    pub overlay: Option<(ShotId, f64)>,
    /// Opacity of black drawn over everything.
    pub dip: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct Running {
    transition: Transition,
    /// Progress in `0.0..=1.0`.
    progress: f64,
    /// True when driven by the T-bar rather than by time.
    manual: bool,
}

/// Preview/program state.
#[derive(Debug, Clone, Default)]
pub struct Switcher {
    program: Option<ShotId>,
    preview: Option<ShotId>,
    transition: Transition,
    running: Option<Running>,
    /// T-bar travel direction flips after each completed manual transition.
    tbar_reversed: bool,
}

/// Smoothstep easing: zero slope at both ends.
fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Switcher {
    /// An empty switcher with the default transition.
    pub fn new() -> Self {
        Self::default()
    }

    /// The shot on air.
    pub fn program(&self) -> Option<&ShotId> {
        self.program.as_ref()
    }

    /// The staged shot.
    pub fn preview(&self) -> Option<&ShotId> {
        self.preview.as_ref()
    }

    /// The selected transition.
    pub fn transition(&self) -> Transition {
        self.transition
    }

    /// True while a transition runs.
    pub fn in_transition(&self) -> bool {
        self.running.is_some()
    }

    /// Stages `shot` on preview. Refused while a transition runs, because the
    /// incoming shot is already on air.
    pub fn set_preview(&mut self, shot: ShotId) -> Result<(), SwitchError> {
        if self.running.is_some() {
            return Err(SwitchError::Busy);
        }
        self.preview = Some(shot);
        Ok(())
    }

    /// Selects the transition used by [`auto`](Self::auto) and the T-bar.
    pub fn set_transition(&mut self, transition: Transition) -> Result<(), SwitchError> {
        if transition.kind != TransitionKind::Cut
            && !(transition.duration.is_finite() && transition.duration > 0.0)
        {
            return Err(SwitchError::InvalidDuration(transition.duration));
        }
        self.transition = transition;
        Ok(())
    }

    fn checked_preview(&self, available: impl Fn(&ShotId) -> bool) -> Result<ShotId, SwitchError> {
        if self.running.is_some() {
            return Err(SwitchError::Busy);
        }
        let shot = self.preview.clone().ok_or(SwitchError::NoPreview)?;
        if !available(&shot) {
            return Err(SwitchError::Unavailable(shot));
        }
        Ok(shot)
    }

    /// Swaps preview and program immediately.
    pub fn cut(&mut self, available: impl Fn(&ShotId) -> bool) -> Result<(), SwitchError> {
        self.checked_preview(available)?;
        self.complete();
        Ok(())
    }

    /// Starts the selected transition from program to preview.
    pub fn auto(&mut self, available: impl Fn(&ShotId) -> bool) -> Result<(), SwitchError> {
        self.checked_preview(available)?;
        if self.transition.kind == TransitionKind::Cut {
            self.complete();
        } else {
            self.running = Some(Running {
                transition: self.transition,
                progress: 0.0,
                manual: false,
            });
        }
        Ok(())
    }

    /// Moves the T-bar to `position` in `0.0..=1.0`.
    ///
    /// Travel starts from the end where the last manual transition finished, as
    /// on a hardware T-bar. Reaching the far end completes the transition;
    /// returning to the start abandons it with program unchanged.
    pub fn set_tbar(
        &mut self,
        position: f64,
        available: impl Fn(&ShotId) -> bool,
    ) -> Result<(), SwitchError> {
        let position = position.clamp(0.0, 1.0);
        let progress = if self.tbar_reversed {
            1.0 - position
        } else {
            position
        };
        match &mut self.running {
            Some(running) if running.manual => running.progress = progress,
            Some(_) => return Err(SwitchError::Busy),
            None if progress > 0.0 => {
                self.checked_preview(available)?;
                let kind = match self.transition.kind {
                    TransitionKind::Cut => TransitionKind::Mix,
                    kind => kind,
                };
                let transition = Transition {
                    kind,
                    ..self.transition
                };
                self.running = Some(Running {
                    transition,
                    progress,
                    manual: true,
                });
            }
            None => return Ok(()),
        }
        match self.running.as_ref().map(|r| r.progress) {
            Some(p) if p >= 1.0 => {
                self.complete();
                self.tbar_reversed = !self.tbar_reversed;
            }
            Some(p) if p <= 0.0 => self.running = None,
            _ => {}
        }
        Ok(())
    }

    /// Advances an automatic transition by `dt` seconds.
    pub fn tick(&mut self, dt: f64) {
        let Some(running) = &mut self.running else {
            return;
        };
        if running.manual {
            return;
        }
        running.progress += dt / running.transition.duration;
        if running.progress >= 1.0 {
            self.complete();
        }
    }

    fn complete(&mut self) {
        self.running = None;
        std::mem::swap(&mut self.program, &mut self.preview);
    }

    /// What to draw this frame.
    pub fn output(&self) -> Output {
        let Some(running) = &self.running else {
            return Output {
                base: self.program.clone(),
                overlay: None,
                dip: 0.0,
            };
        };
        let t = ease(running.progress);
        match running.transition.kind {
            TransitionKind::Cut => Output {
                base: self.program.clone(),
                overlay: None,
                dip: 0.0,
            },
            TransitionKind::Mix => Output {
                base: self.program.clone(),
                overlay: self.preview.clone().map(|s| (s, t)),
                dip: 0.0,
            },
            TransitionKind::Dip if t < 0.5 => Output {
                base: self.program.clone(),
                overlay: None,
                dip: t * 2.0,
            },
            TransitionKind::Dip => Output {
                base: self.preview.clone(),
                overlay: None,
                dip: (1.0 - t) * 2.0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> ShotId {
        ShotId(s.into())
    }

    fn all(_: &ShotId) -> bool {
        true
    }

    fn staged() -> Switcher {
        let mut s = Switcher::new();
        s.set_preview(id("wide")).unwrap();
        s.cut(all).unwrap();
        s.set_preview(id("pastor")).unwrap();
        s
    }

    #[test]
    fn cut_swaps_program_and_preview() {
        let mut s = staged();
        s.cut(all).unwrap();
        assert_eq!(s.program(), Some(&id("pastor")));
        assert_eq!(s.preview(), Some(&id("wide")));
    }

    #[test]
    fn take_without_preview_is_refused() {
        let mut s = Switcher::new();
        assert_eq!(s.cut(all), Err(SwitchError::NoPreview));
    }

    #[test]
    fn unavailable_shot_is_never_taken() {
        let mut s = staged();
        assert_eq!(
            s.auto(|_| false),
            Err(SwitchError::Unavailable(id("pastor")))
        );
        assert_eq!(s.program(), Some(&id("wide")));
    }

    #[test]
    fn mix_blends_then_completes_after_duration() {
        let mut s = staged();
        s.auto(all).unwrap();
        s.tick(0.4);
        let out = s.output();
        assert_eq!(out.base, Some(id("wide")));
        let (shot, alpha) = out.overlay.unwrap();
        assert_eq!(shot, id("pastor"));
        assert!((alpha - 0.5).abs() < 1e-9);
        s.tick(0.41);
        assert!(!s.in_transition());
        assert_eq!(s.program(), Some(&id("pastor")));
    }

    #[test]
    fn dip_goes_through_black_at_midpoint() {
        let mut s = staged();
        s.set_transition(Transition {
            kind: TransitionKind::Dip,
            duration: 1.0,
        })
        .unwrap();
        s.auto(all).unwrap();
        s.tick(0.5);
        assert!(s.output().dip > 0.99);
        s.tick(0.3);
        assert_eq!(s.output().base, Some(id("pastor")));
    }

    #[test]
    fn preview_is_locked_during_transition() {
        let mut s = staged();
        s.auto(all).unwrap();
        assert_eq!(s.set_preview(id("band")), Err(SwitchError::Busy));
        assert_eq!(s.auto(all), Err(SwitchError::Busy));
    }

    #[test]
    fn tbar_completes_and_reverses_direction() {
        let mut s = staged();
        s.set_tbar(0.5, all).unwrap();
        assert!(s.in_transition());
        s.set_tbar(1.0, all).unwrap();
        assert_eq!(s.program(), Some(&id("pastor")));
        // The bar now sits at 1.0; pulling it back towards 0 takes the next shot.
        s.set_tbar(0.4, all).unwrap();
        assert!(s.in_transition());
        s.set_tbar(0.0, all).unwrap();
        assert_eq!(s.program(), Some(&id("wide")));
    }

    #[test]
    fn tbar_returned_to_start_abandons_transition() {
        let mut s = staged();
        s.set_tbar(0.3, all).unwrap();
        s.set_tbar(0.0, all).unwrap();
        assert!(!s.in_transition());
        assert_eq!(s.program(), Some(&id("wide")));
    }

    #[test]
    fn invalid_duration_is_rejected() {
        let mut s = Switcher::new();
        let bad = Transition {
            kind: TransitionKind::Mix,
            duration: 0.0,
        };
        assert_eq!(
            s.set_transition(bad),
            Err(SwitchError::InvalidDuration(0.0))
        );
    }
}
