//! Preview and program monitors and the transition controls between them.
//!
//! The layout follows a hardware switcher: preview on the left with a green
//! frame, program on the right with a red one, and between them Cut, Auto,
//! the transition selector and a vertical T-bar ([`super::controls`]). As
//! on a broadcast multiviewer the picture comes first: each monitor's
//! caption is laid over the top of its picture on a dark band, marked with
//! a bar of its tally colour (amber on preview while it warns of a lost
//! subject), and program draws a running transition's progress along its
//! bottom edge. A monitor with no shot on its bus says
//! so over the picture ([`empty_text`]), so black on air never looks like a
//! frozen window. The block is centred when the monitors are limited by
//! height.
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
//! end, Auto queues a take of it, and Cut cuts the transition short, landing
//! the shot the preview monitor shows. The preview caption names the
//! waiting shot and says whether a take of it is queued ([`queue_caption`]),
//! so the operator sees what will happen when the transition ends; it also
//! says when a take waits for preview to settle, and warns, in amber, when
//! the preview subject is lost ([`super::caption`]). While
//! the T-bar holds a transition the switcher refuses Cut, since the lever
//! would be left mid-travel, so CUT is disabled and says why
//! ([`cut_enabled`]); the lever finishes or abandons the transition.
//!
//! CUT and AUTO carry the preview the operator saw, which the console
//! works out ([`super::console`]), so the engine refuses them when preview
//! changed in between. While the director runs in Auto mode the program
//! monitor carries a red AUTO badge in the top left corner of its picture
//! ([`auto_badge`]), counting down while the operator's switching pauses
//! the director's takes; its hover text says what the operator's own
//! switching does to it (the takeover setting). Like a multiview tile, the
//! badge is painted, so it tells the accessibility tree its name itself
//! ([`auto_badge_name`]).

