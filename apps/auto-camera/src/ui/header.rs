//! The header bar: what is going out, at a glance from across the room.
//!
//! A hardware switcher's tally lamps answer three questions without
//! reading: is the engine alive, is the program on air, is it recording.
//! The header answers them the same way, with fixed pills that keep their
//! place and change colour ([`pills`]): ENGINE (only while it has stopped),
//! the director's mode, ON AIR for the stream and REC for the recording.
//! A pill that is off is drawn dim rather than left out, so the lamps never
//! move and an operator learns where to look. Elapsed output time is the
//! frames pushed at the output's frame rate, so it counts what went out,
//! not wall time. The clock on the right is the studio's master media
//! time, the clock the alarms are stamped with.
//!
//! Once the engine has stopped, a red banner across the central area
//! says that nothing is going out ([`stopped_banner`]), so a dead engine
//! is never mistaken for a quiet one: the last pictures would otherwise
//! stay on the monitors as if live.
//!
//! The pills come from the snapshot by [`pills`], which is tested without
//! a window; [`show`] only paints them. Each pill is a label in the
//! accessibility tree, so screen readers and the window tests read it.

use std::time::Duration;

use ac_director::DirectorMode;
use ac_engine::{EngineSnapshot, OutputState, StudioConfig};
use ac_output::StreamState;
use eframe::egui::{self, CornerRadius, Margin, RichText, Stroke, Ui};

use super::text::clock;
use super::theme;

/// How a pill looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Off: dim outline.
    Idle,
    /// On air or recording: solid red.
    Live,
    /// Needs a look: solid amber.
    Warning,
    /// The operator's choice: solid accent.
    Selected,
}

/// One tally pill: its text and look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pill {
    /// What it says.
    pub text: String,
    /// How it looks.
    pub look: Look,
}

impl Pill {
    /// A pill saying `text`, looking `look`.
    fn new(text: impl Into<String>, look: Look) -> Self {
        Self {
            text: text.into(),
            look,
        }
    }
}

/// The header's pills for `snap`, in order, for an output at `fps` frames
/// per second (see the module docs).
pub fn pills(snap: &EngineSnapshot, fps: u32) -> Vec<Pill> {
    let mut pills = Vec::new();
    if !snap.running {
        pills.push(Pill::new("ENGINE STOPPED", Look::Live));
    }
    pills.push(match snap.director.mode {
        DirectorMode::Off => Pill::new("DIRECTOR OFF", Look::Idle),
        DirectorMode::Suggest => Pill::new("DIRECTOR SUGGEST", Look::Selected),
        DirectorMode::Auto => Pill::new("DIRECTOR AUTO", Look::Live),
    });
    let out = &snap.output;
    if let OutputState::Failed(_) = out.state {
        pills.push(Pill::new("OUTPUT FAILED", Look::Warning));
        return pills;
    }
    let live = matches!(out.state, OutputState::Running | OutputState::Stopping);
    let starting = out.state == OutputState::Starting;
    let elapsed = clock(elapsed(out.frames_pushed, fps));
    pills.push(match &out.rtmp_url {
        Some(_) if starting => Pill::new("STREAM STARTING", Look::Warning),
        Some(_) if live => stream_pill(snap, &elapsed),
        _ => Pill::new("OFF AIR", Look::Idle),
    });
    pills.push(match &out.record_path {
        Some(_) if starting => Pill::new("REC STARTING", Look::Warning),
        Some(_) if live => Pill::new(format!("REC {elapsed}"), Look::Live),
        _ => Pill::new("REC", Look::Idle),
    });
    pills
}

/// The stream's pill while a session that streams is live: ON AIR with the
/// elapsed time while connected, else what is wrong, in amber.
fn stream_pill(snap: &EngineSnapshot, elapsed: &str) -> Pill {
    let health = snap.output.stats.as_ref().and_then(|s| s.stream);
    match health {
        Some(h) if h.stalled => Pill::new("STREAM STALLED", Look::Warning),
        Some(h) => match h.state {
            StreamState::Connected => Pill::new(format!("ON AIR {elapsed}"), Look::Live),
            StreamState::Connecting => Pill::new("CONNECTING", Look::Warning),
            StreamState::Reconnecting { .. } => Pill::new("RECONNECTING", Look::Warning),
            StreamState::Down => Pill::new("STREAM DOWN", Look::Warning),
        },
        // No statistics yet: ffmpeg has not reported.
        None => Pill::new("CONNECTING", Look::Warning),
    }
}

