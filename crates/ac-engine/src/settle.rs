//! Settle-before-take: an operator's take may wait, briefly, for the
//! preview crop to settle (spec V6; C.4: "delay a take ≤0.5 s until the
//! PVW crop settles unless the operator forces it").
//!
//! # Why
//!
//! A subject shot's crop travels to its subject on springs
//! ([`ac_framing`]), and while it does the catalog scores the shot less
//! than settled ([`ShotEntry::settled`](crate::catalog::ShotEntry::settled),
//! 1 on target and falling with the framing error). A take to such a shot
//! puts the end of a camera move on air, which reads as a bad cut. Waiting
//! for the crop to arrive fixes that at the price of a late take, so the
//! wait is short, bounded, and the operator can always cut through it.
//!
//! # Rules
//!
//! With a [`SettleRule`] in force (the `[switcher.settle]` table, off by
//! default; see [`crate::config::SettleSettings`]), the [`SettleGate`]
//! decides for the engine core, which owns it:
//!
//! * **Which takes wait.** Only the operator's: Cut, Auto, and accepting a
//!   suggestion, which stages the suggested shot and runs Auto, so it waits
//!   as Enter does (and while it waits the accepted shot holds preview
//!   against the director's next suggestion, as a shot the operator staged
//!   does). And only when they could go on air now: no transition runs and
//!   preview holds an available subject shot that is tracking, less settled
//!   than `threshold` ([`SettleRule::waits`]). Everything else is as it was
//!   without the rule: a take that would be refused is refused at once, a
//!   wide (always settled) and a lost shot (its crop holds still, so there
//!   is nothing to settle) go at once, and during a transition Cut cuts it
//!   short and Auto queues a take for its end ([`ac_switcher`]). Cut now,
//!   the T-bar and the director's own takes never wait.
//! * **One at a time.** At most one take waits ([`WaitingTake`]); it
//!   carries the shot the operator saw on preview, as a fenced take does
//!   ([`crate::command`]).
//! * **The wait.** Each tick, once framing has stepped, the waiting take
//!   is judged ([`WaitingTake::step`]), in this order: when preview no
//!   longer shows its shot it is dropped, refused as a fenced take is
//!   ([`CommandError::PreviewChanged`]); when the shot is settled, or has
//!   nothing left to settle (its subject was lost, or it is gone), it
//!   fires; when it has waited `max_wait` it fires anyway. It fires as it
//!   was pressed, through the switcher's checks; a take they refuse there
//!   is dropped with their reason. Each drop is counted with the newest
//!   reason ([`SettleGate::dropped`]), so the window can say why, as it
//!   does for a queued take the switcher dropped.
//! * **Time** is the sum of the ticks' `dt`; nothing here reads a clock.
//!   The wait ends on the first tick by which `max_wait` has passed, give
//!   or take [`WAIT_EPSILON`], so the 15 ticks of 1/30 s that make 0.5 s are
//!   not counted one short by rounding. The late press below is timed the
//!   same way.
//! * **Forcing.** A second Cut or Auto of the same shot (Space, Enter, CUT
//!   or AUTO pressed again, or Accept while the director still suggests
//!   it) goes on air at once, as it was pressed: Space after Enter cuts. A
//!   take pressed of another shot (preview changed since the first, and
//!   the new press saw it) replaces the waiting one and is judged afresh.
//! * **The late press.** A take left alone fires by itself within
//!   `max_wait`, which is about how long an operator takes to read "press
//!   again to take now" and do it, so that second press can arrive just
//!   after the take went on air. It must take nothing, and by then it can
//!   name any shot: a cut has swapped the buses, so a window that drew the
//!   swap sends a take of the shot just replaced, which would cut straight
//!   back to it; one that did not yet sends a take of a shot preview no
//!   longer shows, which the fence would refuse, saying nothing was taken,
//!   though the first press's take was; and in Suggest mode the director
//!   can stage its next suggestion as soon as the take's shot is on
//!   program, which a take would put on air within half a second of the
//!   last. So for [`LATE_PRESS`] after a take fired by itself
//!   ([`SettleGate::just_fired`]), while its shot is on program, or coming
//!   in on preview during its mix ([`SettleGate::late_press`]), a Cut, Auto
//!   or Accept is taken as that second press and does nothing, quietly,
//!   whatever its sender saw. The one exception is a Cut while the mix
//!   still runs, which lands it at once, as a Cut during any mix does. Any
//!   staging ends the wait for the late press, and so does any press that
//!   is not one; a take forced by a second press has had it, so the press
//!   after that is a fresh take.
//! * **Other switching.** Staging a shot forces the waiting take first
//!   ([`SettleGate::force`]), still fenced (it is dropped if preview no
//!   longer shows its shot), so a cut pressed and the next shot staged
//!   right after it happen in that order, as without the rule. A double
//!   click on another tile is one of those: its first click stages the
//!   tile, which puts the waiting shot on air, and its second is a Cut now
//!   of the tile, so the waiting shot is on air in between, for as long as
//!   the double click takes, as it would have been from the first press on
//!   without the rule. Cut now, and a T-bar move that starts or moves a
//!   transition, take a shot themselves: the waiting take ends unfired and
//!   unannounced ([`SettleGate::cancel`]), since the operator's later take
//!   decided what goes on air, as the latest press wins in the switcher's
//!   queue. A T-bar move that does nothing (the lever at its rest end with
//!   no transition running) leaves it waiting.
//!
//! Without a rule the gate never holds a take, so every take is immediate,
//! exactly as before the rule existed.

