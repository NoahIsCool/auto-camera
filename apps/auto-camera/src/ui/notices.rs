//! The status bar's notice: one line telling the operator what just
//! happened.
//!
//! Three things raise one: a command's reply ([`notice_for`]: routine
//! switching replies are silent, every error is shown), a take the switcher
//! dropped when the transition before it ended ([`dropped_notice`]), and
//! the engine stopping ([`stopped_notice`]). The newest raised notice is
//! shown for [`NOTICE_FOR`] and then cleared; while the engine is stopped,
//! its notice replaces any other, since nothing else matters then.
//!
//! A take queued during a transition is answered when it is queued, so its
//! drop can only be read from the snapshots: the switcher counts the takes
//! it dropped and keeps the newest reason. [`Notices`] remembers the count
//! it has seen, so each drop raises one notice however many frames draw
//! the same snapshot, and drops counted before the window opened are not
//! news. Several drops between two frames raise one notice, with the
//! newest reason.
//!
//! Time is egui's input time in seconds, which a real window reads from the
//! clock and the test harness advances by a fixed step per frame, so the
//! expiry is tested without waiting.

use std::time::Duration;

use ac_engine::{CommandError, CommandReply, EngineSnapshot};

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
}

impl Notices {
    /// Notices of a window whose first snapshot is `first`: the takes it
    /// already counts as dropped are not news.
    pub fn new(first: &EngineSnapshot) -> Self {
        Self {
            latest: None,
            dropped_seen: first.switcher.dropped_takes,
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
    /// [`NOTICE_FOR`], then raises the notice of a take dropped since the
    /// last snapshot.
    pub fn observe(&mut self, snap: &EngineSnapshot, now: f64) {
        if self
            .latest
            .as_ref()
            .is_some_and(|n| now - n.at > NOTICE_FOR.as_secs_f64())
        {
            self.latest = None;
        }
        if let Some((text, error)) = dropped_notice(snap, self.dropped_seen) {
            self.raise(text, error, now);
        }
        self.dropped_seen = snap.switcher.dropped_takes;
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