/// The studio's name in the header and the window title: its source's
/// name, or how many sources it has.
pub fn studio_title(config: &StudioConfig) -> String {
    match config.sources.as_slice() {
        [one] => one.display_name().to_owned(),
        many => format!("{} sources", many.len()),
    }
}

/// Output time of `frames` at `fps` (none at 0 fps).
pub fn elapsed(frames: u64, fps: u32) -> Duration {
    if fps == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs(frames / u64::from(fps))
}

/// Draws the header for `snap`, under the window title `title`, for an
/// output at `fps`.
pub fn show(ui: &mut Ui, snap: &EngineSnapshot, title: &str, fps: u32) {
    ui.horizontal_centered(|ui| {
        ui.label(
            RichText::new("AUTO CAMERA")
                .size(theme::HEADING)
                .strong()
                .color(theme::TEXT_STRONG),
        );
        ui.label(RichText::new(title).color(theme::TEXT_WEAK));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(clock(snap.media_time))
                    .monospace()
                    .size(theme::HEADING)
                    .color(theme::TEXT_STRONG),
            )
            .on_hover_text("Studio time: the clock alarms are stamped with");
            ui.add_space(8.0);
            for pill in pills(snap, fps).iter().rev() {
                draw(ui, pill);
            }
        });
    });
}

/// What the banner over the monitors says once the engine has stopped.
pub const STOPPED_BANNER: &str = "Nothing is going out: the engine has stopped. The status \
                                  bar says why. Close the window and start Auto Camera \
                                  again.";

/// Draws [`STOPPED_BANNER`] across the central area, in red, while the
/// engine of `snap` is not running; nothing otherwise.
pub fn stopped_banner(ui: &mut Ui, snap: &EngineSnapshot) {
    if snap.running {
        return;
    }
    egui::Frame::new()
        .fill(theme::PROGRAM_RED)
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(STOPPED_BANNER)
                    .strong()
                    .color(theme::text_on(theme::PROGRAM_RED)),
            );
        });
    ui.add_space(8.0);
}

