//! The program loudness meter (spec M12): a compact widget in the output
//! panel with momentary and short-term loudness as bars, and integrated
//! loudness and true peak as numbers.
//!
//! The readings come from the engine's meter on the paced program audio
//! ([`ac_audio::LoudnessMeter`], EBU R 128), so the widget shows what the
//! encoder is given. The bars run from [`FLOOR_LUFS`] to 0 LUFS with a tick
//! every 10 LU. A level below the floor leaves its bar empty and reads
//! "<-60" (the fading tail of a sound that stopped reads hundreds of dB
//! down for a moment); a missing reading (a window not filled yet, digital
//! silence, nothing above the gates) shows as [`NO_READING`] rather than as
//! a number. The true peak is the session's largest, drawn amber above
//! [`TRUE_PEAK_LIMIT`], the maximum true-peak level of EBU R 128, so the
//! operator sees the program near clipping. Everything but the painting is
//! a plain function of the reading, tested without a window, and the widget
//! itself renders in tests through a headless `egui::Context`.

use ac_audio::LoudnessReading;
use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, Ui, vec2};

/// Bottom of the bars' scale, in LUFS.
pub const FLOOR_LUFS: f64 = -60.0;

/// True peaks above this, in dBTP, are drawn amber.
pub const TRUE_PEAK_LIMIT: f64 = -1.0;

/// What a missing reading shows.
pub const NO_READING: &str = "—";

/// Size of a bar, in points.
const BAR: egui::Vec2 = vec2(150.0, 9.0);

/// Colour of a bar's level.
const LEVEL_GREEN: Color32 = Color32::from_rgb(40, 200, 80);

/// Colour of a true peak above the limit.
const AMBER: Color32 = Color32::from_rgb(230, 140, 40);

/// What the widget's hover text explains.
const HOVER: &str = "Program loudness (EBU R 128) of the paced program audio. \
M: momentary, the last 400 ms. S: short-term, the last 3 s. \
I: integrated over the session, gated (silence and quiet passages left out). \
TP: the session's highest true peak, 4x oversampled.";

/// How far along the bar `lufs` reaches, from 0 (at or below the floor, or
/// no reading) to 1 (0 LUFS or more).
pub fn fraction(lufs: Option<f64>) -> f32 {
    lufs.map_or(0.0, |l| {
        ((l - FLOOR_LUFS) / -FLOOR_LUFS).clamp(0.0, 1.0) as f32
    })
}

/// A level to one decimal, or [`NO_READING`].
pub fn value(level: Option<f64>) -> String {
    level.map_or_else(|| NO_READING.to_owned(), |l| format!("{l:.1}"))
}

/// A bar's loudness: [`value`], or "<-60" below the bars' floor.
pub fn bar_value(lufs: Option<f64>) -> String {
    match lufs {
        Some(l) if l < FLOOR_LUFS => format!("<{FLOOR_LUFS:.0}"),
        other => value(other),
    }
}

/// True when the true peak is above [`TRUE_PEAK_LIMIT`].
pub fn peak_warning(true_peak: Option<f64>) -> bool {
    true_peak.is_some_and(|tp| tp > TRUE_PEAK_LIMIT)
}

/// The line under the bars: integrated loudness and true peak.
pub fn totals_line(reading: &LoudnessReading) -> (String, String) {
    (
        format!("I {} LUFS", value(reading.integrated)),
        format!("TP {} dBTP", value(reading.true_peak)),
    )
}

/// Draws one bar for `lufs`.
fn bar(ui: &mut Ui, lufs: Option<f64>) {
    let (rect, _) = ui.allocate_exact_size(BAR, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(2), Color32::from_gray(45));
    let level = Rect::from_min_size(rect.min, vec2(rect.width() * fraction(lufs), rect.height()));
    painter.rect_filled(level, CornerRadius::same(2), LEVEL_GREEN);
    for tick in [-50.0, -40.0, -30.0, -20.0, -10.0] {
        let x = rect.left() + rect.width() * fraction(Some(tick));
        painter.vline(x, rect.y_range(), Stroke::new(1.0, Color32::from_gray(25)));
    }
}

