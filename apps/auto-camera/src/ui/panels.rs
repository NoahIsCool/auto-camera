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
    Tone, destinations, drift_line, drift_notice, dropped_line, encoder_lines, match_lines,
    output_state, percent, query_hover, query_line, shot_name, source_hover, source_line,
    source_tone, stream_status, timing_line,
};
use super::theme;

/// Amber, for warnings such as a prompt with no matches.
const AMBER: Color32 = theme::AMBER;

/// Room a form's field labels take, which its fields leave them.
pub const FIELD_LABEL: f32 = 72.0;

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
    let mut mode = d.mode;
    let mut profile = d.profile;
    egui::Grid::new("director-modes")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Mode").weak());
            segmented(
                ui,
                &mut mode,
                &[
                    (DirectorMode::Off, "Off"),
                    (DirectorMode::Suggest, "Suggest"),
                    (DirectorMode::Auto, "Auto"),
                ],
            );
            ui.end_row();
            ui.label(RichText::new("Profile").weak());
            segmented(
                ui,
                &mut profile,
                &[
                    (ContentProfile::Talk, "Talk"),
                    (ContentProfile::Music, "Music"),
                ],
            );
            ui.end_row();
        });
    if mode != d.mode {
        commands.push(EngineCommand::SetDirectorMode { mode });
    }
    if profile != d.profile {
        commands.push(EngineCommand::SetDirectorProfile { profile });
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Suggests").weak());
        ui.label(
            RichText::new(shot_name(snap, d.suggestion.as_ref()))
                .strong()
                .color(theme::TEXT_STRONG),
        );
    });
    if !d.reason_text.is_empty() {
        ui.label(RichText::new(&d.reason_text).weak());
    }
    ui.horizontal(|ui| {
        let can = d.mode != DirectorMode::Off && d.suggestion.is_some();
        let accept = egui::Button::new("Accept").min_size(egui::vec2(84.0, 26.0));
        if ui
            .add_enabled(can, accept)
            .on_hover_text("Stage the suggestion and take it with the selected transition")
            .clicked()
            && let Some(shot) = seen
        {
            commands.push(EngineCommand::AcceptSuggestion { shot: shot.clone() });
        }
        let skip = ui
            .add_enabled(
                can,
                egui::Button::new("Skip").min_size(egui::vec2(64.0, 26.0)),
            )
            .on_hover_text(format!(
                "Do not suggest this shot for {:.0} s",
                d.skip.as_secs_f64()
            ));
        if skip.clicked()
            && let Some(shot) = seen
        {
            commands.push(EngineCommand::Skip { shot: shot.clone() });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(takes_text(d.takes)).weak());
        });
    });
    if let Some(held) = d
        .preview_held
        .as_ref()
        .filter(|_| d.mode != DirectorMode::Off)
    {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Preview held: {}", shot_name(snap, Some(held))))
                    .color(theme::AMBER),
            )
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

/// How many takes the director has made, in words.
pub fn takes_text(takes: u64) -> String {
    match takes {
        1 => "1 take".into(),
        n => format!("{n} takes"),
    }
}

/// A row of toggle buttons choosing one of `options` into `value`, like a
/// segmented control; the selected one is filled with the accent colour.
fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (option, label) in options {
            let selected = *value == *option;
            let text = RichText::new(*label).color(if selected {
                theme::text_on(theme::ACCENT)
            } else {
                theme::TEXT
            });
            if ui
                .add(
                    egui::Button::new(text)
                        .selected(selected)
                        .min_size(egui::vec2(58.0, 24.0)),
                )
                .clicked()
            {
                *value = *option;
            }
        }
    });
}

/// Hover text of the operator's hold on preview.
const HELD_HOVER: &str = "You staged this shot, so the director does not stage its \
                          suggestions over it until you take it or release it.";

/// What the queries panel says while there are none.
pub const NO_QUERIES: &str = "No queries yet. Describe who to frame below, for example \
                              \"speaker upper body\".";

