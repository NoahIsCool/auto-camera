//! The control column between the monitors: CUT, AUTO, the transition and
//! the T-bar, laid out like a hardware switcher's transition block.
//!
//! On top the transition is a row of three toggles (Cut, Mix, Dip) rather
//! than a drop-down, so the selection shows at a glance and changes with
//! one click, with its rate under them. Below, the two take buttons are
//! the largest targets in the window, CUT over AUTO, beside the T-bar, and
//! AUTO lights red while a transition runs, as a switcher's AUTO key does,
//! so the operator sees a mix in progress without reading the program
//! caption. The buttons and the T-bar share whatever height the monitors
//! leave, so the block never runs past the monitors it sits between.
//!
//! The T-bar is painted rather than an egui slider, as a broad lever in a
//! groove that fills red from its rest end as the transition advances. It
//! keeps the slider's behaviour, which the switcher relies on (see
//! [`super::switching`]): a press anywhere in the groove moves the lever
//! there, a drag follows the pointer, positions run from 0 at the bottom to
//! 1 at the top, and the lever stays where it is left. It tells the
//! accessibility tree it is a slider with its position, so screen readers
//! and the window tests find it by that role. It never takes keyboard
//! focus, so dragging it leaves the shortcuts live.

use ac_core::ShotId;
use ac_engine::{EngineCommand, EngineSnapshot};
use ac_switcher::TransitionKind;
use eframe::egui::{
    self, AtomExt, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use super::switching::{DURATION_RANGE, KINDS, SwitchState, TBAR_HOLDS, cut_enabled};
use super::theme;

/// Width of the control column, in points.
pub const WIDTH: f32 = 176.0;
/// Width of CUT and AUTO, beside the T-bar.
const TAKE_WIDTH: f32 = 104.0;
/// Gap between the take buttons and the T-bar.
const TBAR_GAP: f32 = 8.0;
/// Shortest and tallest CUT and AUTO.
const TAKE_HEIGHT: std::ops::RangeInclusive<f32> = 36.0..=64.0;
/// Width of the T-bar's groove.
const GROOVE_WIDTH: f32 = 14.0;
/// Height of the T-bar's lever.
const LEVER_HEIGHT: f32 = 18.0;
/// Shortest T-bar travel, however small the window.
const MIN_TRAVEL: f32 = 60.0;
/// The edge of an unlit key, lighter than its face, like a backlit
/// switcher key's rim.
const KEY_EDGE: Color32 = Color32::from_rgb(72, 79, 90);
/// Tick marks along the T-bar's travel, ends included.
const TICKS: usize = 11;

/// Draws the controls into a column `height` points tall: the transition
/// and its rate on top, then CUT over AUTO (both carrying `seen`) beside
/// the T-bar, which stretches to fill the rest of the column (keeping at
/// least [`MIN_TRAVEL`]). Pushes the commands to send onto `commands`.
pub fn show(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    seen: &Option<ShotId>,
    state: &mut SwitchState,
    height: f32,
    commands: &mut Vec<EngineCommand>,
) {
    let top = ui.cursor().top();
    ui.label(theme::caps(ui, "Transition"));
    let mut kind = state.transition.kind;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let each = (WIDTH - 8.0) / KINDS.len() as f32;
        for (k, label, hover) in KINDS {
            let selected = kind == k;
            let (fill, color) = if selected {
                (theme::ACCENT, theme::text_on(theme::ACCENT))
            } else {
                (theme::SURFACE, theme::TEXT)
            };
            let button = egui::Button::new(theme::semibold(ui, label, theme::BODY).color(color))
                .fill(fill)
                .corner_radius(CornerRadius::same(theme::RADIUS))
                .min_size(vec2(each, 30.0));
            if ui.add(button).on_hover_text(hover).clicked() {
                kind = k;
            }
        }
    });
    commands.extend(state.set_kind(kind));
    let mut duration = state.transition.duration;
    ui.horizontal(|ui| {
        ui.label(theme::caps(ui, "Rate"));
        ui.add_enabled(
            kind != TransitionKind::Cut,
            egui::DragValue::new(&mut duration)
                .range(DURATION_RANGE)
                .speed(0.02)
                .suffix(" s")
                .fixed_decimals(1),
        );
    });
    commands.extend(state.set_duration(duration));
    ui.add_space(6.0);
    let rest = (height - (ui.cursor().top() - top)).max(MIN_TRAVEL + LEVER_HEIGHT);
    let gap = ui.spacing().item_spacing.y;
    let take = ((rest - gap) / 2.0).clamp(*TAKE_HEIGHT.start(), *TAKE_HEIGHT.end());
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = TBAR_GAP;
        ui.vertical(|ui| {
            ui.set_width(TAKE_WIDTH);
            takes(ui, snap, seen, take, commands);
        });
        commands.extend(tbar(ui, state, rest - LEVER_HEIGHT));
    });
}

