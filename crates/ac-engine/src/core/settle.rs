//! The core's side of settle-before-take: the operator's Cut and Auto, and
//! the Auto of an accepted suggestion, go through the
//! [`SettleGate`](crate::settle::SettleGate), which may hold one of them
//! while the preview crop is still travelling, and each tick judges the
//! take it holds once framing has stepped.
//!
//! The rules are [`crate::settle`]'s. This module supplies what the gate
//! needs from the core and does what it decides:
//!
//! * **Settledness** ([`EngineCore::settling`]) is the catalog's
//!   ([`ShotEntry::settled`](crate::catalog::ShotEntry::settled)) for an
//!   available subject shot that is tracking, and nothing for any other
//!   shot: a wide is always settled, a lost shot holds its crop still, and
//!   a shot that cannot go on air is the switcher's to refuse.
//! * **Pressing** ([`EngineCore::press_take`]) runs a take the gate does
//!   not hold through the switcher at once, as without the rule. A press
//!   the switcher refuses changes nothing, the gate included: a refused
//!   second press leaves the first waiting. Cut, Auto and accepting a
//!   suggestion all press this way ([`super::commands`]).
//! * **The late press** ([`EngineCore::late_press`]) of a take that fired
//!   by itself is answered before any fence, and takes nothing.
//! * **The tick** ([`EngineCore::settle_tick`]) runs between framing and
//!   the switcher's own tick, so the settledness judged is this tick's and
//!   a take that fires is advanced and drawn on this tick, as a press
//!   applied before it would be. A take that fires and goes through the
//!   switcher's checks is handed back to the gate, for its late press. A
//!   waiting take whose shot reached
//!   program by other means (the director took it in Auto mode) has
//!   nothing left to take and ends unannounced.
//! * **Forcing** ([`EngineCore::force_waiting_take`]) runs the waiting take
//!   ahead of a shot the operator stages, fenced as on a tick.
//!
//! A take that fires late is still the operator's manual action, but that
//! was applied when it was pressed ([`super::commands`]): the director's
//! takeover follows the press, not the moment the take goes on air.

use ac_core::ShotId;

use super::EngineCore;
use crate::catalog::ShotStatus;
use crate::command::CommandError;
use crate::settle::TakeKind;

impl EngineCore {
    /// The settledness the gate judges `shot` by; see the module docs.
    fn settling(&self, shot: &ShotId) -> Option<f64> {
        self.catalog
            .get(shot)
            .filter(|e| e.available && e.status == ShotStatus::Tracking)
            .map(|e| e.settled)
    }

    /// The preview bus and its settledness, as the gate takes them.
    fn preview_settling(&self) -> (Option<ShotId>, Option<f64>) {
        let preview = self.switcher.preview().cloned();
        let settling = preview.as_ref().and_then(|p| self.settling(p));
        (preview, settling)
    }

    /// The operator's take of `kind`, whose fence has passed: held by the
    /// gate, or run at once (see the module docs).
    pub(super) fn press_take(&mut self, kind: TakeKind) -> Result<(), CommandError> {
        let before = self.settle.clone();
        let (preview, settling) = self.preview_settling();
        let idle = !self.switcher.in_transition();
        if self.settle.press(kind, preview.as_ref(), settling, idle) {
            return Ok(());
        }
        let result = self.run_take(kind);
        if result.is_err() {
            self.settle = before;
        }
        result
    }

    /// Runs a take of `kind` through the switcher's checks.
    fn run_take(&mut self, kind: TakeKind) -> Result<(), CommandError> {
        let catalog = &self.catalog;
        let available = |id: &ShotId| catalog.is_available(id);
        match kind {
            TakeKind::Cut => self.switcher.cut(available)?,
            TakeKind::Auto => self.switcher.auto(available)?,
        }
        Ok(())
    }

    /// Runs `kind` for the gate, which counts a refusal as a dropped take;
    /// true when it went through.
    fn fire(&mut self, kind: TakeKind) -> bool {
        match self.run_take(kind) {
            Ok(()) => true,
            Err(reason) => {
                self.settle.refused(kind, reason);
                false
            }
        }
    }

    /// True when the operator's Cut (`cut`), Auto or Accept is the late
    /// second press of a take that fired by itself, and takes nothing
    /// ([`crate::settle::SettleGate::late_press`]); a Cut while that take's
    /// mix still runs is not, and lands the mix, as a Cut during any mix
    /// does.
    pub(super) fn late_press(&self, cut: bool) -> bool {
        let switcher = &self.switcher;
        let mixing = switcher.in_transition();
        self.settle
            .late_press(switcher.program(), switcher.preview(), mixing)
            && !(cut && mixing)
    }

    /// Advances the waiting take by the tick's `dt` and fires it when the
    /// gate says so (see the module docs).
    pub(super) fn settle_tick(&mut self, dt: f64) {
        let waiting = self.settle.waiting().map(|w| w.shot.clone());
        if let Some(shot) = &waiting
            && self.switcher.program() == Some(shot)
            && !self.switcher.in_transition()
        {
            self.settle.cancel();
            return;
        }
        let (preview, settling) = self.preview_settling();
        if let Some((kind, _)) = self.settle.tick(dt, preview.as_ref(), settling)
            && let Some(shot) = waiting
            && self.fire(kind)
        {
            self.settle.went_on_air(shot);
        }
    }

    /// Runs the waiting take now, ahead of a shot the operator stages,
    /// unless preview changed since it was pressed.
    pub(super) fn force_waiting_take(&mut self) {
        let preview = self.switcher.preview().cloned();
        if let Some(kind) = self.settle.force(preview.as_ref()) {
            self.fire(kind);
        }
    }
}
