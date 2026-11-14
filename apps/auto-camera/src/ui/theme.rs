//! The operator window's look: one palette, one typeface, one spacing.
//!
//! The window follows the conventions of broadcast control software (vMix,
//! ATEM Software Control, OBS): near-black neutral chrome so the pictures
//! carry the colour, labels laid over the pictures rather than beside
//! them, small capitals for section headings, and colour only where it
//! means something:
//!
//! * [`PROGRAM_RED`]: on air (the program monitor, program tally, ON AIR,
//!   REC, alarms);
//! * [`PREVIEW_GREEN`]: staged (the preview monitor and preview tally);
//! * [`AMBER`]: needs a look (warnings, LIMITED, a lost subject);
//! * [`ACCENT`]: the operator's own selection (a chosen mode or
//!   transition) and the primary action of a section.
//!
//! The window is always dark: a control room is dim, and a light window
//! would glare beside the monitors. It does not follow the system's light
//! mode.
//!
//! Text is Inter, bundled into the binary ([`install_fonts`]), so the
//! window looks the same on every machine instead of falling back to
//! egui's thin default face; [`SEMIBOLD`] is its heavier weight, for
//! headings, keys and names. Numbers that change every frame stay
//! monospaced (egui's Hack), so they do not jitter.
//!
//! Every text colour is chosen against the surfaces it sits on, and the
//! tests hold the palette to WCAG contrast ratios ([`contrast`]): body text
//! at least 7:1 on every surface, weak text and warnings 4.5:1, and the
//! text on a signal colour 4.5:1 too. [`apply`] installs the fonts, the
//! palette and the type scale once, as the window opens; the drawing code
//! takes its colours and text styles from here.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText,
    Stroke, TextStyle, Theme, Vec2, style::Selection,
};

/// The central area behind the monitors and the multiview.
pub const BACKGROUND: Color32 = Color32::from_rgb(12, 13, 15);
/// Side panel, header and status bar.
pub const PANEL: Color32 = Color32::from_rgb(22, 24, 27);
/// An idle widget, and a raised area inside a panel.
pub const SURFACE: Color32 = Color32::from_rgb(32, 35, 40);
/// A hovered widget.
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(42, 46, 52);
/// A pressed widget, and an idle switcher key.
pub const SURFACE_ACTIVE: Color32 = Color32::from_rgb(52, 57, 64);
/// Text fields and other wells, darker than the panel they sit in.
pub const WELL: Color32 = Color32::from_rgb(10, 11, 13);
/// Hairlines between areas.
pub const BORDER: Color32 = Color32::from_rgb(40, 44, 50);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(222, 225, 230);
/// Headings, names and emphasis.
pub const TEXT_STRONG: Color32 = Color32::from_rgb(248, 249, 250);
/// Secondary text: explanations, units, idle states.
pub const TEXT_WEAK: Color32 = Color32::from_rgb(166, 172, 181);
/// Program: on air.
pub const PROGRAM_RED: Color32 = Color32::from_rgb(220, 38, 38);
/// Red text (alarms, failures): lighter than [`PROGRAM_RED`], which is
/// too dark to read as text on the panels.
pub const RED_TEXT: Color32 = Color32::from_rgb(252, 129, 129);
/// Preview: staged.
pub const PREVIEW_GREEN: Color32 = Color32::from_rgb(34, 197, 94);
/// Warnings that need a look but are not on air.
pub const AMBER: Color32 = Color32::from_rgb(245, 158, 11);
/// The operator's own selection.
pub const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);
/// Text on [`PREVIEW_GREEN`] and [`AMBER`], which are too light for white.
pub const ON_LIGHT: Color32 = Color32::from_rgb(10, 20, 14);
/// Text on [`PROGRAM_RED`] and [`ACCENT`].
pub const ON_DARK: Color32 = Color32::WHITE;
/// The translucent band laid over a picture under its label.
pub const SCRIM: Color32 = Color32::from_black_alpha(170);

/// Body text size, in points.
pub const BODY: f32 = 13.5;
/// Small print: units, hints, tile status lines, section headings.
pub const SMALL: f32 = 11.0;
/// Larger names: the director's suggestion, the window's title.
pub const LARGE: f32 = 15.0;
/// Monospaced numbers (encoder statistics, meters).
pub const MONO: f32 = 12.0;
/// The switcher's keys (CUT, AUTO).
pub const KEY: f32 = 15.0;

/// Corner radius of buttons and fields.
pub const RADIUS: u8 = 4;
/// Corner radius of cards, keys and dialogs.
pub const CARD_RADIUS: u8 = 6;

/// The heavier face, for headings, keys and names.
pub const SEMIBOLD: &str = "semibold";

/// Inter Regular, the body face.
const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.otf");
/// Inter SemiBold, [`SEMIBOLD`].
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.otf");

/// Installs the fonts, the palette, the type scale and the spacing in
/// `ctx`, for the dark theme, and pins the window to that theme.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, style);
}