use ac_core::ShotId;

use crate::command::CommandError;

/// Slack on the end of a wait, in seconds; see the module docs.
pub const WAIT_EPSILON: f64 = 1e-6;

/// How long after a take fired by itself a Cut, Auto or Accept is its late
/// second press, in seconds: about the time an operator takes to read
/// "press again to take now" and do it; see the module docs.
pub const LATE_PRESS: f64 = 0.5;

/// When an operator's take waits for preview to settle, and for how long.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettleRule {
    /// Settledness, in `(0, 1]`, at which preview counts as settled.
    pub threshold: f64,
    /// Longest wait, in seconds.
    pub max_wait: f64,
}

impl SettleRule {
    /// True when a take of a shot whose settledness is `settling` waits:
    /// `Some` settledness below the threshold. `None` stands for a shot
    /// with nothing to settle (a wide, a lost subject, a shot that cannot
    /// go on air), which never waits.
    pub fn waits(&self, settling: Option<f64>) -> bool {
        settling.is_some_and(|s| s < self.threshold)
    }
}

/// The take that waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeKind {
    /// Swap preview and program at once.
    Cut,
    /// Run the selected transition.
    Auto,
}

impl TakeKind {
    /// The name of the command that pressed it, as
    /// [`EngineCommand::name`](crate::command::EngineCommand::name) gives
    /// it, so a dropped wait is announced like a refused command.
    pub fn name(self) -> &'static str {
        match self {
            TakeKind::Cut => "cut",
            TakeKind::Auto => "auto",
        }
    }
}

/// Why a waiting take fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fired {
    /// Preview reached the threshold, or has nothing left to settle.
    Settled,
    /// The wait reached [`SettleRule::max_wait`].
    TimedOut,
}

/// What a tick makes of a waiting take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// It waits on.
    Wait,
    /// Preview no longer shows its shot: it is dropped.
    PreviewChanged,
    /// It fires now.
    Fire(Fired),
}

/// A take waiting for preview to settle; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct WaitingTake {
    /// Cut or Auto, as pressed.
    pub kind: TakeKind,
    /// The shot the operator saw on preview, which the take is of.
    pub shot: ShotId,
    /// Seconds of ticks waited so far.
    pub waited: f64,
}

