//! The operator window's look: one palette, one type scale, one spacing.
//!
//! A control room is dim, the operator sits back from the screen, and the
//! picture is what matters, so the window is always dark (it does not follow
//! the system's light mode) and quiet: neutral greys for chrome, colour only
//! where it means something. Colour carries the broadcast conventions and
//! nothing else:
//!
//! * [`PROGRAM_RED`]: on air (the program monitor, program tally, ON AIR,
//!   REC, alarms);
//! * [`PREVIEW_GREEN`]: staged (the preview monitor and preview tally);
//! * [`AMBER`]: needs a look (warnings, LIMITED, a lost subject);
//! * [`ACCENT`]: the operator's own selection (a chosen mode or transition).
//!
//! Every text colour is chosen against the surfaces it sits on, and the
//! tests hold the palette to WCAG contrast ratios ([`contrast`]): body text
//! at least 7:1 on every surface, weak text 4.5:1, and the text on a tally
//! colour 4.5:1 too, so the window stays readable on a dim monitor across
//! the room. [`apply`] installs the palette and the type scale in egui's
//! style once, as the window opens; the drawing code takes its colours from
//! here rather than spelling out its own.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme, Vec2,
    style::Selection,
};

/// The central area behind the monitors and the multiview.
pub const BACKGROUND: Color32 = Color32::from_rgb(17, 19, 23);
/// Side panel, header and status bar.
pub const PANEL: Color32 = Color32::from_rgb(24, 27, 32);
/// A card (one section of the side panel) and an idle widget.
pub const SURFACE: Color32 = Color32::from_rgb(33, 37, 44);
/// A hovered widget.
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(45, 50, 59);
/// A pressed widget.
pub const SURFACE_ACTIVE: Color32 = Color32::from_rgb(56, 62, 73);
/// Text fields and other wells, darker than the panel they sit in.
pub const WELL: Color32 = Color32::from_rgb(12, 14, 17);
/// Hairlines between areas and around cards.
pub const BORDER: Color32 = Color32::from_rgb(48, 53, 62);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(230, 232, 236);
/// Headings and emphasis.
pub const TEXT_STRONG: Color32 = Color32::from_rgb(250, 251, 252);
/// Secondary text: explanations, units, idle states.
pub const TEXT_WEAK: Color32 = Color32::from_rgb(156, 163, 175);
/// Program: on air.
pub const PROGRAM_RED: Color32 = Color32::from_rgb(220, 38, 38);
/// Red text (alarms, failures): lighter than [`PROGRAM_RED`], which is
/// too dark to read as text on the panels.
pub const RED_TEXT: Color32 = Color32::from_rgb(248, 113, 113);
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

/// Body text size, in points: larger than egui's 12.5, since the operator
/// sits back from the screen and the default font is a light weight.
pub const BODY: f32 = 14.0;
/// Small print: units, hints, tile status lines.
pub const SMALL: f32 = 11.5;
/// Section headings in the side panel.
pub const HEADING: f32 = 15.0;
/// Monospaced numbers (encoder statistics, meters).
pub const MONO: f32 = 12.5;
/// The big switcher buttons (CUT, AUTO).
pub const BIG_BUTTON: f32 = 17.0;

/// Corner radius of buttons and fields.
pub const RADIUS: u8 = 4;
/// Corner radius of cards and dialogs.
pub const CARD_RADIUS: u8 = 6;

/// Installs the palette, the type scale and the spacing in `ctx`, for the
/// dark theme, and pins the window to that theme.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, style);
}

/// Rewrites `style` to the operator window's look (see the module docs).
pub fn style(style: &mut egui::Style) {
    use FontFamily::{Monospace, Proportional};
    style.text_styles = [
        (TextStyle::Small, FontId::new(SMALL, Proportional)),
        (TextStyle::Body, FontId::new(BODY, Proportional)),
        (TextStyle::Button, FontId::new(BODY, Proportional)),
        (TextStyle::Heading, FontId::new(HEADING, Proportional)),
        (TextStyle::Monospace, FontId::new(MONO, Monospace)),
    ]
    .into();
    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(8.0, 6.0);
    s.button_padding = Vec2::new(10.0, 4.0);
    s.interact_size = Vec2::new(40.0, 24.0);
    s.window_margin = Margin::same(14);
    s.menu_margin = Margin::same(6);
    s.indent = 16.0;
    s.combo_height = 240.0;
    s.text_edit_width = 220.0;
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
    v.error_fg_color = PROGRAM_RED;
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
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(90, 98, 112));
    w.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(120, 130, 146));
    w.open.bg_stroke = Stroke::new(1.0, BORDER);
}

/// The text colour that reads on a `fill` of the palette: dark on the light
/// tally colours (preview green, amber), white on the rest.
pub fn text_on(fill: Color32) -> Color32 {
    if luminance(fill) > 0.25 {
        ON_LIGHT
    } else {
        ON_DARK
    }
}

/// A side-panel card: a framed section with a heading, which the panels
/// draw their contents into.
pub fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(title.to_uppercase())
                    .size(SMALL)
                    .strong()
                    .color(TEXT_WEAK),
            );
            ui.add_space(2.0);
            // Widgets inside a card sit on SURFACE: give idle ones the
            // panel colour so they stand out from it.
            let w = &mut ui.visuals_mut().widgets;
            w.inactive.bg_fill = PANEL;
            w.inactive.weak_bg_fill = PANEL;
            add(ui)
        })
        .inner
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
    const SURFACES: [Color32; 5] = [BACKGROUND, PANEL, SURFACE, SURFACE_HOVER, WELL];

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
    fn apply_pins_the_dark_theme_and_the_type_scale() {
        let ctx = egui::Context::default();
        ctx.set_theme(Theme::Light);
        apply(&ctx);
        assert_eq!(ctx.theme(), Theme::Dark);
        let style = ctx.style_of(Theme::Dark);
        assert_eq!(style.text_styles[&TextStyle::Body].size, BODY);
        assert_eq!(style.text_styles[&TextStyle::Heading].size, HEADING);
        assert_eq!(style.visuals.panel_fill, PANEL);
        assert_eq!(style.visuals.selection.bg_fill, ACCENT);
        assert!(style.visuals.dark_mode);
    }
}
