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
//! colour opacity, which the compositor renders. It reads no clock: time
//! enters as the `dt` of [`Switcher::tick`], and whether a shot can be shown
//! as the caller's `available` predicate at the moment of each call.
//!
//! # Health
//!
//! Every press that raises a shot's share of the picture checks that shot
//! at the moment it does so: Cut, Cut now ([`Switcher::cut_to`]), Auto, a
//! T-bar move away from rest, and a queued take when it fires (see below).
//! An unavailable shot is never taken: a press is refused
//! ([`SwitchError::Unavailable`]) and changes nothing, and a queued take is
//! dropped. Cutting a transition short raises its incoming shot to the whole
//! picture at once (from nothing, in the first half of a dip), so it checks
//! that shot like any take. Only the transition itself carries on without a
//! re-check: its incoming shot was checked when it started, and the clock or
//! the lever moving it along is not a press.
//!
//! # Pressing during a transition
//!
//! While a transition runs, its incoming shot sits on the preview bus and is
//! already partly on air, so neither bus can change before it ends. Presses
//! that arrive meanwhile are not refused; they wait in a queue of depth one
//! ([`Queued`]), and the latest press wins:
//!
//! * **Staging** a shot ([`Switcher::set_preview`]) queues it: it lands on
//!   preview when the transition ends, in place of the outgoing shot that
//!   preview-follows-program would bring back. Staging another shot replaces
//!   it; staging the incoming shot itself clears the queue.
//! * **Auto** queues a take of the staged shot. When the transition ends and
//!   the shot has landed on preview, Auto runs to it after a re-check that it
//!   is still available. A shot that fails the re-check stays on preview but
//!   is not taken: the take is dropped, counted, and its reason kept
//!   ([`Switcher::last_dropped`]) for the operator. The press is checked too,
//!   so a take that could not fire now is refused at once. Pressing Auto
//!   again changes nothing, and with nothing staged Auto has nothing to add:
//!   the running transition already takes the preview.
//! * **Cut** cuts the running transition short: its incoming shot, the one
//!   the preview monitor shows, lands on program at once, and the old
//!   program shot moves to preview. A shot staged meanwhile lands on preview
//!   in its place, as at any other end, but a take queued of it is
//!   cancelled, not fired: Cut is the operator ending the transition now,
//!   with nothing more to follow. Cut never puts a shot on air that the
//!   preview bus did not show.
//! * **Cut now** ([`Switcher::cut_to`]) puts a named shot on air in one
//!   step, whatever is staged, and the shot it replaces moves to preview (a
//!   shot already on air stays as it is). During a transition it is a Cut
//!   and then, unless the named shot is the incoming one, a cut to it,
//!   without the incoming shot ever showing alone in between. Only the
//!   named shot is checked, since it is the only one that goes on air.
//! * While the T-bar holds the transition, Cut and Cut now are refused
//!   ([`SwitchError::TBarHeld`], [`Switcher::tbar_held`]): the lever would be
//!   left mid-travel, out of step with the buses, and its next move would
//!   jump the picture. The operator finishes or abandons the transition with
//!   the lever. Likewise the T-bar cannot take over an automatic transition
//!   ([`SwitchError::Busy`]), so the lever and the clock never share one.
//!
//! However a transition ends (by the clock, by the T-bar at either end, or by
//! Cut), the queue is resolved at that moment: the staged shot lands on
//! preview, and a queued take fires, is dropped, or (by Cut) is cancelled.
//! Nothing is ever queued while no transition runs, so a queued take fires
//! at most once.
//!
//! # Exposure and tally
//!
//! [`Switcher::exposure`] is the share of the program picture a shot makes
//! up before any dip to black, worked out from the buses: the program shot
//! alone when idle, both shots in proportion during a mix, and during a dip
//! the shot under the black (the outgoing one in the first half, the
//! incoming one in the second). It always agrees with what
//! [`Switcher::output`] draws, which the property tests check. Tally follows
//! exposure, as on hardware switchers ([`Switcher::tally`]): a shot is lit red
//! while it has any exposure, so both sides of a mix are red, and green while
//! it is on preview with none.

use ac_core::ShotId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(test)]
mod proptests;
#[cfg(test)]
mod tests;

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