impl WaitingTake {
    /// A take of `kind` of `shot`, pressed now.
    pub fn new(kind: TakeKind, shot: ShotId) -> Self {
        Self {
            kind,
            shot,
            waited: 0.0,
        }
    }

    /// Advances the wait by `dt` seconds (one that is not a positive number
    /// counts as none) and judges it, in the module docs' order, with the
    /// preview bus showing `preview`, whose settledness is `settling`
    /// (`None`: nothing to settle, as for [`SettleRule::waits`]).
    pub fn step(
        &mut self,
        dt: f64,
        preview: Option<&ShotId>,
        settling: Option<f64>,
        rule: &SettleRule,
    ) -> Verdict {
        if preview != Some(&self.shot) {
            return Verdict::PreviewChanged;
        }
        if dt.is_finite() && dt > 0.0 {
            self.waited += dt;
        }
        if !rule.waits(settling) {
            Verdict::Fire(Fired::Settled)
        } else if self.waited + WAIT_EPSILON >= rule.max_wait {
            Verdict::Fire(Fired::TimedOut)
        } else {
            Verdict::Wait
        }
    }
}

/// A take that fired by itself on a tick, whose late second press may
/// still arrive; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct FiredTake {
    /// The shot it put on air, or is mixing to.
    pub shot: ShotId,
    /// Seconds of ticks since it fired.
    pub since: f64,
}

/// Settle-before-take for one studio: the rule, the take waiting, the take
/// that just fired by itself and the drops; see the module docs. The engine
/// core owns it and copies it into every snapshot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettleGate {
    /// The rule in force; `None` when settle-before-take is off.
    rule: Option<SettleRule>,
    /// The take waiting, if one is.
    waiting: Option<WaitingTake>,
    /// The take that fired by itself less than [`LATE_PRESS`] ago, if one
    /// did and nothing has been pressed or staged since.
    fired: Option<FiredTake>,
    /// Waiting takes dropped so far.
    dropped: u64,
    /// The newest of them, and why it was dropped.
    last_dropped: Option<(TakeKind, CommandError)>,
}

impl SettleGate {
    /// A gate that applies `rule`; with `None` it never holds a take.
    pub fn new(rule: Option<SettleRule>) -> Self {
        Self {
            rule,
            ..Self::default()
        }
    }

    /// The rule in force; `None` when settle-before-take is off.
    pub fn rule(&self) -> Option<&SettleRule> {
        self.rule.as_ref()
    }

    /// The take waiting for preview to settle.
    pub fn waiting(&self) -> Option<&WaitingTake> {
        self.waiting.as_ref()
    }

    /// The take that fired by itself less than [`LATE_PRESS`] ago, whose
    /// late second press takes nothing ([`Self::late_press`]).
    pub fn just_fired(&self) -> Option<&FiredTake> {
        self.fired.as_ref()
    }

    /// Waiting takes dropped so far: preview changed during the wait, or
    /// the switcher refused them when they fired.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The newest dropped take's kind, and why it was dropped.
    pub fn last_dropped(&self) -> Option<&(TakeKind, CommandError)> {
        self.last_dropped.as_ref()
    }

    /// Takes in a press of `kind` by an operator who saw `preview`, which
    /// the preview bus still shows (the fence has passed), whose
    /// settledness is `settling`, while `idle` (no transition runs). True
    /// when the take now waits; false when it is to run at once: it is the
    /// second press of the waiting take (it forces it), or it need not
    /// wait. A take pressed of another shot replaces the one waiting. Either
    /// way, no late press of a take that fired by itself is expected any
    /// more: this press was not one ([`Self::late_press`]).
    pub fn press(
        &mut self,
        kind: TakeKind,
        preview: Option<&ShotId>,
        settling: Option<f64>,
        idle: bool,
    ) -> bool {
        self.fired = None;
        let before = self.waiting.take();
        let Some(shot) = preview else {
            return false;
        };
        if before.is_some_and(|w| &w.shot == shot) {
            return false;
        }
        let waits = idle && self.rule.is_some_and(|r| r.waits(settling));
        if waits {
            self.waiting = Some(WaitingTake::new(kind, shot.clone()));
        }
        waits
    }

