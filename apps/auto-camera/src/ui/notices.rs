//! The status bar's notice: one line telling the operator what just
//! happened.
//!
//! These raise one: a command's reply ([`notice_for`]: routine switching
//! replies are silent, every error is shown, a take refused because preview
//! changed after the operator saw it included, and so is the director
//! dropping to Suggest or pausing because the operator switched by hand), a
//! take the switcher dropped when the transition before it ended
//! ([`dropped_notice`]), a take that waited for preview to settle and was
//! dropped at the end of its wait, shown as the refused command it became
//! ([`settle_dropped_notice`]), a suggestion the director did not stage
//! because the operator's shot holds preview ([`held_back_notice`]), the
//! engine stopping ([`stopped_notice`]), and, as the window opens, the
//! config's load warnings ([`warnings_notice`]). The newest raised notice
//! is shown for [`NOTICE_FOR`] and then cleared; while the engine is
//! stopped, its notice replaces any other, since nothing else matters then.
//!
//! A take queued during a transition is answered when it is queued, so its
//! drop can only be read from the snapshots: the switcher counts the takes
//! it dropped and keeps the newest reason. A take waiting for preview to
//! settle is answered when it is pressed, and its drop is read the same
//! way, from the engine's count ([`ac_engine::settle`]). A suggestion held
//! back is no command's at all, so it too is read from the snapshots, where
//! the engine counts them and keeps the newest. [`Notices`] remembers the
//! counts it has seen, so each drop or hold raises one notice however many
//! frames draw the same snapshot, and those counted before the window
//! opened are not news. Several between two frames raise one notice, the
//! newest.
//!
//! Time is egui's input time in seconds, which a real window reads from the
//! clock and the test harness advances by a fixed step per frame, so the
//! expiry is tested without waiting.

use std::time::Duration;

use ac_engine::{CommandError, CommandReply, EngineSnapshot};

use super::text::shot_name;

/// How long a notice stays in the status bar.
pub const NOTICE_FOR: Duration = Duration::from_secs(8);

/// The notice for a command's reply, if it deserves one. Routine switching
/// replies are silent; every error is shown.
pub fn notice_for(
    command: &str,
    reply: &Result<CommandReply, CommandError>,
) -> Option<(String, bool)> {
    match reply {
        Err(e) => Some((format!("{command}: {e}"), true)),
        Ok(CommandReply::QueryAdded { id }) => Some((format!("query {} added", id.0), false)),
        Ok(CommandReply::OutputStarting) => Some(("output starting".into(), false)),
        Ok(CommandReply::OutputStopping) => Some(("output finishing".into(), false)),
        Ok(CommandReply::TookOver(handover)) => {
            Some((format!("{handover}, as you switched by hand"), false))
        }
        Ok(CommandReply::Done) => match command {
            "set_preview" | "cut" | "cut_to" | "auto" | "set_tbar" | "accept_suggestion" => None,
            other => Some((format!("{other}: done"), false)),
        },
    }
}

/// The notice for a queued take the switcher dropped since the window had
/// seen `seen` drops, shown like a refused command; `None` when there is no
/// new drop.
pub fn dropped_notice(snap: &EngineSnapshot, seen: u64) -> Option<(String, bool)> {
    let switcher = &snap.switcher;
    if switcher.dropped_takes <= seen {
        return None;
    }
    let reason = switcher.last_dropped.clone()?;
    notice_for("queued take", &Err(CommandError::Switch(reason)))
}

/// The notice for an operator's take that waited for preview to settle
/// and was dropped, since the window had seen `seen` such drops: preview
/// changed during the wait, or the switcher refused it when it fired. It
/// reads as the refused command it became ("cut: preview changed to …
/// since you saw …: nothing was taken"); `None` when there is no new drop.
pub fn settle_dropped_notice(snap: &EngineSnapshot, seen: u64) -> Option<(String, bool)> {
    let settle = &snap.switcher.settle;
    if settle.dropped() <= seen {
        return None;
    }
    let (kind, reason) = settle.last_dropped()?;
    notice_for(kind.name(), &Err(reason.clone()))
}

/// The notice for a suggestion the director did not stage, since the
/// window had seen `seen` of them, because the operator's shot holds
/// preview; `None` when there is no new one.
pub fn held_back_notice(snap: &EngineSnapshot, seen: u64) -> Option<(String, bool)> {
    let d = &snap.director;
    if d.held_back <= seen {
        return None;
    }
    let suggested = d.last_held_back.as_ref()?;
    let held = d.preview_held.as_ref()?;
    Some((
        format!(
            "director: {} not staged, your {} holds preview until you take or release it",
            shot_name(snap, Some(suggested)),
            shot_name(snap, Some(held))
        ),
        false,
    ))
}