/// CUT over AUTO, `height` points each, both carrying `seen`. During a
/// transition the preview holds its incoming shot, so both stay enabled:
/// the switcher queues or cuts short (see [`super::switching`]).
fn takes(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    seen: &Option<ShotId>,
    height: f32,
    commands: &mut Vec<EngineCommand>,
) {
    let ready = snap.switcher.preview.is_some();
    let mut cut = ui
        .add_enabled(cut_enabled(snap), take_button(ui, "CUT", false, height))
        .on_hover_text("Space · during a transition, cuts it short");
    if snap.switcher.tbar_held {
        cut = cut.on_disabled_hover_text(TBAR_HOLDS);
    }
    if cut.clicked() {
        commands.push(EngineCommand::Cut {
            preview: seen.clone(),
        });
    }
    let running = snap.switcher.in_transition;
    if ui
        .add_enabled(ready, take_button(ui, "AUTO", running, height))
        .on_hover_text("Enter · during a transition, queues a take of the next shot")
        .clicked()
    {
        commands.push(EngineCommand::Auto {
            preview: seen.clone(),
        });
    }
}

/// A take button `height` points tall: large, bold, and lit red when
/// `lit`.
fn take_button(ui: &Ui, text: &str, lit: bool, height: f32) -> egui::Button<'static> {
    let fill = if lit {
        theme::PROGRAM_RED
    } else {
        theme::SURFACE_ACTIVE
    };
    let color = if lit {
        theme::text_on(theme::PROGRAM_RED)
    } else {
        theme::TEXT_STRONG
    };
    egui::Button::new(
        theme::semibold(ui, text.to_owned(), theme::KEY)
            .extra_letter_spacing(1.0)
            .color(color)
            .atom_grow(true),
    )
    .fill(fill)
    .stroke(Stroke::new(
        1.0,
        if lit { theme::RED_TEXT } else { KEY_EDGE },
    ))
    .corner_radius(CornerRadius::same(theme::CARD_RADIUS))
    .min_size(vec2(TAKE_WIDTH, height))
}

/// The lever position for a pointer at height `y` in a groove spanning
/// `top..=bottom`: 0 at the bottom, 1 at the top, clamped.
pub fn position_at(y: f32, top: f32, bottom: f32) -> f64 {
    let span = bottom - top;
    if span <= 0.0 {
        return 0.0;
    }
    f64::from(((bottom - y) / span).clamp(0.0, 1.0))
}