/// Makes Inter the proportional face and adds [`SEMIBOLD`], each falling
/// back to egui's own fonts for the glyphs Inter lacks.
pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
}

/// The font definitions [`install_fonts`] installs.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        Arc::new(FontData::from_static(INTER_REGULAR)),
    );
    fonts.font_data.insert(
        "inter-semibold".into(),
        Arc::new(FontData::from_static(INTER_SEMIBOLD)),
    );
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        list.insert(0, "inter".into());
    }
    let mut semibold = vec!["inter-semibold".to_owned()];
    semibold.extend(fallback);
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold);
    fonts
}

/// Rewrites `style` to the operator window's look (see the module docs).
pub fn style(style: &mut egui::Style) {
    use FontFamily::{Monospace, Proportional};
    style.text_styles = [
        (TextStyle::Small, FontId::new(SMALL, Proportional)),
        (TextStyle::Body, FontId::new(BODY, Proportional)),
        (TextStyle::Button, FontId::new(BODY, Proportional)),
        (
            TextStyle::Heading,
            FontId::new(LARGE, FontFamily::Name(SEMIBOLD.into())),
        ),
        (TextStyle::Monospace, FontId::new(MONO, Monospace)),
    ]
    .into();
    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(8.0, 6.0);
    s.button_padding = Vec2::new(10.0, 4.0);
    s.interact_size = Vec2::new(40.0, 26.0);
    s.window_margin = Margin::same(16);
    s.menu_margin = Margin::same(6);
    s.indent = 16.0;
    s.combo_height = 240.0;
    s.text_edit_width = 220.0;
    s.scroll.bar_width = 6.0;
    s.scroll.floating = true;
    visuals(&mut style.visuals);
}

/// Rewrites `v` to the operator window's palette.
fn visuals(v: &mut egui::Visuals) {
    *v = egui::Visuals::dark();
    v.override_text_color = None;
    v.weak_text_color = Some(TEXT_WEAK);
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.menu_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.extreme_bg_color = WELL;
    v.text_edit_bg_color = Some(WELL);
    v.faint_bg_color = SURFACE;
    v.code_bg_color = WELL;
    v.warn_fg_color = AMBER;
    v.error_fg_color = RED_TEXT;
    v.hyperlink_color = Color32::from_rgb(96, 165, 250);
    v.selection = Selection {
        bg_fill: ACCENT,
        stroke: Stroke::new(1.0, ON_DARK),
    };
    v.slider_trailing_fill = true;
    let radius = CornerRadius::same(RADIUS);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = radius;
    for (state, fill, text) in [
        (&mut w.inactive, SURFACE, TEXT),
        (&mut w.hovered, SURFACE_HOVER, TEXT_STRONG),
        (&mut w.active, SURFACE_ACTIVE, TEXT_STRONG),
        (&mut w.open, SURFACE_HOVER, TEXT_STRONG),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.fg_stroke = Stroke::new(1.0, text);
        state.corner_radius = radius;
        state.expansion = 0.0;
    }
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(78, 86, 98));
    w.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(110, 120, 135));
    w.open.bg_stroke = Stroke::new(1.0, BORDER);
}

/// Installs the look ([`apply`]) unless `ctx` has it already, and says
/// whether its fonts are live on this frame. egui switches fonts at the
/// start of a frame, so on the frame that installs them [`semibold`] text
/// falls back to the body face, and the frame is drawn again at once.
pub fn ensure(ctx: &egui::Context) -> bool {
    if has_semibold(ctx) {
        return true;
    }
    apply(ctx);
    ctx.request_repaint();
    false
}

/// Whether `ctx`'s live fonts have the [`SEMIBOLD`] face.
fn has_semibold(ctx: &egui::Context) -> bool {
    let family = FontFamily::Name(SEMIBOLD.into());
    ctx.fonts(|f| f.definitions().families.contains_key(&family))
}

/// `text` in the heavier face ([`SEMIBOLD`]) at `size`, or in the body
/// face while `ui`'s fonts do not have it yet (see [`ensure`]).
pub fn semibold(ui: &egui::Ui, text: impl Into<String>, size: f32) -> RichText {
    let text = RichText::new(text).size(size);
    if has_semibold(ui.ctx()) {
        text.family(FontFamily::Name(SEMIBOLD.into()))
    } else {
        text.strong()
    }
}

/// The heavier face at `size` for painted text, or the body face while
/// `ui`'s fonts do not have it yet (see [`ensure`]).
pub fn semibold_font(ui: &egui::Ui, size: f32) -> FontId {
    if has_semibold(ui.ctx()) {
        FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
    } else {
        FontId::proportional(size)
    }
}

/// A section heading: small capitals, spaced out, weak.
pub fn caps(ui: &egui::Ui, text: &str) -> RichText {
    semibold(ui, text.to_uppercase(), SMALL)
        .extra_letter_spacing(0.8)
        .color(TEXT_WEAK)
}

