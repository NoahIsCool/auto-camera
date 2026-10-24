//! The multiview: every shot as a tile with tally, status and quality.
//!
//! Tiles follow broadcast multiview conventions: a **red** border marks a
//! shot visible on program (both shots during a mix), **green** the shot
//! staged on preview, and an unavailable shot is dimmed with "no signal".
//! The caption carries the tile's shortcut number (1–9), the shot name, its
//! tracking status and a quality bar.
//!
//! Interaction: a click stages the shot on preview; a double click stages
//! and cuts it to program. The right-click menu of a subject shot pins,
//! hides or re-acquires its subject. Clicks become [`EngineCommand`]s that
//! the caller sends; this module never talks to the engine itself.

use ac_core::ShotId;
use ac_director::CandidateKind;
use ac_engine::{EngineCommand, EngineSnapshot, ShotView, SubjectView, Tally};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use super::keys::tile_label;
use super::layout::{CAPTION, grid};
use super::text::shot_status;
use super::textures::Textures;

/// Program tally colour.
pub const PROGRAM_RED: Color32 = Color32::from_rgb(230, 40, 40);
/// Preview tally colour.
pub const PREVIEW_GREEN: Color32 = Color32::from_rgb(40, 200, 80);
/// Border of a tile with no tally.
const IDLE_BORDER: Color32 = Color32::from_gray(70);
/// Width of a tally border.
const TALLY_WIDTH: f32 = 3.0;

/// The tally colour of a tile, program taking precedence.
pub fn tally_color(tally: Tally) -> Option<Color32> {
    if tally.program {
        Some(PROGRAM_RED)
    } else if tally.preview {
        Some(PREVIEW_GREEN)
    } else {
        None
    }
}

/// Commands for a click (`clicked`) or double click on the tile of `shot`.
///
/// egui reports the first click of a double click as a click too, so a
/// double click arrives as a preview on one frame and preview + cut on the
/// next; staging again first keeps the cut correct even if the first click
/// was missed.
pub fn click_commands(shot: &ShotId, clicked: bool, double_clicked: bool) -> Vec<EngineCommand> {
    let preview = EngineCommand::SetPreview { shot: shot.clone() };
    if double_clicked {
        vec![preview, EngineCommand::Cut]
    } else if clicked {
        vec![preview]
    } else {
        Vec::new()
    }
}

/// The subject a shot frames, if it is a subject shot.
fn subject_of<'a>(snap: &'a EngineSnapshot, shot: &ShotView) -> Option<&'a SubjectView> {
    let id = shot.subject?;
    snap.subjects
        .iter()
        .find(|s| s.source == shot.source && s.id == id)
}

/// Draws the multiview filling the space left in `ui`; returns the commands
/// the operator's clicks asked for.
pub fn show(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    textures: &mut Textures,
    aspect: f32,
) -> Vec<EngineCommand> {
    let mut commands = Vec::new();
    if snap.shots.is_empty() {
        ui.label("No shots yet: waiting for sources.");
        return commands;
    }
    let scrollbar = ui.spacing().scroll.bar_width + 4.0;
    let area = ui.available_size() - vec2(scrollbar, 0.0);
    let g = grid(snap.shots.len(), area, aspect);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            for (i, shot) in snap.shots.iter().enumerate() {
                let tile = g.tile_rect(rect.min, i);
                let picture = g.picture_rect(tile);
                let response =
                    ui.interact(tile, ui.id().with(("tile", &shot.id.0)), Sense::click());
                draw_tile(ui, shot, i, tile, picture, textures, response.hovered());
                commands.extend(click_commands(
                    &shot.id,
                    response.clicked(),
                    response.double_clicked(),
                ));
                if shot.kind == CandidateKind::Subject {
                    response.context_menu(|ui| subject_menu(ui, snap, shot, &mut commands));
                }
                response
                    .on_hover_text(format!("{}\nclick: preview · double-click: cut", shot.id.0));
            }
        });
    commands
}

