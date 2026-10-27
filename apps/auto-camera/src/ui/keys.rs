//! Keyboard shortcuts, laid out like a hardware switcher panel.
//!
//! | Key   | Action                                        |
//! |-------|-----------------------------------------------|
//! | 1–9   | stage multiview tile 1–9 on preview            |
//! | Space | cut: preview and program swap at once          |
//! | Enter | auto: run the selected transition to preview   |
//!
//! During a transition the same keys stage the next shot for its end, cut
//! it short (or cut to the next shot) and queue a take of the next shot; the
//! switcher has the rules ([`ac_switcher`]).
//!
//! Shortcuts must never fire by accident during a show, so they are ignored
//! while a text field has keyboard focus (typing a prompt must not cut),
//! on key repeat (holding Space must not cut back and forth), and while
//! Ctrl, Alt or Cmd is held (those combinations belong to the system). Only
//! key presses count, not releases.
//!
//! [`actions`] turns egui input events into [`Action`]s and [`command`]
//! turns an action into an engine command; neither needs a window.

use ac_core::ShotId;
use ac_engine::EngineCommand;
use eframe::egui::{Event, Key, Modifiers};

/// What a shortcut asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Stage the multiview tile with this zero-based index on preview.
    Preview(usize),
    /// Cut.
    Cut,
    /// Auto transition.
    Auto,
}

/// The action bound to `key`, ignoring modifiers.
pub fn action_for(key: Key) -> Option<Action> {
    let digit = [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    match key {
        Key::Space => Some(Action::Cut),
        Key::Enter => Some(Action::Auto),
        k => digit.iter().position(|d| *d == k).map(Action::Preview),
    }
}

/// True when `m` holds a modifier that disables shortcuts (Shift does not).
fn reserved(m: Modifiers) -> bool {
    m.alt || m.ctrl || m.command || m.mac_cmd
}

/// The shortcut actions in one frame's `events`; none while `typing`.
pub fn actions(events: &[Event], typing: bool) -> Vec<Action> {
    if typing {
        return Vec::new();
    }
    events
        .iter()
        .filter_map(|e| match e {
            Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } if !reserved(*modifiers) => action_for(*key),
            _ => None,
        })
        .collect()
}

/// The engine command for `action`, given the multiview's shots in tile
/// order; `None` when the tile does not exist.
pub fn command(action: Action, shots: &[ShotId]) -> Option<EngineCommand> {
    match action {
        Action::Preview(i) => shots
            .get(i)
            .map(|shot| EngineCommand::SetPreview { shot: shot.clone() }),
        Action::Cut => Some(EngineCommand::Cut),
        Action::Auto => Some(EngineCommand::Auto),
    }
}

/// The shortcut label of tile `index`, for the first nine tiles.
pub fn tile_label(index: usize) -> Option<String> {
    (index < 9).then(|| (index + 1).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain key press.
    fn press(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    /// `n` shot ids in tile order.
    fn shots(n: usize) -> Vec<ShotId> {
        (0..n).map(|i| ShotId(format!("s{i}"))).collect()
    }

    #[test]
    fn digits_space_and_enter_are_bound() {
        assert_eq!(action_for(Key::Num1), Some(Action::Preview(0)));
        assert_eq!(action_for(Key::Num9), Some(Action::Preview(8)));
        assert_eq!(action_for(Key::Space), Some(Action::Cut));
        assert_eq!(action_for(Key::Enter), Some(Action::Auto));
        assert_eq!(action_for(Key::Num0), None);
        assert_eq!(action_for(Key::A), None);
    }

    #[test]
    fn presses_become_actions_in_order() {
        let events = [press(Key::Num3), press(Key::Space), press(Key::Enter)];
        assert_eq!(
            actions(&events, false),
            [Action::Preview(2), Action::Cut, Action::Auto]
        );
    }

    #[test]
    fn typing_repeats_releases_and_modifiers_are_ignored() {
        let events = [press(Key::Space)];
        assert!(actions(&events, true).is_empty());
        let repeat = Event::Key {
            key: Key::Space,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        };
        let release = Event::Key {
            key: Key::Space,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let ctrl = Event::Key {
            key: Key::Num1,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        };
        let shift = Event::Key {
            key: Key::Num1,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::SHIFT,
        };
        assert!(actions(&[repeat, release, ctrl], false).is_empty());
        assert_eq!(actions(&[shift], false), [Action::Preview(0)]);
        assert!(actions(&[Event::Text(" ".into())], false).is_empty());
    }

    #[test]
    fn actions_map_to_commands_for_existing_tiles() {
        let list = shots(3);
        assert_eq!(
            command(Action::Preview(2), &list),
            Some(EngineCommand::SetPreview {
                shot: ShotId("s2".into())
            })
        );
        assert_eq!(command(Action::Preview(3), &list), None);
        assert_eq!(command(Action::Preview(0), &[]), None);
        assert_eq!(command(Action::Cut, &[]), Some(EngineCommand::Cut));
        assert_eq!(command(Action::Auto, &list), Some(EngineCommand::Auto));
    }

    #[test]
    fn only_the_first_nine_tiles_have_labels() {
        assert_eq!(tile_label(0).as_deref(), Some("1"));
        assert_eq!(tile_label(8).as_deref(), Some("9"));
        assert_eq!(tile_label(9), None);
    }
}