/// The notice for the config's load warnings (a missing `schema` key), all
/// of them in order; not an error, since the studio runs.
pub fn warnings_notice(warnings: &[String]) -> Option<(String, bool)> {
    (!warnings.is_empty()).then(|| (format!("warning: {}", warnings.join(" · ")), false))
}

/// The notice for an engine that is no longer running, with the reason when
/// it died rather than stopped on request; it replaces any other notice.
pub fn stopped_notice(snap: &EngineSnapshot) -> Option<(String, bool)> {
    if snap.running {
        return None;
    }
    Some(match &snap.fatal {
        Some(why) => (format!("the engine has stopped: {why}"), true),
        None => ("the engine has stopped".into(), true),
    })
}

/// A raised notice.
#[derive(Debug, Clone, PartialEq)]
struct Notice {
    /// Text.
    text: String,
    /// It reports a failure.
    error: bool,
    /// When it was raised, in egui seconds.
    at: f64,
}

/// The newest notice and the dropped takes already noticed; see the module
/// docs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Notices {
    /// The newest notice, until it expires.
    latest: Option<Notice>,
    /// Dropped takes counted by the newest snapshot observed.
    dropped_seen: u64,
    /// Suggestions held back counted by the newest snapshot observed.
    held_back_seen: u64,
    /// Waiting takes dropped counted by the newest snapshot observed.
    settle_dropped_seen: u64,
}

impl Notices {
    /// Notices of a window whose first snapshot is `first`: the takes it
    /// already counts as dropped (queued or waiting to settle), and the
    /// suggestions as held back, are not news.
    pub fn new(first: &EngineSnapshot) -> Self {
        Self {
            latest: None,
            dropped_seen: first.switcher.dropped_takes,
            held_back_seen: first.director.held_back,
            settle_dropped_seen: first.switcher.settle.dropped(),
        }
    }

    /// Raises `text` at `now`, a failure when `error`; it replaces the
    /// notice shown.
    pub fn raise(&mut self, text: String, error: bool, now: f64) {
        self.latest = Some(Notice {
            text,
            error,
            at: now,
        });
    }

    /// Reads the frame's snapshot at `now`: clears a notice shown for
    /// [`NOTICE_FOR`], then raises the notice of a suggestion held back and
    /// of a take dropped since the last snapshot, queued or waiting to
    /// settle (the drops last, as the failures).
    pub fn observe(&mut self, snap: &EngineSnapshot, now: f64) {
        if self
            .latest
            .as_ref()
            .is_some_and(|n| now - n.at > NOTICE_FOR.as_secs_f64())
        {
            self.latest = None;
        }
        if let Some((text, error)) = held_back_notice(snap, self.held_back_seen) {
            self.raise(text, error, now);
        }
        self.held_back_seen = snap.director.held_back;
        if let Some((text, error)) = dropped_notice(snap, self.dropped_seen) {
            self.raise(text, error, now);
        }
        self.dropped_seen = snap.switcher.dropped_takes;
        if let Some((text, error)) = settle_dropped_notice(snap, self.settle_dropped_seen) {
            self.raise(text, error, now);
        }
        self.settle_dropped_seen = snap.switcher.settle.dropped();
    }