use ac_core::ShotId;
use ac_director::{DirectorMode, Takeover};
use ac_engine::{EngineCommand, EngineSnapshot};
use ac_switcher::{Transition, TransitionKind};
use eframe::egui::{
    self, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use super::caption::{caption_color, preview_notes};
use super::controls;
use super::layout::{centred, fit};
use super::text::{percent, shot_name};
use super::textures::Textures;
use super::theme::{self, PREVIEW_GREEN, PROGRAM_RED};

/// Shortest and longest selectable transition, in seconds.
pub const DURATION_RANGE: std::ops::RangeInclusive<f64> = 0.1..=5.0;
/// T-bar moves smaller than this are not sent.
const TBAR_STEP: f64 = 1e-3;
/// Height of the caption band laid over a monitor's picture, in points.
const BAND_HEIGHT: f32 = 28.0;
/// Gap between a monitor and the control column.
const MONITOR_GAP: f32 = 14.0;
/// Width of a monitor's tally border.
const BORDER: f32 = 2.0;
/// Font size of the AUTO badge, in points.
const BADGE_FONT: f32 = 13.0;
/// Padding around the AUTO badge's text, and its gap to the picture's
/// edges, in points.
const BADGE_PAD: f32 = 5.0;

/// Transition kinds with their button labels and hover texts.
pub const KINDS: [(TransitionKind, &str, &str); 3] = [
    (TransitionKind::Cut, "Cut", "Cut: no transition"),
    (TransitionKind::Mix, "Mix", "Mix: dissolve to the next shot"),
    (
        TransitionKind::Dip,
        "Dip",
        "Dip to black, then up on the next shot",
    ),
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

/// Draws the monitors and controls; returns the commands to send. CUT and
/// AUTO carry `seen`, the preview the operator saw.
pub fn show(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    seen: &Option<ShotId>,
    state: &mut SwitchState,
    textures: &mut Textures,
    aspect: f32,
    max_height: f32,
) -> Vec<EngineCommand> {
    let mut commands = Vec::new();
    let gaps = 2.0 * MONITOR_GAP;
    let width = ((ui.available_width() - controls::WIDTH - gaps) / 2.0).max(80.0);
    let size = fit(aspect, vec2(width, max_height));
    // Centre the block when the monitors are limited by height.
    let block = 2.0 * size.x + controls::WIDTH + gaps;
    let spare = (ui.available_width() - block).max(0.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = MONITOR_GAP;
        ui.add_space(spare / 2.0);
        monitor(ui, snap, textures, Monitor::Preview, size);
        ui.vertical(|ui| {
            ui.set_width(controls::WIDTH);
            controls::show(ui, snap, seen, state, size.y, &mut commands);
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

/// The monitor's caption: its bus, the shot and what is about to happen to
/// it (a transition's progress on program; the shot waiting, a take
/// waiting for preview to settle and a lost subject on preview).
fn caption(snap: &EngineSnapshot, which: Monitor) -> String {
    let (label, shot) = match which {
        Monitor::Preview => ("PREVIEW", snap.switcher.preview.as_ref()),
        Monitor::Program => ("PROGRAM", snap.switcher.program.as_ref()),
    };
    // An empty bus says so over the picture instead (`empty_text`).
    let mut caption = match shot {
        Some(_) => format!("{label}  {}", shot_name(snap, shot)),
        None => label.to_owned(),
    };
    if which == Monitor::Program
        && let Some((_, opacity)) = &snap.switcher.overlay
    {
        caption.push_str(&format!("  ·  transition {}", percent(*opacity)));
    }
    if which == Monitor::Preview {
        if let Some(queued) = queue_caption(snap) {
            caption.push_str(&format!("  ·  {queued}"));
        }
        for note in preview_notes(snap) {
            caption.push_str(&format!("  ·  {note}"));
        }
    }
    caption
}

/// What an empty monitor says: no shot on that bus.
pub fn empty_text(program: bool) -> &'static str {
    if program { "BLACK" } else { "NOTHING STAGED" }
}

/// Draws one monitor, picture first as on a broadcast multiviewer: the
/// picture in a thin tally border, and its caption laid over the top of the
/// picture on a dark band with a bar of the tally colour (amber on preview
/// while it warns of a lost subject). Program shows a running transition's
/// progress as a red line along its bottom edge.
fn monitor(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    textures: &mut Textures,
    which: Monitor,
    size: Vec2,
) {
    let (color, frame, shot, name) = match which {
        Monitor::Preview => (
            PREVIEW_GREEN,
            snap.preview_monitor.as_ref(),
            snap.switcher.preview.as_ref(),
            "monitor/preview",
        ),
        Monitor::Program => (
            PROGRAM_RED,
            snap.program_monitor.as_ref(),
            snap.switcher.program.as_ref(),
            "monitor/program",
        ),
    };
    let warn = match which {
        Monitor::Preview => caption_color(snap, color) != color,
        Monitor::Program => false,
    };
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect.expand(BORDER));
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::BLACK);
    if let Some((id, px)) = textures.show(ui.ctx(), name, frame) {
        let inner = centred(rect, fit(px.x / px.y, rect.size()));
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(id, inner, uv, Color32::WHITE);
    }
    if shot.is_none() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            empty_text(which == Monitor::Program),
            theme::semibold_font(ui, theme::BODY),
            theme::TEXT_WEAK,
        );
    }
    // The caption band over the top of the picture.
    let band = Rect::from_min_size(rect.min, vec2(rect.width(), BAND_HEIGHT));
    painter.rect_filled(band, CornerRadius::ZERO, theme::SCRIM);
    let bar = if warn { theme::AMBER } else { color };
    painter.rect_filled(
        Rect::from_min_size(band.min, vec2(4.0, band.height())),
        CornerRadius::ZERO,
        bar,
    );
    let text = theme::semibold(ui, caption(snap, which), theme::BODY).color(if warn {
        theme::AMBER
    } else {
        theme::TEXT_STRONG
    });
    let label = Rect::from_min_max(pos2(band.left() + 12.0, band.top()), band.right_bottom());
    // A child that the row's layout does not see, laid over the picture.
    let mut over = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(label)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    over.add(egui::Label::new(text).truncate());
    if which == Monitor::Program
        && let Some((_, opacity)) = &snap.switcher.overlay
    {
        let done = rect.width() * (*opacity as f32).clamp(0.0, 1.0);
        let line = Rect::from_min_size(pos2(rect.left(), rect.bottom() - 4.0), vec2(done, 4.0));
        painter.rect_filled(line, CornerRadius::ZERO, PROGRAM_RED);
    }
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(BORDER, color),
        StrokeKind::Outside,
    );
    if which == Monitor::Program
        && let Some(text) = auto_badge(snap)
    {
        let below = Rect::from_min_max(pos2(rect.left(), band.bottom()), rect.max);
        draw_auto_badge(ui, below, &text, snap);
    }
}

/// The AUTO badge's text while the director runs in Auto mode: `AUTO`, or
/// `AUTO · paused 7 s` while the operator's switching pauses its takes
/// (whole seconds, rounded up); `None` in the other modes.
pub fn auto_badge(snap: &EngineSnapshot) -> Option<String> {
    let d = &snap.director;
    if d.mode != DirectorMode::Auto {
        return None;
    }
    Some(match d.paused {
        Some(left) => format!("AUTO · paused {:.0} s", left.as_secs_f64().ceil()),
        None => "AUTO".into(),
    })
}

/// The accessible name of the AUTO badge whose text is `text`, which says
/// whose badge it is ("program AUTO"), so it is not taken for the AUTO
/// button.
pub fn auto_badge_name(text: &str) -> String {
    format!("program {text}")
}