/// Paints one pill.
fn draw(ui: &mut Ui, pill: &Pill) {
    let (fill, stroke, text) = match pill.look {
        Look::Idle => (egui::Color32::TRANSPARENT, theme::BORDER, theme::TEXT_WEAK),
        Look::Live => (
            theme::PROGRAM_RED,
            theme::PROGRAM_RED,
            theme::text_on(theme::PROGRAM_RED),
        ),
        Look::Warning => (theme::AMBER, theme::AMBER, theme::text_on(theme::AMBER)),
        Look::Selected => (theme::ACCENT, theme::ACCENT, theme::text_on(theme::ACCENT)),
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(&pill.text)
                    .size(theme::SMALL)
                    .strong()
                    .color(text),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{encoder_stats, snapshot};
    use ac_output::{EncoderStats, StreamHealth};

    /// The pills' texts.
    fn texts(snap: &EngineSnapshot) -> Vec<String> {
        pills(snap, 30).into_iter().map(|p| p.text).collect()
    }

    /// The look of the pill whose text starts with `prefix`.
    fn look(snap: &EngineSnapshot, prefix: &str) -> Look {
        pills(snap, 30)
            .into_iter()
            .find(|p| p.text.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} pill in {:?}", texts(snap)))
            .look
    }

    /// Statistics with a stream in `state`.
    fn streaming(state: StreamState, stalled: bool) -> EncoderStats {
        EncoderStats {
            stream: Some(StreamHealth {
                state,
                stalled,
                outages: 0,
                reconnects: 0,
                bitrate_kbps: None,
            }),
            ..encoder_stats()
        }
    }

    #[test]
    fn an_idle_studio_shows_its_lamps_off() {
        let snap = snapshot();
        assert_eq!(texts(&snap), ["DIRECTOR SUGGEST", "OFF AIR", "REC"]);
        assert_eq!(look(&snap, "OFF AIR"), Look::Idle);
        assert_eq!(look(&snap, "REC"), Look::Idle);
        assert_eq!(look(&snap, "DIRECTOR"), Look::Selected);
    }

    #[test]
    fn a_live_session_counts_its_output_time() {
        let mut snap = snapshot();
        snap.output.state = OutputState::Running;
        snap.output.rtmp_url = Some("rtmp://ingest/app/****".into());
        snap.output.record_path = Some("/tmp/show.mp4".into());
        snap.output.frames_pushed = 30 * 75;
        snap.output.stats = Some(streaming(StreamState::Connected, false));
        assert_eq!(
            texts(&snap),
            ["DIRECTOR SUGGEST", "ON AIR 1:15", "REC 1:15"]
        );
        assert_eq!(look(&snap, "ON AIR"), Look::Live);
        assert_eq!(look(&snap, "REC"), Look::Live);
        // A recording alone leaves the stream lamp off.
        snap.output.rtmp_url = None;
        assert_eq!(look(&snap, "OFF AIR"), Look::Idle);
    }

    #[test]
    fn a_troubled_stream_turns_amber() {
        let mut snap = snapshot();
        snap.output.state = OutputState::Running;
        snap.output.rtmp_url = Some("rtmp://ingest/app/****".into());
        assert_eq!(look(&snap, "CONNECTING"), Look::Warning, "no stats yet");
        for (state, stalled, text) in [
            (StreamState::Connecting, false, "CONNECTING"),
            (
                StreamState::Reconnecting { attempt: 2 },
                false,
                "RECONNECTING",
            ),
            (StreamState::Down, false, "STREAM DOWN"),
            (StreamState::Connected, true, "STREAM STALLED"),
        ] {
            snap.output.stats = Some(streaming(state, stalled));
            assert_eq!(look(&snap, text), Look::Warning, "{state:?}");
        }
        snap.output.state = OutputState::Starting;
        assert_eq!(look(&snap, "STREAM STARTING"), Look::Warning);
    }

    #[test]
    fn a_failed_output_and_a_stopped_engine_say_so_first() {
        let mut snap = snapshot();
        snap.output.state = OutputState::Failed("ffmpeg exited".into());
        assert_eq!(texts(&snap), ["DIRECTOR SUGGEST", "OUTPUT FAILED"]);
        snap.running = false;
        assert_eq!(texts(&snap)[0], "ENGINE STOPPED");
        assert_eq!(look(&snap, "ENGINE"), Look::Live);
    }

    #[test]
    fn the_director_lamp_follows_its_mode() {
        let mut snap = snapshot();
        snap.director.mode = DirectorMode::Auto;
        assert_eq!(look(&snap, "DIRECTOR AUTO"), Look::Live);
        snap.director.mode = DirectorMode::Off;
        assert_eq!(look(&snap, "DIRECTOR OFF"), Look::Idle);
    }

    #[test]
    fn elapsed_time_needs_a_frame_rate() {
        assert_eq!(elapsed(90, 30), Duration::from_secs(3));
        assert_eq!(elapsed(89, 30), Duration::from_secs(2), "whole seconds");
        assert_eq!(elapsed(1000, 0), Duration::ZERO);
    }

    #[test]
    fn the_header_and_banner_render_headless() {
        let ctx = egui::Context::default();
        let mut snap = snapshot();
        let painted = |snap: &EngineSnapshot| {
            let out = ctx.run_ui(egui::RawInput::default(), |ui| {
                egui::Panel::top("header").show(ui, |ui| show(ui, snap, "Stage", 30));
                stopped_banner(ui, snap);
            });
            out.shapes.iter().any(|c| {
                matches!(&c.shape, egui::Shape::Text(t)
                    if t.galley.text().starts_with("Nothing is going out"))
            })
        };
        assert!(!painted(&snap), "a running engine has no banner");
        snap.running = false;
        assert!(painted(&snap));
    }
}