/// The queries panel: live queries with what the newest detector pass
/// found for each, and the add form (its prompt read with `prompts`, the
/// engine's reader).
pub fn queries(
    ui: &mut Ui,
    snap: &EngineSnapshot,
    form: &mut QueryForm,
    prompts: &PromptReader,
) -> Vec<EngineCommand> {
    let mut commands = Vec::new();
    if snap.queries.is_empty() {
        ui.label(RichText::new(NO_QUERIES).weak());
    }
    for q in &snap.queries {
        ui.horizontal(|ui| {
            if ui
                .small_button(REMOVE)
                .on_hover_text(format!("Remove {}", q.name))
                .clicked()
            {
                commands.push(EngineCommand::RemoveQuery {
                    query: q.id.clone(),
                });
            }
            ui.label(RichText::new(&q.name).strong().color(theme::TEXT_STRONG));
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
                    let label = ui.label(text.size(theme::SMALL));
                    if !hover.is_empty() {
                        label.on_hover_text(hover);
                    }
                }
            });
        }
    }
    ui.add_space(4.0);
    ui.separator();
    ui.label(RichText::new("Add a query").weak());
    commands.extend(query_form::show(ui, form, prompts));
    commands
}

/// The remove button's text: a multiplication sign, which the default font
/// has (it has no "✕").
const REMOVE: &str = "×";

/// The subjects panel: every subject with its tracking state, and Pin and
/// Hide.
pub fn subjects(ui: &mut Ui, snap: &EngineSnapshot) -> Vec<EngineCommand> {
    let mut commands = Vec::new();
    if snap.subjects.is_empty() {
        ui.label(RichText::new("No subjects found yet.").weak());
        return commands;
    }
    egui::Grid::new("subjects")
        .num_columns(4)
        .spacing([10.0, 4.0])
        .striped(false)
        .show(ui, |ui| {
            for s in &snap.subjects {
                let mut name = format!("{} #{}", s.query.0, s.id.0);
                if snap.sources.len() > 1 {
                    name = format!("{} · {name}", s.source.0);
                }
                ui.label(RichText::new(name).color(theme::TEXT_STRONG));
                let state = match s.state {
                    TrackState::Tracking => {
                        RichText::new(format!("tracking {}", percent(f64::from(s.confidence))))
                            .weak()
                    }
                    TrackState::Lost => RichText::new("lost").color(AMBER),
                };
                ui.horizontal(|ui| {
                    ui.label(state);
                    if s.protected {
                        ui.label(
                            RichText::new("on air")
                                .size(theme::SMALL)
                                .strong()
                                .color(theme::RED_TEXT),
                        );
                    }
                });
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
                ui.end_row();
            }
        });
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
    let idle = matches!(
        out.state,
        OutputState::Idle | OutputState::Finished | OutputState::Failed(_)
    );
    // The fields fill the card past their labels.
    let width = (ui.available_width() - FIELD_LABEL).max(120.0);
    ui.add_enabled_ui(idle, |ui| {
        egui::Grid::new("output-form")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Stream").weak());
                ui.add(
                    egui::TextEdit::singleline(&mut form.rtmp_url)
                        .password(true)
                        .hint_text("rtmp://ingest/app/stream-key")
                        .desired_width(width),
                );
                ui.end_row();
                ui.label(RichText::new("Record").weak());
                ui.add(
                    egui::TextEdit::singleline(&mut form.record_path)
                        .hint_text("~/Movies/show.mp4")
                        .desired_width(width),
                );
                ui.end_row();
            });
    });
    ui.horizontal(|ui| {
        if idle {
            let ready = form.command();
            let start = ui.add_enabled(
                ready.is_ok(),
                egui::Button::new(
                    RichText::new("Start")
                        .strong()
                        .color(theme::text_on(theme::PROGRAM_RED)),
                )
                .fill(theme::PROGRAM_RED)
                .min_size(egui::vec2(84.0, 28.0)),
            );
            match ready {
                Ok(cmd) if start.clicked() => commands.push(cmd),
                Ok(_) => {}
                Err(why) => {
                    start.on_disabled_hover_text(why);
                }
            }
        } else if ui
            .add_enabled(
                out.state == OutputState::Running,
                egui::Button::new(RichText::new("Stop").strong()).min_size(egui::vec2(84.0, 28.0)),
            )
            .clicked()
        {
            commands.extend(guard.stop_requested(close_guard::output_live(snap)));
        }
        let color = match out.state {
            OutputState::Running => theme::RED_TEXT,
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
    if let Some((line, tone)) = drift_line(snap) {
        let text = RichText::new(line);
        ui.label(match tone {
            Tone::Plain => text,
            Tone::Warning => text.color(AMBER),
            Tone::Weak => text.weak(),
        });
    }
    if let Some(notice) = drift_notice(snap) {
        ui.label(RichText::new(notice).weak());
    }
    if let Some(reading) = &out.loudness {
        super::meter::show(ui, reading);
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
        ui.label(RichText::new(timing_line(&snap.timing)).weak());
        ui.separator();
        ui.label(RichText::new(dropped_line(snap)).weak());
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
                theme::RED_TEXT
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
