//! The side panels (director, queries and subjects, output) and the status
//! bar.
//!
//! Each panel draws from the snapshot and returns the [`EngineCommand`]s
//! the operator asked for; none of them talks to the engine. The only state
//! they own is what the operator is typing ([`QueryForm`], in
//! [`query_form`], and [`OutputForm`]), and the forms validate themselves
//! into commands without a window, so the rules (a query needs a name, a
//! prompt with something to frame and at least one framing; an output needs
//! a destination) are tested directly. The output panel's Stop button goes
//! through the [`CloseGuard`], which asks before a live output is stopped.

use std::path::PathBuf;

use ac_core::{ShotId, TrackState};
use ac_director::{ContentProfile, DirectorMode};
use ac_engine::{EngineCommand, EngineSnapshot, OutputState, PromptReader};
use eframe::egui::{self, Color32, RichText, Ui};

use super::close_guard::{self, CloseGuard};
use super::keys;
use super::query_form::{self, QueryForm};
use super::text::{
    Tone, destinations, dropped_line, encoder_lines, match_lines, output_state, percent,
    query_hover, query_line, shot_name, source_hover, source_line, source_tone, stream_status,
    timing_line,
};

/// Amber, for warnings such as a prompt with no matches.
const AMBER: Color32 = Color32::from_rgb(230, 140, 40);

/// Indent of a query's feedback line, under its name (past the remove
/// button).
const FEEDBACK_INDENT: f32 = 24.0;

/// The output form.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputForm {
    /// RTMP URL with stream key.
    pub rtmp_url: String,
    /// Recording path.
    pub record_path: String,
}

impl OutputForm {
    /// A form prefilled with the config's destinations.
    pub fn new(rtmp_url: Option<&str>, record_path: Option<&std::path::Path>) -> Self {
        Self {
            rtmp_url: rtmp_url.unwrap_or_default().to_owned(),
            record_path: record_path
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        }
    }

    /// The `StartOutput` command, or why the form is incomplete.
    pub fn command(&self) -> Result<EngineCommand, &'static str> {
        let rtmp = self.rtmp_url.trim();
        let record = self.record_path.trim();
        if rtmp.is_empty() && record.is_empty() {
            return Err("give an RTMP URL or a recording path");
        }
        if !rtmp.is_empty() && !(rtmp.starts_with("rtmp://") || rtmp.starts_with("rtmps://")) {
            return Err("the stream URL must start with rtmp:// or rtmps://");
        }
        Ok(EngineCommand::StartOutput {
            rtmp_url: (!rtmp.is_empty()).then(|| rtmp.to_owned()),
            record_path: (!record.is_empty()).then(|| PathBuf::from(record)),
        })
    }
}

/// The director panel: mode, profile, the suggestion with Accept and Skip,
/// and the operator's hold on preview with Release. Accept and Skip name
/// `seen`, the suggestion the operator saw (the console's, the one the last
/// frame drew), so a suggestion that changed meanwhile is neither the one
/// taken nor the one skipped: the engine refuses the Accept.
pub fn director(ui: &mut Ui, snap: &EngineSnapshot, seen: Option<&ShotId>) -> Vec<EngineCommand> {
    let mut commands = Vec::new();
    let d = &snap.director;
    ui.heading("Director");
    let mut mode = d.mode;
    ui.horizontal(|ui| {
        ui.label("Mode");
        ui.selectable_value(&mut mode, DirectorMode::Off, "Off");
        ui.selectable_value(&mut mode, DirectorMode::Suggest, "Suggest");
        ui.selectable_value(&mut mode, DirectorMode::Auto, "Auto");
    });
    if mode != d.mode {
        commands.push(EngineCommand::SetDirectorMode { mode });
    }
    let mut profile = d.profile;
    ui.horizontal(|ui| {
        ui.label("Profile");
        ui.selectable_value(&mut profile, ContentProfile::Talk, "Talk");
        ui.selectable_value(&mut profile, ContentProfile::Music, "Music");
    });
    if profile != d.profile {
        commands.push(EngineCommand::SetDirectorProfile { profile });
    }
    ui.horizontal(|ui| {
        ui.label("Suggests");
        ui.label(RichText::new(shot_name(snap, d.suggestion.as_ref())).strong());
    });
    ui.label(RichText::new(&d.reason_text).weak());
    ui.horizontal(|ui| {
        let can = d.mode != DirectorMode::Off && d.suggestion.is_some();
        if ui
            .add_enabled(can, egui::Button::new("Accept"))
            .on_hover_text("Stage the suggestion and take it with the selected transition")
            .clicked()
            && let Some(shot) = seen
        {
            commands.push(EngineCommand::AcceptSuggestion { shot: shot.clone() });
        }
        let skip = ui
            .add_enabled(can, egui::Button::new("Skip"))
            .on_hover_text(format!(
                "Do not suggest this shot for {:.0} s",
                d.skip.as_secs_f64()
            ));
        if skip.clicked()
            && let Some(shot) = seen
        {
            commands.push(EngineCommand::Skip { shot: shot.clone() });
        }
        ui.label(format!("{} takes", d.takes));
    });
    if let Some(held) = d
        .preview_held
        .as_ref()
        .filter(|_| d.mode != DirectorMode::Off)
    {
        ui.horizontal(|ui| {
            ui.label(format!("Preview held: {}", shot_name(snap, Some(held))))
                .on_hover_text(HELD_HOVER);
            if ui
                .button("Release")
                .on_hover_text("Hand preview back to the director")
                .clicked()
            {
                commands.push(EngineCommand::ReleasePreview);
            }
        });
    }
    commands
}

