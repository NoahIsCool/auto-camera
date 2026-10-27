//! Preview and program monitors and the transition controls between them.
//!
//! The layout follows a hardware switcher: preview on the left with a green
//! frame, program on the right with a red one, and between them Cut, Auto,
//! the transition selector and a vertical T-bar.
//!
//! The T-bar behaves like a physical lever: it stays where the operator
//! leaves it. Moving it away from its rest end starts a manual transition;
//! reaching the far end completes it, after which the far end is the new
//! rest end (the switcher reverses the direction of travel), and moving it
//! back to the start abandons the transition. The UI therefore keeps the
//! lever position itself ([`SwitchState::tbar`]) and only sends positions.
//! The selected transition is likewise UI state, sent when it changes.
//!
//! Cut and Auto stay live while a transition runs, as the switcher takes
//! presses then too ([`ac_switcher`]): a shot staged meanwhile waits for the
//! end, Auto queues a take of it, and Cut cuts the transition short (or cuts
//! straight to the waiting shot). The preview caption names the waiting shot
//! and says whether a take of it is queued ([`queue_caption`]), so the
//! operator sees what will happen when the transition ends.

use ac_engine::{EngineCommand, EngineSnapshot};
use ac_switcher::{Transition, TransitionKind};
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};

use super::layout::{centred, fit};
use super::multiview::{PREVIEW_GREEN, PROGRAM_RED};
use super::text::{percent, shot_name};
use super::textures::Textures;

/// Width of the control column between the monitors.
const CONTROLS_WIDTH: f32 = 130.0;
/// Shortest and longest selectable transition, in seconds.
const DURATION_RANGE: std::ops::RangeInclusive<f64> = 0.1..=5.0;
/// T-bar moves smaller than this are not sent.
const TBAR_STEP: f64 = 1e-3;

/// Transition kinds with their button labels.
pub const KINDS: [(TransitionKind, &str); 3] = [
    (TransitionKind::Cut, "Cut"),
    (TransitionKind::Mix, "Mix"),
    (TransitionKind::Dip, "Dip to black"),
];

/// Operator-side switcher state; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchState {
    /// T-bar lever position in `0..=1`.
    pub tbar: f64,
    /// Selected transition.
    pub transition: Transition,
}

impl SwitchState {
    /// State for a switcher configured with `transition`.
    pub fn new(transition: Transition) -> Self {
        Self {
            tbar: 0.0,
            transition,
        }
    }

    /// Selects `kind`; the command to send if it changed.
    pub fn set_kind(&mut self, kind: TransitionKind) -> Option<EngineCommand> {
        (kind != self.transition.kind).then(|| {
            self.transition.kind = kind;
            self.command()
        })
    }

    /// Selects a duration (clamped); the command to send if it changed.
    pub fn set_duration(&mut self, seconds: f64) -> Option<EngineCommand> {
        let seconds = seconds.clamp(*DURATION_RANGE.start(), *DURATION_RANGE.end());
        ((seconds - self.transition.duration).abs() > 1e-9).then(|| {
            self.transition.duration = seconds;
            self.command()
        })
    }

    /// Moves the lever to `position` (clamped); the command to send if it
    /// moved noticeably.
    pub fn move_tbar(&mut self, position: f64) -> Option<EngineCommand> {
        let position = position.clamp(0.0, 1.0);
        let at_end = position == 0.0 || position == 1.0;
        ((position - self.tbar).abs() >= TBAR_STEP || at_end).then(|| {
            self.tbar = position;
            EngineCommand::SetTBar { position }
        })
    }

    /// The command selecting the current transition.
    fn command(&self) -> EngineCommand {
        EngineCommand::SetTransition {
            transition: self.transition,
        }
    }
}

/// Draws the monitors and controls; returns the commands to send.
pub fn show(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    state: &mut SwitchState,
    textures: &mut Textures,
    aspect: f32,
    max_height: f32,
) -> Vec<EngineCommand> {
    let mut commands = Vec::new();
    let width = ((ui.available_width() - CONTROLS_WIDTH - 24.0) / 2.0).max(80.0);
    let size = fit(aspect, vec2(width, max_height - 24.0));
    ui.horizontal_top(|ui| {
        monitor(ui, snap, textures, Monitor::Preview, size);
        ui.vertical(|ui| {
            ui.set_width(CONTROLS_WIDTH);
            controls(ui, snap, state, size.y, &mut commands);
        });
        monitor(ui, snap, textures, Monitor::Program, size);
    });
    commands
}

