//! The side panels (director, queries and subjects, output) and the status
//! bar.
//!
//! Each panel draws from the snapshot and returns the [`EngineCommand`]s
//! the operator asked for; none of them talks to the engine. The only state
//! they own is what the operator is typing ([`QueryForm`], [`OutputForm`]),
//! and the forms validate themselves into commands without a window, so
//! the rules (a query needs a name, a prompt and at least one framing; an
//! output needs a destination) are tested directly.

use std::path::PathBuf;

use ac_core::{Preset, ShotSize, TrackState};
use ac_director::{ContentProfile, DirectorMode};
use ac_engine::{EngineCommand, EngineSnapshot, OutputState, preset_for};
use eframe::egui::{self, Color32, RichText, Ui};

use super::text::{
    destinations, dropped_line, encoder_lines, output_state, percent, shot_name, source_line,
    timing_line,
};

/// Framings offered for new queries, with their labels.
pub const SIZES: [(ShotSize, &str); 4] = [
    (ShotSize::Full, "Full"),
    (ShotSize::Medium, "Medium"),
    (ShotSize::MediumClose, "Medium close"),
    (ShotSize::Close, "Close"),
];

/// The add-query form.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryForm {
    /// Operator-facing name.
    pub name: String,
    /// Detector prompt.
    pub prompt: String,
    /// Which of [`SIZES`] to create shots for.
    pub sizes: [bool; 4],
}

impl Default for QueryForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            prompt: String::new(),
            sizes: [false, true, false, true],
        }
    }
}

impl QueryForm {
    /// The selected framings.
    pub fn presets(&self) -> Vec<Preset> {
        SIZES
            .iter()
            .zip(self.sizes)
            .filter(|(_, on)| *on)
            .map(|((size, _), _)| preset_for(*size))
            .collect()
    }

    /// The `AddQuery` command, or why the form is incomplete.
    pub fn command(&self) -> Result<EngineCommand, &'static str> {
        let (name, prompt) = (self.name.trim(), self.prompt.trim());
        if name.is_empty() {
            return Err("give the query a name");
        }
        if prompt.is_empty() {
            return Err("describe what to look for");
        }
        let presets = self.presets();
        if presets.is_empty() {
            return Err("pick at least one framing");
        }
        Ok(EngineCommand::AddQuery {
            name: name.to_owned(),
            prompt: prompt.to_owned(),
            presets,
        })
    }
}

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

/// The director panel: mode, profile, suggestion and Accept.
pub fn director(ui: &mut Ui, snap: &EngineSnapshot) -> Vec<EngineCommand> {
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
        {
            commands.push(EngineCommand::AcceptSuggestion);
        }
        ui.label(format!("{} takes", d.takes));
    });
    commands
}

/// The queries panel: live queries, the add form and the subjects.
pub fn queries(ui: &mut Ui, snap: &EngineSnapshot, form: &mut QueryForm) -> Vec<EngineCommand> {
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
            let sizes: Vec<&str> = q.presets.iter().map(|p| p.name.as_str()).collect();
            ui.label(RichText::new(&q.name).strong());
            ui.label(RichText::new(format!("“{}” · {}", q.prompt, sizes.join(", "))).weak());
        });
    }
    ui.add_space(4.0);
    egui::Grid::new("query-form").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut form.name);
        ui.end_row();
        ui.label("Prompt");
        ui.text_edit_singleline(&mut form.prompt);
        ui.end_row();
    });
    ui.horizontal_wrapped(|ui| {
        for ((_, label), on) in SIZES.iter().zip(form.sizes.iter_mut()) {
            ui.checkbox(on, *label);
        }
    });
    let ready = form.command();
    let add = ui.add_enabled(ready.is_ok(), egui::Button::new("Add query"));
    match ready {
        Ok(cmd) if add.clicked() => {
            commands.push(cmd);
            *form = QueryForm {
                sizes: form.sizes,
                ..QueryForm::default()
            };
        }
        Ok(_) => {}
        Err(why) => {
            add.on_disabled_hover_text(why);
        }
    }
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

/// The output panel: destinations, start/stop and encoder statistics.
pub fn output(ui: &mut Ui, snap: &EngineSnapshot, form: &mut OutputForm) -> Vec<EngineCommand> {
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
            commands.push(EngineCommand::StopOutput);
        }
        let color = match out.state {
            OutputState::Running => Color32::from_rgb(230, 40, 40),
            OutputState::Failed(_) => Color32::from_rgb(230, 140, 40),
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
    if let Some(a) = &out.audio {
        ui.label(format!(
            "audio pacing: {} pushed · {} silence · {} dropped",
            a.pushed, a.silence, a.dropped
        ));
    }
    commands
}

/// The status bar: sources, tick timing, drops and the latest notice.
pub fn status_bar(ui: &mut Ui, snap: &EngineSnapshot, notice: Option<(&str, bool)>) {
    ui.horizontal_wrapped(|ui| {
        for s in &snap.sources {
            ui.label(source_line(s, snap.media_time));
            ui.separator();
        }
        ui.label(timing_line(&snap.timing));
        ui.separator();
        ui.label(dropped_line(snap));
        if !snap.render_issues.is_empty() {
            ui.separator();
            ui.label(
                RichText::new(format!("render: {}", snap.render_issues.join("; ")))
                    .color(Color32::from_rgb(230, 140, 40)),
            );
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
    fn a_complete_query_form_adds_a_query() {
        let form = QueryForm {
            name: " Lead singer ".into(),
            prompt: "person singing".into(),
            sizes: [true, false, false, true],
        };
        let Ok(EngineCommand::AddQuery {
            name,
            prompt,
            presets,
        }) = form.command()
        else {
            panic!("a command");
        };
        assert_eq!(name, "Lead singer");
        assert_eq!(prompt, "person singing");
        let sizes: Vec<ShotSize> = presets.iter().map(|p| p.size).collect();
        assert_eq!(sizes, [ShotSize::Full, ShotSize::Close]);
        assert_eq!(QueryForm::default().presets().len(), 2);
    }

    #[test]
    fn incomplete_query_forms_say_what_is_missing() {
        let mut form = QueryForm::default();
        assert_eq!(form.command(), Err("give the query a name"));
        form.name = "Pastor".into();
        form.prompt = "   ".into();
        assert_eq!(form.command(), Err("describe what to look for"));
        form.prompt = "person at the lectern".into();
        form.sizes = [false; 4];
        assert_eq!(form.command(), Err("pick at least one framing"));
    }

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
        let snap = snapshot();
        let mut q = QueryForm::default();
        let mut o = OutputForm::default();
        let mut commands = Vec::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            commands.clear();
            commands.extend(director(ui, &snap));
            commands.extend(queries(ui, &snap, &mut q));
            commands.extend(output(ui, &snap, &mut o));
            status_bar(ui, &snap, Some(("cut: no preview", true)));
        });
        assert!(commands.is_empty(), "{commands:?}");
    }
}