    /// The notice the status bar shows for `snap`, and whether it reports a
    /// failure: a stopped engine's, otherwise the newest raised.
    pub fn shown(&self, snap: &EngineSnapshot) -> Option<(String, bool)> {
        stopped_notice(snap).or_else(|| self.latest.as_ref().map(|n| (n.text.clone(), n.error)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::snapshot;
    use ac_core::{QueryId, ShotId};
    use ac_director::Handover;
    use ac_engine::settle::{SettleGate, TakeKind};
    use ac_switcher::SwitchError;

    #[test]
    fn routine_replies_are_silent_and_errors_are_shown() {
        assert_eq!(notice_for("cut", &Ok(CommandReply::Done)), None);
        assert_eq!(notice_for("cut_to", &Ok(CommandReply::Done)), None);
        assert_eq!(notice_for("set_tbar", &Ok(CommandReply::Done)), None);
        assert_eq!(
            notice_for("pin", &Ok(CommandReply::Done)),
            Some(("pin: done".into(), false))
        );
        assert_eq!(
            notice_for(
                "add_query",
                &Ok(CommandReply::QueryAdded {
                    id: QueryId("singer".into())
                })
            ),
            Some(("query singer added".into(), false))
        );
        let (text, error) =
            notice_for("auto", &Err(CommandError::Switch(SwitchError::NoPreview))).unwrap();
        assert!(error && text.starts_with("auto: "), "{text}");
    }

    #[test]
    fn a_refused_take_and_a_takeover_are_shown() {
        let changed = CommandError::PreviewChanged {
            seen: Some(ShotId("mine".into())),
            preview: Some(ShotId("theirs".into())),
        };
        let (text, error) = notice_for("cut", &Err(changed)).unwrap();
        assert!(error, "{text}");
        assert!(
            text.starts_with("cut: preview changed to ShotId(\"theirs\")"),
            "{text}"
        );
        assert_eq!(
            notice_for(
                "set_preview",
                &Ok(CommandReply::TookOver(Handover::ToSuggest))
            ),
            Some((
                "the director dropped to Suggest, as you switched by hand".into(),
                false
            ))
        );
        let paused = Handover::Paused(Duration::from_secs(10));
        let (text, error) = notice_for("cut", &Ok(CommandReply::TookOver(paused))).unwrap();
        assert!(!error && text.contains("paused for 10 s"), "{text}");
    }

    /// `snap` with `n` suggestions held back, the newest `shot`, while the
    /// operator's `held` holds preview.
    fn held_back(mut snap: EngineSnapshot, n: u64, shot: &str, held: &str) -> EngineSnapshot {
        snap.director.held_back = n;
        snap.director.last_held_back = Some(ShotId(shot.into()));
        snap.director.preview_held = Some(ShotId(held.into()));
        snap
    }

    #[test]
    fn each_held_back_suggestion_raises_one_notice() {
        let fresh = snapshot();
        let wide = fresh.shots[0].id.0.clone();
        let first = held_back(fresh.clone(), 2, "gone", &wide);
        let mut notices = Notices::new(&first);
        notices.observe(&first, 0.0);
        assert_eq!(notices.shown(&first), None, "not news");
        let third = held_back(fresh.clone(), 3, "gone", &wide);
        assert_eq!(held_back_notice(&third, 3), None);
        notices.observe(&third, 1.0);
        let (text, error) = notices.shown(&third).unwrap();
        assert!(!error, "{text}");
        let name = shot_name(&third, Some(&ShotId(wide.clone())));
        assert_eq!(
            text,
            format!(
                "director: gone not staged, your {name} holds preview until you take or \
                 release it"
            )
        );
        notices.raise("pin: done".into(), false, 2.0);
        notices.observe(&third, 3.0);
        assert_eq!(notices.shown(&third), Some(("pin: done".into(), false)));
        // Without a hold there is nothing to say.
        let mut released = held_back(fresh, 4, "gone", &wide);
        released.director.preview_held = None;
        assert_eq!(held_back_notice(&released, 3), None);
    }

    #[test]
    fn a_dropped_take_is_shown_once_like_a_refused_command() {
        let mut snap = snapshot();
        assert_eq!(dropped_notice(&snap, 0), None);
        snap.switcher.dropped_takes = 2;
        snap.switcher.last_dropped = Some(SwitchError::Unavailable(ShotId("stage/wide".into())));
        let (text, error) = dropped_notice(&snap, 1).unwrap();
        assert!(error, "{text}");
        assert_eq!(
            text,
            "queued take: switcher: shot ShotId(\"stage/wide\") is not available"
        );
        assert_eq!(dropped_notice(&snap, 2), None, "already shown");
        snap.switcher.last_dropped = None;
        assert_eq!(dropped_notice(&snap, 1), None, "no reason, no notice");
    }

    /// `snap` with `n` waiting takes dropped, the newest a cut of "mine"
    /// refused because preview changed to "theirs".
    fn settle_dropped(mut snap: EngineSnapshot, n: u64) -> EngineSnapshot {
        let mut gate = SettleGate::default();
        for _ in 0..n {
            gate.refused(
                TakeKind::Cut,
                CommandError::PreviewChanged {
                    seen: Some(ShotId("mine".into())),
                    preview: Some(ShotId("theirs".into())),
                },
            );
        }
        snap.switcher.settle = gate;
        snap
    }

    #[test]
    fn a_dropped_waiting_take_reads_as_the_refused_take_once() {
        let fresh = snapshot();
        assert_eq!(settle_dropped_notice(&fresh, 0), None);
        let first = settle_dropped(fresh.clone(), 1);
        let (text, error) = settle_dropped_notice(&first, 0).unwrap();
        assert!(error, "{text}");
        // The same words as a cut the fence refused when it was pressed.
        let refused = notice_for(
            "cut",
            &Err(CommandError::PreviewChanged {
                seen: Some(ShotId("mine".into())),
                preview: Some(ShotId("theirs".into())),
            }),
        );
        assert_eq!(Some((text, error)), refused);
        // Raised once, and not for drops counted before the window opened.
        let mut notices = Notices::new(&first);
        notices.observe(&first, 0.0);
        assert_eq!(notices.shown(&first), None, "not news");
        let second = settle_dropped(fresh, 2);
        notices.observe(&second, 1.0);
        assert_eq!(notices.shown(&second), refused);
        notices.raise("pin: done".into(), false, 2.0);
        notices.observe(&second, 3.0);
        assert_eq!(notices.shown(&second), Some(("pin: done".into(), false)));
    }

    #[test]
    fn config_warnings_are_one_notice_that_is_not_an_error() {
        assert_eq!(warnings_notice(&[]), None);
        let one = ["studio.toml: no `schema` key; add `schema = 1`".to_owned()];
        assert_eq!(
            warnings_notice(&one),
            Some((
                "warning: studio.toml: no `schema` key; add `schema = 1`".into(),
                false
            ))
        );
        let two = ["a".to_owned(), "b".to_owned()];
        assert_eq!(
            warnings_notice(&two),
            Some(("warning: a · b".into(), false))
        );
    }

    #[test]
    fn a_dead_engine_is_shown_with_its_reason() {
        let mut snap = snapshot();
        assert_eq!(stopped_notice(&snap), None);
        snap.running = false;
        assert_eq!(
            stopped_notice(&snap),
            Some(("the engine has stopped".into(), true))
        );
        snap.fatal = Some("the engine thread panicked: boom".into());
        let (text, error) = stopped_notice(&snap).unwrap();
        assert!(error && text.ends_with("panicked: boom"), "{text}");
    }

    /// `snap` with `n` dropped takes, the newest of `shot`.
    fn dropped(mut snap: EngineSnapshot, n: u64, shot: &str) -> EngineSnapshot {
        snap.switcher.dropped_takes = n;
        snap.switcher.last_dropped = Some(SwitchError::Unavailable(ShotId(shot.into())));
        snap
    }

    #[test]
    fn each_drop_raises_one_notice_that_expires() {
        let fresh = snapshot();
        let mut notices = Notices::new(&fresh);
        notices.observe(&fresh, 0.0);
        assert_eq!(notices.shown(&fresh), None);
        let one = dropped(fresh.clone(), 1, "a");
        notices.observe(&one, 1.0);
        let (text, error) = notices.shown(&one).unwrap();
        assert!(error && text.contains("\"a\""), "{text}");
        // The same snapshot drawn again raises nothing new: a reply notice
        // raised meanwhile stays.
        notices.raise("pin: done".into(), false, 2.0);
        notices.observe(&one, 3.0);
        assert_eq!(notices.shown(&one), Some(("pin: done".into(), false)));
        // Two more drops between frames: one notice, the newest reason.
        let three = dropped(fresh.clone(), 3, "c");
        notices.observe(&three, 4.0);
        let newest = notices.shown(&three).unwrap().0;
        assert!(newest.contains("\"c\""), "{newest}");
        // Shown for NOTICE_FOR, then cleared.
        let at_most = 4.0 + NOTICE_FOR.as_secs_f64();
        notices.observe(&three, at_most);
        assert!(notices.shown(&three).is_some());
        notices.observe(&three, at_most + 0.1);
        assert_eq!(notices.shown(&three), None);
    }

    #[test]
    fn drops_before_the_window_opened_are_not_news() {
        let first = dropped(snapshot(), 4, "a");
        let mut notices = Notices::new(&first);
        notices.observe(&first, 0.0);
        assert_eq!(notices.shown(&first), None);
        let fifth = dropped(snapshot(), 5, "b");
        notices.observe(&fifth, 1.0);
        assert!(notices.shown(&fifth).is_some());
    }

    #[test]
    fn a_stopped_engine_replaces_any_other_notice() {
        let mut snap = snapshot();
        let mut notices = Notices::new(&snap);
        notices.raise("cut: switcher: no shot on preview".into(), true, 0.0);
        snap.running = false;
        assert_eq!(
            notices.shown(&snap),
            Some(("the engine has stopped".into(), true))
        );
    }
}