    /// Advances the waiting take by one tick of `dt` seconds with preview
    /// showing `preview`, whose settledness is `settling` (see
    /// [`WaitingTake::step`]). Returns the take to run now, and why; a take
    /// whose preview changed is dropped and counted here. The time since a
    /// take fired by itself advances too, and once it reaches
    /// [`LATE_PRESS`] (give or take [`WAIT_EPSILON`]) its late press is no
    /// longer expected.
    pub fn tick(
        &mut self,
        dt: f64,
        preview: Option<&ShotId>,
        settling: Option<f64>,
    ) -> Option<(TakeKind, Fired)> {
        if let Some(fired) = &mut self.fired {
            if dt.is_finite() && dt > 0.0 {
                fired.since += dt;
            }
            if fired.since + WAIT_EPSILON >= LATE_PRESS {
                self.fired = None;
            }
        }
        let rule = self.rule?;
        let waiting = self.waiting.as_mut()?;
        match waiting.step(dt, preview, settling, &rule) {
            Verdict::Wait => None,
            Verdict::PreviewChanged => {
                let w = self.waiting.take()?;
                self.preview_changed(w, preview);
                None
            }
            Verdict::Fire(why) => self.waiting.take().map(|w| (w.kind, why)),
        }
    }

    /// Drops `waiting`, whose shot `preview` no longer shows, refused as a
    /// fenced take is.
    fn preview_changed(&mut self, waiting: WaitingTake, preview: Option<&ShotId>) {
        self.refused(
            waiting.kind,
            CommandError::PreviewChanged {
                seen: Some(waiting.shot),
                preview: preview.cloned(),
            },
        );
    }

    /// Counts a waiting take of `kind` that could not go on air, for
    /// `reason`.
    pub fn refused(&mut self, kind: TakeKind, reason: CommandError) {
        self.dropped += 1;
        self.last_dropped = Some((kind, reason));
    }

    /// Takes the waiting take out to run it now, ahead of a shot the
    /// operator stages, with the preview bus showing `preview`: the kind to
    /// run, or `None` when nothing waits, or when preview no longer shows
    /// its shot (it is then dropped, as on a tick). The operator has moved
    /// on, so no late press of a take that fired by itself is expected any
    /// more.
    pub fn force(&mut self, preview: Option<&ShotId>) -> Option<TakeKind> {
        self.fired = None;
        let waiting = self.waiting.take()?;
        if preview == Some(&waiting.shot) {
            return Some(waiting.kind);
        }
        self.preview_changed(waiting, preview);
        None
    }

    /// Ends the waiting take unfired and uncounted: a take of the
    /// operator's own superseded it. It ends the wait for a late press of a
    /// take that fired by itself too.
    pub fn cancel(&mut self) {
        self.waiting = None;
        self.fired = None;
    }

    /// Records that a waiting take of `shot` fired by itself and went
    /// through the switcher's checks, so that its late second press, for
    /// [`LATE_PRESS`], takes nothing (see the module docs).
    pub fn went_on_air(&mut self, shot: ShotId) {
        self.fired = Some(FiredTake { shot, since: 0.0 });
    }

    /// True when a take pressed now, with `program` and `preview` on the
    /// buses and a transition running or not (`mixing`), is the late second
    /// press of the take that just fired by itself: one did, less than
    /// [`LATE_PRESS`] ago, and its shot is on program, or coming in on
    /// preview during its mix (see the module docs). The core lets a Cut
    /// during that mix through, to land it.
    pub fn late_press(
        &self,
        program: Option<&ShotId>,
        preview: Option<&ShotId>,
        mixing: bool,
    ) -> bool {
        self.fired
            .as_ref()
            .is_some_and(|f| program == Some(&f.shot) || (mixing && preview == Some(&f.shot)))
    }
}

#[cfg(test)]
mod tests;