/// Hover text of the operator's hold on preview.
const HELD_HOVER: &str = "You staged this shot, so the director does not stage its \
                          suggestions over it until you take it or release it.";

/// The queries panel: live queries, the add form (its prompt read with
/// `prompts`, the engine's reader) and the subjects.
pub fn queries(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    form: &mut QueryForm,
    prompts: &PromptReader,
) -> Vec<EngineCommand> {
    let mut commands = Vec::new();
    ui.heading("Queries");
    if snap.queries.is_empty() {
        ui.label(RichText::new("No queries: add one to find subjects.").weak());
    }
    for q in &snap.queries {
        ui.horizontal(|ui| {
            if ui.small_button("✕").on_hover_text("Remove").clicked() {
                commands.push(EngineCommand::RemoveQuery {
                    query: q.id.clone(),
                });
            }
            ui.label(RichText::new(&q.name).strong());
            ui.label(RichText::new(query_line(q)).weak())
                .on_hover_text(query_hover(q));
        });
        // Prompt feedback (spec M2): what the newest pass found.
        let lines = match_lines(snap, q);
        if !lines.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(FEEDBACK_INDENT);
                for (text, tone, hover) in lines {
                    let text = match tone {
                        Tone::Plain => RichText::new(text),
                        Tone::Warning => RichText::new(text).color(AMBER),
                        Tone::Weak => RichText::new(text).weak(),
                    };
                    let label = ui.label(text);
                    if !hover.is_empty() {
                        label.on_hover_text(hover);
                    }
                }
            });
        }
    }
    ui.add_space(4.0);
    commands.extend(query_form::show(ui, form, prompts));
    ui.separator();
    ui.heading("Subjects");
    if snap.subjects.is_empty() {
        ui.label(RichText::new("none").weak());
    }
    for s in &snap.subjects {
        ui.horizontal(|ui| {
            let state = match s.state {
                TrackState::Tracking => format!("tracking {}", percent(f64::from(s.confidence))),
                TrackState::Lost => "lost".into(),
            };
            let mut label = format!("{} #{} · {}", s.query.0, s.id.0, state);
            if snap.sources.len() > 1 {
                label = format!("{} · {label}", s.source.0);
            }
            ui.label(label);
            if s.protected {
                ui.label(RichText::new("on air").color(Color32::from_rgb(230, 40, 40)));
            }
            let pin = if s.pinned { "Unpin" } else { "Pin" };
            if ui.small_button(pin).clicked() {
                commands.push(EngineCommand::Pin {
                    source: s.source.clone(),
                    subject: s.id,
                    pinned: !s.pinned,
                });
            }
            let hide = if s.hidden { "Show" } else { "Hide" };
            if ui.small_button(hide).clicked() {
                commands.push(EngineCommand::Hide {
                    source: s.source.clone(),
                    subject: s.id,
                    hidden: !s.hidden,
                });
            }
        });
    }
    commands
}

/// The output panel: destinations, start/stop and encoder statistics. Stop
/// asks `guard` first while the output is live.
pub fn output(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    form: &mut OutputForm,
    guard: &mut CloseGuard,
) -> Vec<EngineCommand> {
    let mut commands = Vec::new();
    let out = &snap.output;
    ui.heading("Output");
    let idle = matches!(
        out.state,
        OutputState::Idle | OutputState::Finished | OutputState::Failed(_)
    );
    ui.add_enabled_ui(idle, |ui| {
        egui::Grid::new("output-form")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("RTMP URL");
                ui.add(egui::TextEdit::singleline(&mut form.rtmp_url).password(true));
                ui.end_row();
                ui.label("Record to");
                ui.text_edit_singleline(&mut form.record_path);
                ui.end_row();
            });
    });
    ui.horizontal(|ui| {
        if idle {
            let ready = form.command();
            let start = ui.add_enabled(ready.is_ok(), egui::Button::new("Start"));
            match ready {
                Ok(cmd) if start.clicked() => commands.push(cmd),
                Ok(_) => {}
                Err(why) => {
                    start.on_disabled_hover_text(why);
                }
            }
        } else if ui
            .add_enabled(out.state == OutputState::Running, egui::Button::new("Stop"))
            .clicked()
        {
            commands.extend(guard.stop_requested(close_guard::output_live(snap)));
        }
        let color = match out.state {
            OutputState::Running => Color32::from_rgb(230, 40, 40),
            OutputState::Failed(_) => AMBER,
            _ => ui.visuals().text_color(),
        };
        ui.label(
            RichText::new(output_state(&out.state))
                .strong()
                .color(color),
        );
    });
    if !idle || out.stats.is_some() {
        ui.label(RichText::new(destinations(out)).weak());
        ui.label(format!("{} frames pushed", out.frames_pushed));
    }
    if let Some(stats) = &out.stats {
        for line in encoder_lines(stats) {
            ui.label(RichText::new(line).monospace());
        }
    }
    if let Some((line, tone)) = stream_status(out) {
        let text = RichText::new(line).monospace();
        ui.label(match tone {
            Tone::Plain => text,
            Tone::Warning => text.color(AMBER),
            Tone::Weak => text.weak(),
        });
    }
    if let Some(a) = &out.audio {
        ui.label(format!(
            "audio pacing: {} pushed · {} silence · {} dropped",
            a.pushed, a.silence, a.dropped
        ));
    }
    commands
}