/// The text colour that reads on a `fill` of the palette: dark on the light
/// signal colours (preview green, amber), white on the rest.
pub fn text_on(fill: Color32) -> Color32 {
    if luminance(fill) > 0.25 {
        ON_LIGHT
    } else {
        ON_DARK
    }
}

/// A side-panel section: a small-capitals heading over its contents, and a
/// hairline under them, like an inspector in a video editor.
pub fn section<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let inner = egui::Frame::new()
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 12,
            bottom: 14,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(caps(ui, title));
            ui.add_space(4.0);
            add(ui)
        })
        .inner;
    let rect = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter()
        .hline(rect.x_range(), y, Stroke::new(1.0, BORDER));
    inner
}

/// A button for a section's primary action, filled with the accent.
pub fn primary(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    egui::Button::new(semibold(ui, text, BODY).color(text_on(ACCENT))).fill(ACCENT)
}

/// The WCAG 2 contrast ratio of two colours (1 to 21), which the tests
/// hold the palette to.
#[cfg(test)]
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// The WCAG 2 relative luminance of an opaque colour (0 to 1).
fn luminance(c: Color32) -> f32 {
    /// One sRGB channel, linearised.
    fn channel(v: u8) -> f32 {
        let v = f32::from(v) / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every surface text can sit on.
    const SURFACES: [Color32; 6] = [
        BACKGROUND,
        PANEL,
        SURFACE,
        SURFACE_HOVER,
        SURFACE_ACTIVE,
        WELL,
    ];

    #[test]
    fn contrast_follows_wcag() {
        assert!((contrast(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
        assert!((contrast(TEXT, TEXT) - 1.0).abs() < 1e-6);
        assert_eq!(contrast(TEXT, PANEL), contrast(PANEL, TEXT), "symmetric");
    }

    #[test]
    fn text_reads_on_every_surface() {
        for surface in SURFACES {
            assert!(contrast(TEXT, surface) >= 7.0, "{surface:?}");
            assert!(contrast(TEXT_STRONG, surface) >= 7.0, "{surface:?}");
            assert!(contrast(TEXT_WEAK, surface) >= 4.5, "{surface:?}");
            // Warnings and alarms are text too.
            assert!(contrast(AMBER, surface) >= 4.5, "{surface:?}");
            assert!(contrast(RED_TEXT, surface) >= 4.5, "{surface:?}");
        }
    }

    #[test]
    fn text_reads_on_every_signal_colour() {
        for fill in [PROGRAM_RED, PREVIEW_GREEN, AMBER, ACCENT] {
            let ratio = contrast(text_on(fill), fill);
            assert!(ratio >= 4.5, "{fill:?}: {ratio}");
        }
        assert_eq!(text_on(PREVIEW_GREEN), ON_LIGHT);
        assert_eq!(text_on(PROGRAM_RED), ON_DARK);
    }

    #[test]
    fn the_tallies_stand_out_from_the_background() {
        // Tally borders are thin lines: WCAG's 3:1 for graphics.
        for tally in [PROGRAM_RED, PREVIEW_GREEN, AMBER] {
            assert!(contrast(tally, BACKGROUND) >= 3.0, "{tally:?}");
        }
    }

    #[test]
    fn inter_is_the_face_and_falls_back_to_egui_fonts() {
        let fonts = fonts();
        let body = &fonts.families[&FontFamily::Proportional];
        assert_eq!(body[0], "inter");
        assert!(body.len() > 1, "egui's fonts cover what Inter lacks");
        let semibold = &fonts.families[&FontFamily::Name(SEMIBOLD.into())];
        assert_eq!(semibold[0], "inter-semibold");
        assert_eq!(&semibold[1..], &body[1..]);
    }

    #[test]
    fn ensure_installs_the_look_once_and_semibold_waits_for_it() {
        let ctx = egui::Context::default();
        let mut live = Vec::new();
        for _ in 0..3 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                live.push(ensure(ui.ctx()));
                // Never panics, whether the face is live yet or not.
                ui.label(semibold(ui, "AUTO", KEY));
            });
        }
        assert_eq!(live, [false, true, true]);
        assert_eq!(ctx.theme(), Theme::Dark);
    }

    #[test]
    fn apply_pins_the_dark_theme_and_the_type_scale() {
        let ctx = egui::Context::default();
        ctx.set_theme(Theme::Light);
        apply(&ctx);
        assert_eq!(ctx.theme(), Theme::Dark);
        let style = ctx.style_of(Theme::Dark);
        assert_eq!(style.text_styles[&TextStyle::Body].size, BODY);
        assert_eq!(style.visuals.panel_fill, PANEL);
        assert_eq!(style.visuals.selection.bg_fill, ACCENT);
        assert!(style.visuals.dark_mode);
        // The fonts load: a frame lays text out in them.
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label(semibold(ui, "CUT", KEY));
            ui.label(caps(ui, "Director"));
        });
    }
}