/// Draws the T-bar `travel` points tall and moves its lever; the command
/// to send if it moved (see the module docs).
fn tbar(ui: &mut Ui, state: &mut SwitchState, travel: f32) -> Option<EngineCommand> {
    let width = WIDTH - TAKE_WIDTH - TBAR_GAP;
    let size = vec2(width, travel + LEVER_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    // The lever's centre travels between these.
    let (top, bottom) = (
        rect.top() + LEVER_HEIGHT / 2.0,
        rect.bottom() - LEVER_HEIGHT / 2.0,
    );
    let mut command = None;
    if (response.dragged() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        command = state.move_tbar(position_at(pointer.y, top, bottom));
    }
    let value = state.tbar;
    response.widget_info(|| egui::WidgetInfo::slider(true, value, "T-bar"));
    let painter = ui.painter_at(rect);
    let groove = Rect::from_center_size(
        pos2(rect.center().x, rect.center().y),
        vec2(GROOVE_WIDTH, bottom - top),
    );
    // A scale on both sides of the groove, longer at the ends and middle.
    for i in 0..TICKS {
        let y = top + (bottom - top) * i as f32 / (TICKS - 1) as f32;
        let major = i == 0 || i == TICKS - 1 || i == TICKS / 2;
        let length = if major { 10.0 } else { 6.0 };
        for side in [-1.0, 1.0] {
            let x = rect.center().x + side * (GROOVE_WIDTH / 2.0 + 4.0);
            painter.hline(x..=x + side * length, y, Stroke::new(1.0, theme::BORDER));
        }
    }
    painter.rect_filled(groove, CornerRadius::same(7), theme::WELL);
    painter.rect_stroke(
        groove,
        CornerRadius::same(7),
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    let y = bottom - (bottom - top) * value as f32;
    // Mid-travel the groove fills red under the lever, so a transition the
    // lever holds shows from across the room; at either end it is empty.
    let in_travel = value > 0.0 && value < 1.0;
    if in_travel {
        let fill = Rect::from_min_max(pos2(groove.left(), y), groove.right_bottom());
        painter.rect_filled(fill, CornerRadius::same(7), theme::PROGRAM_RED);
    }
    let lever = Rect::from_center_size(pos2(rect.center().x, y), vec2(width - 4.0, LEVER_HEIGHT));
    let lit = response.hovered() || response.dragged();
    let face = if lit {
        Color32::from_rgb(92, 100, 112)
    } else {
        Color32::from_rgb(70, 77, 88)
    };
    painter.rect_filled(lever, CornerRadius::same(4), face);
    painter.rect_stroke(
        lever,
        CornerRadius::same(4),
        Stroke::new(1.0, Color32::from_rgb(120, 130, 144)),
        StrokeKind::Inside,
    );
    // Grip lines across the handle, and its red index line.
    for dy in [-4.0, 4.0] {
        painter.hline(
            lever.left() + 10.0..=lever.right() - 10.0,
            lever.center().y + dy,
            Stroke::new(1.0, Color32::from_rgb(110, 119, 132)),
        );
    }
    painter.hline(
        lever.left() + 6.0..=lever.right() - 6.0,
        lever.center().y,
        Stroke::new(2.0, theme::PROGRAM_RED),
    );
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::snapshot;
    use ac_switcher::Transition;

    #[test]
    fn the_lever_maps_height_to_position() {
        assert_eq!(position_at(100.0, 0.0, 100.0), 0.0, "bottom");
        assert_eq!(position_at(0.0, 0.0, 100.0), 1.0, "top");
        assert!((position_at(25.0, 0.0, 100.0) - 0.75).abs() < 1e-6);
        assert_eq!(position_at(-40.0, 0.0, 100.0), 1.0, "clamped above");
        assert_eq!(position_at(140.0, 0.0, 100.0), 0.0, "clamped below");
        assert_eq!(position_at(5.0, 10.0, 10.0), 0.0, "no travel");
    }

    #[test]
    fn the_controls_render_headless_in_a_tiny_column() {
        let ctx = egui::Context::default();
        let snap = snapshot();
        let mut state = SwitchState::new(Transition {
            kind: TransitionKind::Mix,
            duration: 0.8,
        });
        let mut commands = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &snap, &None, &mut state, 10.0, &mut commands);
        });
        assert!(commands.is_empty(), "{commands:?}");
    }
}