/// Draws the meter for `reading`.
pub fn show(ui: &mut Ui, reading: &LoudnessReading) {
    let response = ui
        .vertical(|ui| {
            for (name, lufs) in [("M", reading.momentary), ("S", reading.short_term)] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).monospace());
                    bar(ui, lufs);
                    ui.label(RichText::new(bar_value(lufs)).monospace());
                });
            }
            let (integrated, peak) = totals_line(reading);
            ui.horizontal(|ui| {
                ui.label(RichText::new(integrated).monospace());
                ui.separator();
                let peak = RichText::new(peak).monospace();
                ui.label(if peak_warning(reading.true_peak) {
                    peak.color(AMBER)
                } else {
                    peak
                });
            });
        })
        .response;
    response.on_hover_text(HOVER);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Every text a frame painted.
    fn painted_texts(out: &egui::FullOutput) -> Vec<String> {
        out.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    /// A reading of a program at about −23 LUFS.
    fn program() -> LoudnessReading {
        LoudnessReading {
            momentary: Some(-18.24),
            short_term: Some(-21.07),
            integrated: Some(-23.0),
            sample_peak: Some(-4.0),
            true_peak: Some(-3.46),
            measured: Duration::from_secs(90),
        }
    }

    #[test]
    fn bars_cover_the_scale_and_clamp() {
        assert_eq!(fraction(None), 0.0);
        assert_eq!(fraction(Some(-90.0)), 0.0);
        assert_eq!(fraction(Some(FLOOR_LUFS)), 0.0);
        assert_eq!(fraction(Some(-30.0)), 0.5);
        assert_eq!(fraction(Some(0.0)), 1.0);
        assert_eq!(fraction(Some(12.0)), 1.0);
    }

    #[test]
    fn values_are_numbers_or_an_explicit_dash() {
        assert_eq!(value(Some(-18.24)), "-18.2");
        assert_eq!(value(Some(0.04)), "0.0");
        assert_eq!(value(None), NO_READING);
        assert_eq!(bar_value(Some(-312.48)), "<-60");
        assert_eq!(bar_value(Some(FLOOR_LUFS)), "-60.0");
        assert_eq!(bar_value(Some(-18.24)), "-18.2");
        assert_eq!(bar_value(None), NO_READING);
        let (i, tp) = totals_line(&LoudnessReading::default());
        assert_eq!((i.as_str(), tp.as_str()), ("I — LUFS", "TP — dBTP"));
        let (i, tp) = totals_line(&program());
        assert_eq!((i.as_str(), tp.as_str()), ("I -23.0 LUFS", "TP -3.5 dBTP"));
    }

    #[test]
    fn a_true_peak_above_the_limit_is_a_warning() {
        assert!(!peak_warning(None));
        assert!(!peak_warning(Some(-3.0)));
        assert!(!peak_warning(Some(TRUE_PEAK_LIMIT)));
        assert!(peak_warning(Some(-0.9)));
        assert!(peak_warning(Some(2.0)));
    }

    #[test]
    fn the_meter_renders_headless() {
        // A full egui frame without a window, for a program, for silence
        // and for a session that has not measured anything yet.
        let ctx = egui::Context::default();
        let silence = LoudnessReading {
            measured: Duration::from_secs(5),
            ..LoudnessReading::default()
        };
        let hot = LoudnessReading {
            true_peak: Some(0.4),
            ..program()
        };
        let fading = LoudnessReading {
            momentary: Some(-287.1),
            ..program()
        };
        for (reading, wanted) in [
            (
                program(),
                ["-18.2", "-21.1", "I -23.0 LUFS", "TP -3.5 dBTP"],
            ),
            (hot, ["-18.2", "-21.1", "I -23.0 LUFS", "TP 0.4 dBTP"]),
            (fading, ["<-60", "-21.1", "I -23.0 LUFS", "TP -3.5 dBTP"]),
            (silence, [NO_READING, NO_READING, "I — LUFS", "TP — dBTP"]),
            (
                LoudnessReading::default(),
                [NO_READING, NO_READING, "I — LUFS", "TP — dBTP"],
            ),
        ] {
            let out = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, &reading));
            let texts = painted_texts(&out);
            for want in wanted {
                assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
            }
            for t in &texts {
                assert!(!t.contains("NaN") && !t.contains("inf"), "{t}");
            }
            // The two bars' backgrounds, at least, were painted.
            let rects = out
                .shapes
                .iter()
                .filter(|c| matches!(c.shape, egui::Shape::Rect(_)))
                .count();
            assert!(rects >= 2, "{rects} rectangles");
        }
    }
}