/// Which monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Monitor {
    /// The staged shot.
    Preview,
    /// The shot on air.
    Program,
}

/// Draws one monitor with its caption.
fn monitor(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    textures: &mut Textures,
    which: Monitor,
    size: Vec2,
) {
    let (label, color, frame, shot) = match which {
        Monitor::Preview => (
            "PREVIEW",
            PREVIEW_GREEN,
            snap.preview_monitor.as_ref(),
            snap.switcher.preview.as_ref(),
        ),
        Monitor::Program => (
            "PROGRAM",
            PROGRAM_RED,
            snap.program_monitor.as_ref(),
            snap.switcher.program.as_ref(),
        ),
    };
    ui.vertical(|ui| {
        let mut caption = format!("{label}  {}", shot_name(snap, shot));
        if which == Monitor::Program
            && let Some((_, opacity)) = &snap.switcher.overlay
        {
            caption.push_str(&format!("  ·  transition {}", percent(*opacity)));
        }
        if which == Monitor::Preview
            && let Some(queued) = queue_caption(snap)
        {
            caption.push_str(&format!("  ·  {queued}"));
        }
        ui.label(RichText::new(caption).strong().color(color));
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect.expand(3.0));
        painter.rect_filled(rect, CornerRadius::ZERO, Color32::BLACK);
        let name = match which {
            Monitor::Preview => "monitor/preview",
            Monitor::Program => "monitor/program",
        };
        if let Some((id, px)) = textures.show(ui.ctx(), name, frame) {
            let inner = centred(rect, fit(px.x / px.y, rect.size()));
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(id, inner, uv, Color32::WHITE);
        } else if shot.is_none() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "black",
                FontId::proportional(14.0),
                Color32::GRAY,
            );
        }
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(3.0, color),
            StrokeKind::Outside,
        );
    });
}

/// The preview caption's note on the shot waiting for the running
/// transition to end, if one is (`next Speaker 1 Medium · take queued`).
pub fn queue_caption(snap: &EngineSnapshot) -> Option<String> {
    let queued = snap.switcher.queued.as_ref()?;
    let name = shot_name(snap, Some(&queued.shot));
    Some(if queued.take {
        format!("next {name} · take queued")
    } else {
        format!("next {name}")
    })
}

/// Cut, Auto, the transition selector and the T-bar.
fn controls(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    state: &mut SwitchState,
    height: f32,
    commands: &mut Vec<EngineCommand>,
) {
    ui.add_space(20.0);
    // During a transition the preview holds its incoming shot, so both stay
    // enabled: the switcher queues or cuts short (see the module docs).
    let ready = snap.switcher.preview.is_some();
    let big = vec2(CONTROLS_WIDTH, 30.0);
    if ui
        .add_enabled(
            ready,
            egui::Button::new(RichText::new("CUT").strong()).min_size(big),
        )
        .on_hover_text("Space · during a transition, cuts it short")
        .clicked()
    {
        commands.push(EngineCommand::Cut);
    }
    if ui
        .add_enabled(
            ready,
            egui::Button::new(RichText::new("AUTO").strong()).min_size(big),
        )
        .on_hover_text("Enter · during a transition, queues a take of the next shot")
        .clicked()
    {
        commands.push(EngineCommand::Auto);
    }
    ui.add_space(6.0);
    let mut kind = state.transition.kind;
    egui::ComboBox::from_id_salt("transition-kind")
        .width(CONTROLS_WIDTH - 8.0)
        .selected_text(label_of(kind))
        .show_ui(ui, |ui| {
            for (k, label) in KINDS {
                ui.selectable_value(&mut kind, k, label);
            }
        });
    commands.extend(state.set_kind(kind));
    let mut duration = state.transition.duration;
    ui.add_enabled(
        kind != TransitionKind::Cut,
        egui::DragValue::new(&mut duration)
            .range(DURATION_RANGE)
            .speed(0.02)
            .suffix(" s")
            .fixed_decimals(1),
    );
    commands.extend(state.set_duration(duration));
    ui.add_space(6.0);
    ui.label("T-bar");
    let mut tbar = state.tbar;
    let previous = ui.spacing().slider_width;
    ui.spacing_mut().slider_width = (height - 150.0).max(60.0);
    let response = ui.add(
        egui::Slider::new(&mut tbar, 0.0..=1.0)
            .vertical()
            .show_value(false),
    );
    ui.spacing_mut().slider_width = previous;
    if response.changed() {
        commands.extend(state.move_tbar(tbar));
    }
}