/// The status bar: sources, tick timing, drops, [`keys::PAUSED`] while the
/// shortcuts are `paused`, and the latest notice.
pub fn status_bar(ui: &mut Ui, snap: &EngineSnapshot, paused: bool, notice: Option<(&str, bool)>) {
    ui.horizontal_wrapped(|ui| {
        for s in &snap.sources {
            let line = source_line(s, snap.media_time);
            let text = match source_tone(s) {
                Tone::Warning => RichText::new(line).color(AMBER),
                Tone::Plain | Tone::Weak => RichText::new(line),
            };
            let label = ui.label(text);
            let hover = source_hover(s);
            if !hover.is_empty() {
                label.on_hover_text(hover);
            }
            ui.separator();
        }
        ui.label(timing_line(&snap.timing));
        ui.separator();
        ui.label(dropped_line(snap));
        if !snap.render_issues.is_empty() {
            ui.separator();
            ui.label(
                RichText::new(format!("render: {}", snap.render_issues.join("; "))).color(AMBER),
            );
        }
        if paused {
            ui.separator();
            ui.label(RichText::new(keys::PAUSED).color(AMBER))
                .on_hover_text(keys::PAUSED_HOVER);
        }
        if let Some((text, error)) = notice {
            ui.separator();
            let color = if error {
                Color32::from_rgb(230, 90, 60)
            } else {
                ui.visuals().weak_text_color()
            };
            ui.label(RichText::new(text).color(color));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::snapshot;

    #[test]
    fn the_output_form_needs_a_valid_destination() {
        let mut form = OutputForm::new(None, None);
        assert_eq!(form.command(), Err("give an RTMP URL or a recording path"));
        form.record_path = " /tmp/show.mp4 ".into();
        assert_eq!(
            form.command(),
            Ok(EngineCommand::StartOutput {
                rtmp_url: None,
                record_path: Some("/tmp/show.mp4".into())
            })
        );
        form.rtmp_url = "http://example.com/live".into();
        assert!(form.command().is_err());
        form.rtmp_url = "rtmp://example.com/app/key".into();
        let Ok(EngineCommand::StartOutput { rtmp_url, .. }) = form.command() else {
            panic!("a command");
        };
        assert_eq!(rtmp_url.as_deref(), Some("rtmp://example.com/app/key"));
        let pre = OutputForm::new(Some("rtmp://a/b"), Some(std::path::Path::new("x.mp4")));
        assert_eq!(
            (pre.rtmp_url.as_str(), pre.record_path.as_str()),
            ("rtmp://a/b", "x.mp4")
        );
    }

    #[test]
    fn the_panels_render_headless_without_commands() {
        let ctx = egui::Context::default();
        let mut snap = snapshot();
        // A query with prompt feedback from two passes, one empty.
        let mut second = snap.queries[0].clone();
        second.id = ac_core::QueryId("other".into());
        for count in [2, 0] {
            second.matches.push(ac_engine::MatchView {
                source: snap.sources[0].id.clone(),
                seq: 7,
                pts: std::time::Duration::from_secs(1),
                count,
                best: (count > 0).then_some(0.8),
                kept: count,
                floor: 0.35,
            });
        }
        snap.queries.push(second);
        snap.sources[0].discovery_enabled = true;
        let mut q = QueryForm::default();
        let mut o = OutputForm::default();
        let mut guard = CloseGuard::default();
        let mut commands = Vec::new();
        let prompts = PromptReader::new(ac_core::Lexicon::builtin(), false);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            commands.clear();
            commands.extend(director(ui, &snap, snap.director.suggestion.as_ref()));
            commands.extend(queries(ui, &snap, &mut q, &prompts));
            commands.extend(output(ui, &snap, &mut o, &mut guard));
            status_bar(ui, &snap, true, Some(("cut: no preview", true)));
        });
        assert!(commands.is_empty(), "{commands:?}");
    }
}