/// Why a command was refused, or a queued take dropped.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SwitchError {
    /// Nothing is staged on preview.
    #[error("no shot on preview")]
    NoPreview,
    /// The shot cannot be shown right now.
    #[error("shot {0:?} is not available")]
    Unavailable(ShotId),
    /// The T-bar cannot take over an automatic transition in progress.
    #[error("an automatic transition is in progress")]
    Busy,
    /// Cut and Cut now cannot cut short a transition the T-bar holds.
    #[error("the T-bar holds the transition in progress")]
    TBarHeld,
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

/// A press waiting for the running transition to end; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    /// The shot that lands on preview when the transition ends.
    pub shot: ShotId,
    /// Auto runs to the shot once it has landed, if it is still available.
    pub take: bool,
}

/// Tally lamps of a shot; see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Exposed on program (red), including both sides of a running mix.
    pub program: bool,
    /// On preview and not exposed on program (green).
    pub preview: bool,
}

/// A transition in progress.
#[derive(Debug, Clone, PartialEq)]
struct Running {
    /// The transition being run.
    transition: Transition,
    /// Progress in `0.0..=1.0`.
    progress: f64,
    /// True when driven by the T-bar rather than by time.
    manual: bool,
}

/// Preview/program state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Switcher {
    /// The shot on air.
    program: Option<ShotId>,
    /// The staged shot; during a transition, the incoming one.
    preview: Option<ShotId>,
    /// Transition used by the next auto take.
    transition: Transition,
    /// The transition in progress, if any.
    running: Option<Running>,
    /// T-bar travel direction flips after each completed manual transition.
    tbar_reversed: bool,
    /// The press waiting for the running transition to end; always `None`
    /// while no transition runs.
    queued: Option<Queued>,
    /// Queued takes dropped by their re-check.
    dropped: u64,
    /// Why the newest of them was dropped.
    last_dropped: Option<SwitchError>,
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

    /// The staged shot; during a transition, the shot it brings in.
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

    /// True while the T-bar holds a transition, which refuses Cut and Cut
    /// now (see the module docs).
    pub fn tbar_held(&self) -> bool {
        self.running.as_ref().is_some_and(|r| r.manual)
    }

    /// The press waiting for the running transition to end.
    pub fn queued(&self) -> Option<&Queued> {
        self.queued.as_ref()
    }

    /// Queued takes dropped so far because their shot failed the re-check.
    pub fn dropped_takes(&self) -> u64 {
        self.dropped
    }

    /// Why the newest dropped take was dropped.
    pub fn last_dropped(&self) -> Option<&SwitchError> {
        self.last_dropped.as_ref()
    }

    /// Stages `shot` on preview; during a transition, queues it for the
    /// transition's end (see the module docs).
    pub fn set_preview(&mut self, shot: ShotId) {
        if self.running.is_none() {
            self.preview = Some(shot);
        } else if self.preview.as_ref() == Some(&shot) {
            self.queued = None;
        } else {
            let take = self.queued.as_ref().is_some_and(|q| q.take);
            self.queued = Some(Queued { shot, take });
        }
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

    /// The preview shot, if it may be taken now.
    fn checked_preview(&self, available: impl Fn(&ShotId) -> bool) -> Result<ShotId, SwitchError> {
        let shot = self.preview.clone().ok_or(SwitchError::NoPreview)?;
        if !available(&shot) {
            return Err(SwitchError::Unavailable(shot));
        }
        Ok(shot)
    }

    /// Swaps preview and program immediately; during a transition, cuts it
    /// short, landing its incoming shot (see the module docs).
    pub fn cut(&mut self, available: impl Fn(&ShotId) -> bool) -> Result<(), SwitchError> {
        if self.tbar_held() {
            return Err(SwitchError::TBarHeld);
        }
        self.checked_preview(available)?;
        if self.running.is_some() {
            self.cut_short();
        } else {
            self.swap();
        }
        Ok(())
    }

    /// Cuts `shot` straight to program ("Cut now"), whatever is staged; the
    /// old program shot moves to preview. During a transition, cuts it short
    /// first, in the same step (see the module docs).
    pub fn cut_to(
        &mut self,
        shot: ShotId,
        available: impl Fn(&ShotId) -> bool,
    ) -> Result<(), SwitchError> {
        if self.tbar_held() {
            return Err(SwitchError::TBarHeld);
        }
        if !available(&shot) {
            return Err(SwitchError::Unavailable(shot));
        }
        if self.running.is_some() {
            self.cut_short();
        }
        if self.program.as_ref() != Some(&shot) {
            self.preview = self.program.replace(shot);
        }
        Ok(())
    }

    /// Ends the running transition at once with its incoming shot on
    /// program; a shot staged meanwhile lands on preview and a take queued
    /// of it is cancelled.
    fn cut_short(&mut self) {
        self.running = None;
        self.swap();
        if let Some(q) = self.queued.take() {
            self.preview = Some(q.shot);
        }
    }

    /// Starts the selected transition from program to preview; during a
    /// transition, queues a take of the shot staged meanwhile (see the
    /// module docs).
    pub fn auto(&mut self, available: impl Fn(&ShotId) -> bool) -> Result<(), SwitchError> {
        if self.running.is_none() {
            self.checked_preview(available)?;
            self.start();
            return Ok(());
        }
        if let Some(q) = &mut self.queued {
            if !available(&q.shot) {
                return Err(SwitchError::Unavailable(q.shot.clone()));
            }
            q.take = true;
        }
        Ok(())
    }

    /// Moves the T-bar to `position` in `0.0..=1.0`.
    ///
    /// Travel starts from the end where the last manual transition finished, as
    /// on a hardware T-bar. Reaching the far end completes the transition;
    /// returning to the start abandons it with program unchanged. Either end
    /// resolves the queue (see the module docs).
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
                self.checked_preview(&available)?;
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
                self.tbar_reversed = !self.tbar_reversed;
                self.finish(true, available);
            }
            Some(p) if p <= 0.0 => self.finish(false, available),
            _ => {}
        }
        Ok(())
    }

    /// Advances an automatic transition by `dt` seconds; at its end, a take
    /// queued during it fires if `available` passes its shot. A `dt` that is
    /// not a positive number of seconds changes nothing.
    pub fn tick(&mut self, dt: f64, available: impl Fn(&ShotId) -> bool) {
        if !(dt.is_finite() && dt > 0.0) {
            return;
        }
        let Some(running) = &mut self.running else {
            return;
        };
        if running.manual {
            return;
        }
        running.progress += dt / running.transition.duration;
        if running.progress >= 1.0 {
            self.finish(true, available);
        }
    }

    /// Swaps preview and program.
    fn swap(&mut self) {
        std::mem::swap(&mut self.program, &mut self.preview);
    }

    /// Starts the selected transition from program to preview, or swaps at
    /// once when it is a cut.
    fn start(&mut self) {
        if self.transition.kind == TransitionKind::Cut {
            self.swap();
        } else {
            self.running = Some(Running {
                transition: self.transition,
                progress: 0.0,
                manual: false,
            });
        }
    }

    /// Ends the running transition, `completed` (preview and program swap)
    /// or abandoned (they stay), and resolves the queue: the staged shot
    /// lands on preview and a queued take of it fires or is dropped.
    fn finish(&mut self, completed: bool, available: impl Fn(&ShotId) -> bool) {
        self.running = None;
        if completed {
            self.swap();
        }
        let Some(Queued { shot, take }) = self.queued.take() else {
            return;
        };
        let fire = take && available(&shot);
        if take && !fire {
            self.dropped += 1;
            self.last_dropped = Some(SwitchError::Unavailable(shot.clone()));
        }
        self.preview = Some(shot);
        if fire {
            self.start();
        }
    }

    /// The share of the program picture `shot` makes up, in `0.0..=1.0`,
    /// before any dip to black; see the module docs.
    pub fn exposure(&self, shot: &ShotId) -> f64 {
        let (outgoing, incoming) = match &self.running {
            None => (1.0, 0.0),
            Some(running) => {
                let t = ease(running.progress);
                match running.transition.kind {
                    TransitionKind::Mix => (1.0 - t, t),
                    TransitionKind::Dip if t < 0.5 => (1.0, 0.0),
                    TransitionKind::Dip => (0.0, 1.0),
                    TransitionKind::Cut => (1.0, 0.0),
                }
            }
        };
        let on = |bus: &Option<ShotId>, weight: f64| {
            if bus.as_ref() == Some(shot) {
                weight
            } else {
                0.0
            }
        };
        on(&self.program, outgoing) + on(&self.preview, incoming)
    }

    /// The tally lamps of `shot`; see the module docs.
    pub fn tally(&self, shot: &ShotId) -> Tally {
        let program = self.exposure(shot) > 0.0;
        Tally {
            program,
            preview: !program && self.preview.as_ref() == Some(shot),
        }
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