/// The label of a transition kind.
fn label_of(kind: TransitionKind) -> &'static str {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or("?", |(_, l)| l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::snapshot;
    use ac_core::ShotId;
    use ac_switcher::Queued;

    /// A mix of 0.8 s, lever at rest.
    fn state() -> SwitchState {
        SwitchState::new(Transition {
            kind: TransitionKind::Mix,
            duration: 0.8,
        })
    }

    #[test]
    fn transition_changes_produce_one_command_each() {
        let mut s = state();
        assert_eq!(s.set_kind(TransitionKind::Mix), None);
        assert_eq!(
            s.set_kind(TransitionKind::Dip),
            Some(EngineCommand::SetTransition {
                transition: Transition {
                    kind: TransitionKind::Dip,
                    duration: 0.8
                }
            })
        );
        assert_eq!(s.set_duration(0.8), None);
        let Some(EngineCommand::SetTransition { transition }) = s.set_duration(9.0) else {
            panic!("a command");
        };
        assert_eq!(transition.duration, 5.0, "clamped");
        assert!(s.set_duration(-1.0).is_some());
        assert_eq!(s.transition.duration, 0.1);
    }

    #[test]
    fn the_tbar_sends_positions_and_rests_at_its_ends() {
        let mut s = state();
        assert_eq!(s.move_tbar(0.0004), None, "too small a move");
        assert_eq!(
            s.move_tbar(0.5),
            Some(EngineCommand::SetTBar { position: 0.5 })
        );
        assert_eq!(
            s.move_tbar(1.7),
            Some(EngineCommand::SetTBar { position: 1.0 })
        );
        assert_eq!(s.tbar, 1.0, "stays at the far end");
        assert_eq!(
            s.move_tbar(0.0),
            Some(EngineCommand::SetTBar { position: 0.0 })
        );
    }

    #[test]
    fn every_kind_has_a_label() {
        for (k, l) in KINDS {
            assert_eq!(label_of(k), l);
        }
    }

    #[test]
    fn the_preview_caption_names_the_queued_shot() {
        let mut snap = snapshot();
        assert_eq!(queue_caption(&snap), None);
        let wide = snap.shots[0].id.clone();
        let name = shot_name(&snap, Some(&wide));
        snap.switcher.queued = Some(Queued {
            shot: wide.clone(),
            take: false,
        });
        assert_eq!(queue_caption(&snap), Some(format!("next {name}")));
        snap.switcher.queued = Some(Queued {
            shot: wide,
            take: true,
        });
        assert_eq!(
            queue_caption(&snap),
            Some(format!("next {name} · take queued"))
        );
        snap.switcher.queued = Some(Queued {
            shot: ShotId("gone".into()),
            take: true,
        });
        assert_eq!(
            queue_caption(&snap).as_deref(),
            Some("next gone · take queued"),
            "a shot no longer listed is named by its id"
        );
    }

    #[test]
    fn the_switching_area_renders_headless() {
        let ctx = egui::Context::default();
        let snap = snapshot();
        let mut s = state();
        let mut textures = Textures::default();
        let mut commands = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            commands = show(ui, &snap, &mut s, &mut textures, 16.0 / 9.0, 300.0);
        });
        assert!(commands.is_empty(), "{commands:?}");
        assert_eq!(textures.len(), 0, "no monitor pictures in this snapshot");
    }
}