/// Paints one tile.
fn draw_tile(
    ui: &Ui,
    shot: &ShotView,
    index: usize,
    tile: Rect,
    picture: Rect,
    textures: &mut Textures,
    hovered: bool,
) {
    let painter = ui.painter_at(tile.expand(TALLY_WIDTH));
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    match textures.show(
        ui.ctx(),
        &format!("tile/{}", shot.id.0),
        shot.thumbnail.as_ref(),
    ) {
        Some((id, _)) => {
            painter.image(id, picture, uv, Color32::WHITE);
        }
        None => {
            painter.rect_filled(picture, CornerRadius::ZERO, Color32::from_gray(24));
        }
    }
    if !shot.available {
        painter.rect_filled(picture, CornerRadius::ZERO, Color32::from_black_alpha(170));
        painter.text(
            picture.center(),
            Align2::CENTER_CENTER,
            "NO SIGNAL",
            FontId::proportional(14.0),
            Color32::LIGHT_GRAY,
        );
    }
    let caption = Rect::from_min_size(picture.left_bottom(), vec2(picture.width(), CAPTION));
    painter.rect_filled(caption, CornerRadius::ZERO, Color32::from_gray(30));
    let label = match tile_label(index) {
        Some(n) => format!("{n}  {}", shot.name),
        None => shot.name.clone(),
    };
    let text = ui.visuals().strong_text_color();
    painter.text(
        caption.left_top() + vec2(6.0, 3.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(13.0),
        text,
    );
    painter.text(
        caption.left_bottom() + vec2(6.0, -4.0),
        Align2::LEFT_BOTTOM,
        shot_status(shot),
        FontId::proportional(11.0),
        ui.visuals().weak_text_color(),
    );
    // Quality bar along the bottom-right of the caption.
    let bar = Rect::from_min_size(caption.right_bottom() + vec2(-66.0, -10.0), vec2(60.0, 5.0));
    painter.rect_filled(bar, CornerRadius::same(2), Color32::from_gray(55));
    let q = shot.quality.clamp(0.0, 1.0);
    let fill = Rect::from_min_size(bar.min, vec2(bar.width() * q, bar.height()));
    let color = if q >= 0.6 {
        PREVIEW_GREEN
    } else if q >= 0.3 {
        Color32::from_rgb(230, 180, 40)
    } else {
        PROGRAM_RED
    };
    painter.rect_filled(fill, CornerRadius::same(2), color);
    let (width, border) = match tally_color(shot.tally) {
        Some(c) => (TALLY_WIDTH, c),
        None if hovered => (1.5, Color32::from_gray(160)),
        None => (1.0, IDLE_BORDER),
    };
    painter.rect_stroke(
        tile,
        CornerRadius::same(2),
        Stroke::new(width, border),
        StrokeKind::Outside,
    );
}

/// The right-click menu of a subject shot.
fn subject_menu(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    shot: &ShotView,
    commands: &mut Vec<EngineCommand>,
) {
    let Some(subject) = subject_of(snap, shot) else {
        ui.label("subject gone");
        return;
    };
    ui.label(format!("Subject {} ({})", subject.id.0, subject.query.0));
    let pin = if subject.pinned {
        "Unpin"
    } else {
        "Pin (never retire)"
    };
    if ui.button(pin).clicked() {
        commands.push(EngineCommand::Pin {
            source: subject.source.clone(),
            subject: subject.id,
            pinned: !subject.pinned,
        });
        ui.close();
    }
    if ui.button("Hide its shots").clicked() {
        commands.push(EngineCommand::Hide {
            source: subject.source.clone(),
            subject: subject.id,
            hidden: true,
        });
        ui.close();
    }
    if ui
        .button("Re-acquire at last box")
        .on_hover_text("Restart the tracker on the subject's last known box")
        .clicked()
    {
        commands.push(EngineCommand::Reacquire {
            source: subject.source.clone(),
            subject: subject.id,
            bbox: subject.bbox,
        });
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::snapshot;

    #[test]
    fn program_tally_wins_over_preview() {
        let both = Tally {
            program: true,
            preview: true,
        };
        assert_eq!(tally_color(both), Some(PROGRAM_RED));
        let pvw = Tally {
            program: false,
            preview: true,
        };
        assert_eq!(tally_color(pvw), Some(PREVIEW_GREEN));
        assert_eq!(tally_color(Tally::default()), None);
    }

    #[test]
    fn clicks_preview_and_double_clicks_cut() {
        let s = ShotId("cam/wide".into());
        assert!(click_commands(&s, false, false).is_empty());
        assert_eq!(
            click_commands(&s, true, false),
            [EngineCommand::SetPreview { shot: s.clone() }]
        );
        assert_eq!(
            click_commands(&s, true, true),
            [
                EngineCommand::SetPreview { shot: s.clone() },
                EngineCommand::Cut
            ]
        );
    }

    #[test]
    fn wides_have_no_subject() {
        let snap = snapshot();
        assert!(subject_of(&snap, &snap.shots[0]).is_none());
    }

    #[test]
    fn the_multiview_renders_headless() {
        // A full egui frame without a window: layout, painting and the
        // texture upload path all run.
        let ctx = egui::Context::default();
        let mut snap = snapshot();
        snap.shots[0].thumbnail = Some(
            ac_media::VideoFrame::new(
                snap.sources[0].id.clone(),
                0,
                std::time::Duration::ZERO,
                4,
                2,
                vec![0; 32].into(),
            )
            .unwrap(),
        );
        let mut textures = Textures::default();
        let mut commands = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            commands = show(ui, &snap, &mut textures, 16.0 / 9.0);
        });
        assert!(commands.is_empty());
        assert_eq!(textures.len(), 1);
    }
}
