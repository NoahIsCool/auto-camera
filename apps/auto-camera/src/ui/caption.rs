//! What the preview monitor's caption says beyond the shot's name: that
//! the operator's take waits for preview to settle, and that the preview
//! shot's subject is lost.
//!
//! * **A waiting take.** With settle-before-take on, a Cut or Auto pressed
//!   while the preview crop is still travelling to its subject waits for
//!   it, at most half a second, and a second press takes it at once
//!   ([`ac_engine::settle`]). Without a word on screen the operator would
//!   see a press that did nothing, so the caption says which take waits
//!   and how to force it ([`settle_note`]) for as long as it does.
//! * **A lost subject** (spec M8, shot status). A shot whose subject is
//!   lost holds its framing on where the subject was last seen, so the
//!   picture on preview can be an empty stage. The caption names the
//!   subject that is lost ([`lost_warning`]) and turns amber
//!   ([`caption_color`]), so it is seen at a glance, while the monitor's
//!   frame stays green. It only warns: the take goes ahead as it would
//!   without it (whether a lost subject may be cut to is not this window's
//!   to decide).
//!
//! Both are functions of the snapshot alone, so the wording is tested
//! without a window; [`super::switching`] appends them to the caption after
//! the note on a shot queued during a transition.

use ac_engine::settle::TakeKind;
use ac_engine::{EngineSnapshot, ShotStatus};
use eframe::egui::Color32;

/// The caption's colour while it carries [`lost_warning`]: the amber of the
/// side panel's warnings.
pub const WARNING: Color32 = Color32::from_rgb(230, 140, 40);

/// The caption's note on a take waiting for preview to settle
/// (`CUT waiting for preview to settle · press again to take now`); `None`
/// while none waits.
pub fn settle_note(snap: &EngineSnapshot) -> Option<String> {
    let waiting = snap.switcher.settle.waiting()?;
    let button = match waiting.kind {
        TakeKind::Cut => "CUT",
        TakeKind::Auto => "AUTO",
    };
    Some(format!(
        "{button} waiting for preview to settle · press again to take now"
    ))
}

/// The caption's warning that the preview shot's subject is lost
/// (`subject 2 lost`); `None` when preview is empty, holds a wide or a
/// shot whose subject is tracking, or names a shot no longer listed.
pub fn lost_warning(snap: &EngineSnapshot) -> Option<String> {
    let shot = snap.shot(snap.switcher.preview.as_ref()?)?;
    (shot.status == ShotStatus::Lost).then(|| match shot.subject {
        Some(subject) => format!("subject {} lost", subject.0),
        None => "subject lost".into(),
    })
}

/// The preview notes, in caption order: the waiting take, then the lost
/// subject.
pub fn preview_notes(snap: &EngineSnapshot) -> Vec<String> {
    settle_note(snap)
        .into_iter()
        .chain(lost_warning(snap))
        .collect()
}

/// The preview caption's colour: [`WARNING`] while it warns of a lost
/// subject, `normal` otherwise.
pub fn caption_color(snap: &EngineSnapshot, normal: Color32) -> Color32 {
    if lost_warning(snap).is_some() {
        WARNING
    } else {
        normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{nine_tiles, snapshot};
    use ac_core::ShotId;
    use ac_engine::settle::{SettleGate, SettleRule};

    /// `snap` with a take of `kind` of its preview waiting.
    fn waiting(mut snap: EngineSnapshot, kind: TakeKind) -> EngineSnapshot {
        let mut gate = SettleGate::new(Some(SettleRule {
            threshold: 0.6,
            max_wait: 0.5,
        }));
        let preview = snap.switcher.preview.clone();
        assert!(gate.press(kind, preview.as_ref(), Some(0.2), true));
        snap.switcher.settle = gate;
        snap
    }

    /// [`nine_tiles`] with tile 2, a subject shot, on preview.
    fn staged() -> EngineSnapshot {
        let mut snap = nine_tiles();
        snap.switcher.preview = Some(snap.shots[1].id.clone());
        snap
    }

    #[test]
    fn a_waiting_take_says_which_button_waits_and_how_to_force_it() {
        let snap = staged();
        assert_eq!(settle_note(&snap), None);
        assert_eq!(
            settle_note(&waiting(snap.clone(), TakeKind::Cut)).as_deref(),
            Some("CUT waiting for preview to settle · press again to take now")
        );
        assert_eq!(
            settle_note(&waiting(snap, TakeKind::Auto)).as_deref(),
            Some("AUTO waiting for preview to settle · press again to take now")
        );
    }

    #[test]
    fn a_lost_preview_subject_is_named_in_amber() {
        let mut snap = staged();
        assert_eq!(lost_warning(&snap), None, "tracking");
        assert_eq!(caption_color(&snap, Color32::GREEN), Color32::GREEN);
        snap.shots[1].status = ShotStatus::Lost;
        assert_eq!(lost_warning(&snap).as_deref(), Some("subject 1 lost"));
        assert_eq!(caption_color(&snap, Color32::GREEN), WARNING);
        snap.shots[1].subject = None;
        assert_eq!(lost_warning(&snap).as_deref(), Some("subject lost"));
        // Lost on program or on another tile is not preview's to say.
        let mut other = staged();
        other.shots[2].status = ShotStatus::Lost;
        assert_eq!(lost_warning(&other), None);
    }

    #[test]
    fn nothing_is_said_of_an_empty_preview_a_wide_or_a_shot_gone() {
        let mut snap = snapshot();
        assert_eq!(snap.switcher.preview, None);
        assert!(preview_notes(&snap).is_empty());
        snap.switcher.preview = Some(snap.shots[0].id.clone());
        snap.shots[0].status = ShotStatus::Wide;
        assert_eq!(lost_warning(&snap), None, "a wide has no subject to lose");
        snap.switcher.preview = Some(ShotId("stage/9/close".into()));
        assert_eq!(lost_warning(&snap), None);
    }

    #[test]
    fn the_notes_come_in_caption_order() {
        let mut snap = waiting(staged(), TakeKind::Cut);
        snap.shots[1].status = ShotStatus::Lost;
        assert_eq!(
            preview_notes(&snap),
            [
                "CUT waiting for preview to settle · press again to take now",
                "subject 1 lost"
            ]
        );
    }
}