/// The AUTO badge's hover text: what the operator's own switching does to
/// the director, by the takeover setting.
pub fn auto_badge_hover(snap: &EngineSnapshot) -> String {
    let d = &snap.director;
    let what = match d.takeover {
        Takeover::Suggest => "Any switching of yours (a key, a click, the T-bar) drops it to \
                              Suggest."
            .to_owned(),
        Takeover::Pause => format!(
            "Any switching of yours pauses its takes for {:.0} s.",
            d.pause.as_secs_f64()
        ),
        Takeover::Director => "It goes on switching whatever you do, and stages its own \
                               shot over yours."
            .to_owned(),
    };
    format!("The auto-director takes shots by itself. {what}")
}

/// Paints the AUTO badge `text` in the top left corner of the program
/// `picture` and names it for the accessibility tree.
fn draw_auto_badge(ui: &Ui, picture: Rect, text: &str, snap: &EngineSnapshot) {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        theme::semibold_font(ui, BADGE_FONT),
        Color32::WHITE,
    );
    let badge = Rect::from_min_size(
        picture.left_top() + Vec2::splat(BADGE_PAD),
        galley.size() + Vec2::splat(2.0 * BADGE_PAD),
    );
    let painter = ui.painter_at(picture);
    painter.rect_filled(badge, CornerRadius::same(3), PROGRAM_RED);
    painter.galley(badge.min + Vec2::splat(BADGE_PAD), galley, Color32::WHITE);
    let response = ui.interact(badge, ui.id().with("auto-badge"), Sense::hover());
    let name = auto_badge_name(text);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &name));
    response.on_hover_text(auto_badge_hover(snap));
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

/// Hover text of a CUT button disabled because the T-bar holds the
/// transition.
pub const TBAR_HOLDS: &str = "The T-bar holds the transition: finish it or pull it back";

/// Whether CUT can be pressed: something is on preview and the T-bar does
/// not hold the running transition (see the module docs).
pub fn cut_enabled(snap: &EngineSnapshot) -> bool {
    snap.switcher.preview.is_some() && !snap.switcher.tbar_held
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
    fn every_kind_has_a_label_and_a_hover() {
        for (i, (k, l, hover)) in KINDS.iter().enumerate() {
            assert!(!l.is_empty() && !hover.is_empty());
            assert!(KINDS[..i].iter().all(|(other, ..)| other != k), "unique");
        }
        assert_eq!(empty_text(true), "BLACK");
        assert_eq!(empty_text(false), "NOTHING STAGED");
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
    fn cut_is_disabled_while_the_tbar_holds_the_transition() {
        let mut snap = snapshot();
        assert!(!cut_enabled(&snap), "nothing on preview");
        snap.switcher.preview = Some(snap.shots[0].id.clone());
        assert!(cut_enabled(&snap));
        snap.switcher.in_transition = true;
        assert!(cut_enabled(&snap), "an automatic transition is cut short");
        snap.switcher.tbar_held = true;
        assert!(!cut_enabled(&snap));
    }

    #[test]
    fn the_auto_badge_shows_in_auto_mode_and_counts_a_pause_down() {
        let mut snap = snapshot();
        assert_eq!(snap.director.mode, DirectorMode::Suggest);
        assert_eq!(auto_badge(&snap), None);
        snap.director.mode = DirectorMode::Auto;
        assert_eq!(auto_badge(&snap).as_deref(), Some("AUTO"));
        assert_eq!(auto_badge_name("AUTO"), "program AUTO");
        snap.director.paused = Some(std::time::Duration::from_millis(6200));
        assert_eq!(auto_badge(&snap).as_deref(), Some("AUTO · paused 7 s"));
        snap.director.mode = DirectorMode::Off;
        assert_eq!(auto_badge(&snap), None);
    }

    #[test]
    fn the_auto_badge_says_what_the_operators_switching_does() {
        let mut snap = snapshot();
        assert!(auto_badge_hover(&snap).contains("drops it to Suggest"));
        snap.director.takeover = Takeover::Pause;
        snap.director.pause = std::time::Duration::from_secs(12);
        assert!(auto_badge_hover(&snap).contains("pauses its takes for 12 s"));
        snap.director.takeover = Takeover::Director;
        assert!(auto_badge_hover(&snap).contains("over yours"));
    }

    #[test]
    fn the_switching_area_renders_headless() {
        let ctx = egui::Context::default();
        let snap = snapshot();
        let mut s = state();
        let mut textures = Textures::default();
        let mut commands = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            commands = show(ui, &snap, &None, &mut s, &mut textures, 16.0 / 9.0, 300.0);
        });
        assert!(commands.is_empty(), "{commands:?}");
        assert_eq!(textures.len(), 0, "no monitor pictures in this snapshot");
    }
}
