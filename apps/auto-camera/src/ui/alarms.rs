//! The status bar's alarms: the persistent list of what is wrong on air
//! (spec M14), each with an acknowledge control.
//!
//! The list is the console's [`Alarms`] model, which reads every frame's
//! snapshot ([`ac_engine::alarms`] has the raise and clear rules of each
//! alarm). Unlike the notice beside it ([`super::notices`]), which says
//! once what just happened and goes after a few seconds, an alarm stays
//! listed for as long as its cause lasts: a black or frozen program, the
//! program on a lost subject, encoder drops, a failed output, a stream
//! reconnecting, stalled or down, a recording without audio. Those
//! conditions raise no notice; everything else keeps the notice's path.
//!
//! Each alarm is a row above the status line, oldest first: the time it
//! was raised (master-clock media time, as `m:ss`) and what is wrong, in
//! red with an Acknowledge button while nobody has acknowledged it, dimmed
//! once someone has. Acknowledging marks it as seen without hiding it: it
//! stays listed until its cause clears, and a cause that comes back raises
//! a new alarm. The alarms are the window's own: acknowledging tells the
//! engine nothing.
//!
//! Every Acknowledge button carries the alarm's message in its accessible
//! name ([`acknowledge_label`]), so a screen reader, and the window tests,
//! can tell them apart.

use ac_engine::{Alarm, AlarmCause, Alarms};
use eframe::egui::{self, Color32, RichText, Ui};

use super::text::clock;

/// Red, for an alarm nobody has acknowledged yet.
const ALARM_RED: Color32 = Color32::from_rgb(235, 60, 50);

/// Hover text of the Acknowledge button.
const ACKNOWLEDGE_HOVER: &str = "Mark as seen: it stays listed, dimmed, until its cause clears";

/// An alarm's row: "ALARM 0:12 · program has been black for over 2 s",
/// with "(acknowledged)" once it is.
pub fn alarm_line(alarm: &Alarm) -> String {
    let mut line = format!("ALARM {} · {}", clock(alarm.raised_at), alarm.message);
    if alarm.acknowledged {
        line.push_str(" (acknowledged)");
    }
    line
}

/// The accessible name of an alarm's Acknowledge button.
pub fn acknowledge_label(alarm: &Alarm) -> String {
    format!("Acknowledge: {}", alarm.message)
}

/// Draws the active alarms of `alarms`, one row each, and returns the
/// causes whose Acknowledge button was clicked on this frame.
pub fn show(ui: &mut Ui, alarms: &Alarms) -> Vec<AlarmCause> {
    let mut acknowledged = Vec::new();
    for alarm in alarms.active() {
        ui.horizontal_wrapped(|ui| {
            let line = alarm_line(alarm);
            if alarm.acknowledged {
                ui.label(RichText::new(line).weak());
                return;
            }
            ui.label(RichText::new(line).strong().color(ALARM_RED));
            let button = ui
                .small_button("Acknowledge")
                .on_hover_text(ACKNOWLEDGE_HOVER);
            button.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, acknowledge_label(alarm))
            });
            if button.clicked() {
                acknowledged.push(alarm.cause.clone());
            }
        });
    }
    acknowledged
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// An alarm about the output, raised at `secs`.
    fn failed(secs: u64, acknowledged: bool) -> Alarm {
        Alarm {
            cause: AlarmCause::OutputFailed,
            raised_at: Duration::from_secs(secs),
            message: "output failed: ffmpeg exited".into(),
            acknowledged,
        }
    }

    #[test]
    fn a_row_says_when_and_what_and_whether_it_was_acknowledged() {
        assert_eq!(
            alarm_line(&failed(72, false)),
            "ALARM 1:12 · output failed: ffmpeg exited"
        );
        assert_eq!(
            alarm_line(&failed(3725, true)),
            "ALARM 1:02:05 · output failed: ffmpeg exited (acknowledged)"
        );
        assert_eq!(
            acknowledge_label(&failed(0, false)),
            "Acknowledge: output failed: ffmpeg exited"
        );
    }

    #[test]
    fn no_alarms_draw_nothing_and_acknowledge_nothing() {
        let ctx = egui::Context::default();
        let alarms = Alarms::new();
        let mut clicked = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            clicked = show(ui, &alarms);
        });
        assert!(clicked.is_empty());
    }
}
