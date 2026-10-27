//! The multiview: every shot as a tile with tally, status and quality.
//!
//! Tiles follow broadcast multiview conventions: a **red** border marks a
//! shot visible on program (both shots during a mix), **green** the shot
//! staged on preview, and an unavailable shot is dimmed with "no signal".
//! The caption carries the tile's shortcut number (1–9), the shot name, its
//! tracking status and a quality bar. A shot that its preset's zoom ceiling
//! keeps looser than its size carries an amber [`LIMITED_BADGE`] in the top
//! right corner of its picture, never in the caption: a caption is clipped
//! to its tile, and on a narrow tile a long name would push a trailing
//! "(limited)" out of sight. The badge's width does not depend on the name
//! and fits the narrowest tile ([`MIN_TILE_WIDTH`]). The hover text says why
//! the shot is limited and, when it looks the same as another of its
//! subject's shots, which one.
//!
//! Interaction: a click stages the shot on preview; a double click cuts it
//! to program at once ("Cut now", [`EngineCommand::CutTo`]). During a
//! transition a click stages the shot for its end, and a double click still
//! cuts it to program at once, ending the transition in the same command,
//! so the transition's incoming shot never shows alone in between. The
//! right-click menu of a subject shot pins, hides or re-acquires its
//! subject. Clicks become [`EngineCommand`]s that the caller sends; this
//! module never talks to the engine itself.

use ac_core::ShotId;
use ac_director::CandidateKind;
use ac_engine::{EngineCommand, EngineSnapshot, ShotView, SubjectView, Tally};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Painter, Rect, Sense, Stroke, StrokeKind, Ui,
    Vec2, pos2, vec2,
};

use super::keys::tile_label;
#[cfg(doc)]
use super::layout::MIN_TILE_WIDTH;
use super::layout::{CAPTION, grid};
use super::text::{LIMITED_BADGE, shot_hover, shot_status};
use super::textures::Textures;

/// Program tally colour.
pub const PROGRAM_RED: Color32 = Color32::from_rgb(230, 40, 40);
/// Preview tally colour.
pub const PREVIEW_GREEN: Color32 = Color32::from_rgb(40, 200, 80);
/// Border of a tile with no tally.
const IDLE_BORDER: Color32 = Color32::from_gray(70);
/// Width of a tally border.
const TALLY_WIDTH: f32 = 3.0;
/// Colour of the zoom-limit badge.
const BADGE_AMBER: Color32 = Color32::from_rgb(230, 160, 40);
/// Font size of the zoom-limit badge, in points.
const BADGE_FONT: f32 = 10.0;
/// Padding around the badge's text, in points.
const BADGE_PAD: f32 = 3.0;
/// Gap between the badge and the picture's edges, in points.
const BADGE_MARGIN: f32 = 4.0;

/// Where a badge whose text is `text_size` sits on `picture`: its top
/// right corner, inset by the margin.
pub fn badge_rect(picture: Rect, text_size: Vec2) -> Rect {
    let size = text_size + Vec2::splat(2.0 * BADGE_PAD);
    let min = pos2(
        picture.right() - BADGE_MARGIN - size.x,
        picture.top() + BADGE_MARGIN,
    );
    Rect::from_min_size(min, size)
}

/// Paints the zoom-limit badge on `picture`.
fn draw_badge(painter: &Painter, picture: Rect) {
    let galley = painter.layout_no_wrap(
        LIMITED_BADGE.into(),
        FontId::proportional(BADGE_FONT),
        Color32::BLACK,
    );
    let rect = badge_rect(picture, galley.size());
    painter.rect_filled(rect, CornerRadius::same(2), BADGE_AMBER);
    painter.galley(rect.min + Vec2::splat(BADGE_PAD), galley, Color32::BLACK);
}

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
/// double click arrives as a preview on one frame and a cut on the next.
/// The cut names its shot, so it is correct even if the first click was
/// missed, and it is one command: the engine may tick between two commands
/// of one frame.
pub fn click_commands(shot: &ShotId, clicked: bool, double_clicked: bool) -> Vec<EngineCommand> {
    if double_clicked {
        vec![EngineCommand::CutTo { shot: shot.clone() }]
    } else if clicked {
        vec![EngineCommand::SetPreview { shot: shot.clone() }]
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
                response.on_hover_text(shot_hover(snap, shot));
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
    if shot.limited_by_zoom {
        draw_badge(&painter, picture);
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
    use crate::ui::layout::MIN_TILE_WIDTH;

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
            [EngineCommand::CutTo { shot: s.clone() }]
        );
        assert_eq!(
            click_commands(&s, false, true),
            [EngineCommand::CutTo { shot: s.clone() }],
            "the first click was missed"
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

    /// Every text the frame painted, with its on-screen rectangle and the
    /// clip rectangle it was painted under.
    fn painted_texts(out: &egui::FullOutput) -> Vec<(String, Rect, Rect)> {
        out.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_string(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                    c.clip_rect,
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_limited_badge_is_whole_on_the_narrowest_tile() {
        // Many shots with long names in a narrow window: tiles at their
        // minimum width, captions clipped, the badge whole.
        let mut snap = snapshot();
        for i in 0..12 {
            let mut s = crate::ui::fixtures::subject_shot(
                &format!("stage/{i}/medium_close"),
                ac_engine::ShotStatus::Tracking,
                0.9,
            );
            s.name = format!("Speaker {i} Medium close with a long name");
            s.limited_by_zoom = true;
            snap.shots.push(s);
        }
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 900.0))),
            ..Default::default()
        };
        let mut textures = Textures::default();
        let out = ctx.run_ui(input, |ui| {
            show(ui, &snap, &mut textures, 16.0 / 9.0);
        });
        let texts = painted_texts(&out);
        // Tiles scrolled out of view are clipped away entirely.
        let badges: Vec<&(String, Rect, Rect)> = texts
            .iter()
            .filter(|(t, _, clip)| t == LIMITED_BADGE && clip.is_positive())
            .collect();
        assert!(!badges.is_empty());
        for (_, rect, clip) in &badges {
            assert!(clip.contains_rect(*rect), "{rect:?} clipped by {clip:?}");
        }
        assert!(
            texts.iter().all(|(t, _, _)| !t.contains("(limited)")),
            "the caption carries no mark that could be clipped"
        );
        // Whatever the name, the badge takes the same room, well inside the
        // narrowest tile.
        let size = badges[0].1.size() + Vec2::splat(2.0 * BADGE_PAD);
        let picture = Rect::from_min_size(pos2(0.0, 0.0), vec2(MIN_TILE_WIDTH, 68.0));
        let badge = badge_rect(picture, badges[0].1.size());
        assert!(picture.contains_rect(badge), "{badge:?}");
        assert!(
            size.x + 2.0 * BADGE_MARGIN <= MIN_TILE_WIDTH / 2.0,
            "{size:?}"
        );
    }
}
