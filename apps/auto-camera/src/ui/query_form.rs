//! The add-query form: what the operator is typing, and how the engine will
//! read it.
//!
//! # Theory
//!
//! An operator's prompt is not the detector's prompt (ADR 0003): "speaker
//! upper body" asks the detector for "a person speaking" and asks for
//! Medium and Medium close shots. The form reads the prompt on every frame
//! with the engine's own rules ([`ac_engine::read_prompt`]) and the same
//! reader, built from the same config ([`ac_engine::PromptReader::of`]:
//! the lexicon, and whether the detector sees actions), so the parse line
//! under the prompt ("Detector sees: a person speaking · Framing: Medium,
//! Medium close (person)") and the notices under it are exactly what the
//! engine will make of it. On OWLv2 "speaker" draws a notice that the
//! detector cannot see speaking; on the sidecar it draws none.
//!
//! The size checkboxes **follow the parse** until the operator touches one:
//! while untouched they show the prompt's shot sizes, or the engine's
//! default (medium and close) when it names none. The first click hands
//! them to the operator for the rest of this query. Either way the command
//! carries the ticked boxes as explicit presets, which always win in the
//! engine, so what is ticked is what gets made. Adding a query resets the
//! form, and the boxes follow again.
//!
//! # Invariants
//!
//! * The form owns only what is being typed; it never talks to the engine.
//! * A prompt with nothing to frame ("close-up") disables Add, with the
//!   parser's reason as the hover text.
//! * Literal prompts are sent as typed; their parse line says "(literal)".

use ac_core::{ParsedPrompt, Preset, ShotSize};
use ac_engine::{EngineCommand, PromptReader, default_presets, preset_for, read_prompt};
use eframe::egui::{self, Color32, RichText, Ui};

use super::text::{parse_line, parse_notes};

/// Framings offered for new queries, with their labels.
pub const SIZES: [(ShotSize, &str); 4] = [
    (ShotSize::Full, "Full"),
    (ShotSize::Medium, "Medium"),
    (ShotSize::MediumClose, "Medium close"),
    (ShotSize::Close, "Close"),
];

/// The prompt field's hover text.
const PROMPT_HINT: &str = "Who or what to frame, optionally with shot words: \
                           \"speaker upper body\", \"guitar close-up\", \"drummer wide\"";

/// The literal checkbox's hover text.
const LITERAL_HINT: &str =
    "Send the prompt to the detector exactly as typed: no shot words, no role words";

/// Shown when the operator's ticks override the prompt's shot words.
pub const OVERRIDDEN: &str = "Sizes chosen by hand: the prompt's shot words are not used.";

/// The add-query form; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryForm {
    /// Operator-facing name.
    pub name: String,
    /// The operator's prompt.
    pub prompt: String,
    /// Which of [`SIZES`] to create shots for.
    pub sizes: [bool; 4],
    /// The operator ticked or cleared a size, so the sizes no longer
    /// follow the prompt's shot words.
    pub touched: bool,
    /// Send the prompt to the detector as typed.
    pub literal: bool,
}

impl Default for QueryForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            prompt: String::new(),
            sizes: ticks(&sizes_of(&default_presets())),
            touched: false,
            literal: false,
        }
    }
}

/// The sizes of `presets`, in order.
fn sizes_of(presets: &[Preset]) -> Vec<ShotSize> {
    presets.iter().map(|p| p.size).collect()
}

/// The checkboxes of [`SIZES`] that `sizes` ticks.
fn ticks(sizes: &[ShotSize]) -> [bool; 4] {
    SIZES.map(|(size, _)| sizes.contains(&size))
}

impl QueryForm {
    /// The prompt as the engine will read it, with `prompts`.
    pub fn parse(&self, prompts: &PromptReader) -> ParsedPrompt {
        read_prompt(&self.prompt, self.literal, prompts)
    }

    /// Unless the operator touched them, sets the sizes to those `parse`
    /// asks for, or to the default when it names none.
    pub fn follow(&mut self, parse: &ParsedPrompt) {
        if self.touched {
            return;
        }
        self.sizes = if parse.is_valid() && !parse.shots.is_empty() {
            ticks(&parse.shots)
        } else {
            ticks(&sizes_of(&default_presets()))
        };
    }

    /// True when the ticked sizes differ from the shot words of `parse`
    /// because the operator chose them.
    pub fn overrides(&self, parse: &ParsedPrompt) -> bool {
        self.touched && !parse.shots.is_empty() && self.sizes != ticks(&parse.shots)
    }

    /// The ticked framings, in [`SIZES`] order.
    pub fn presets(&self) -> Vec<Preset> {
        SIZES
            .iter()
            .zip(self.sizes)
            .filter(|(_, on)| *on)
            .map(|((size, _), _)| preset_for(*size))
            .collect()
    }

    /// The `AddQuery` command, or why the form is incomplete.
    pub fn command(&self, prompts: &PromptReader) -> Result<EngineCommand, String> {
        let (name, prompt) = (self.name.trim(), self.prompt.trim());
        if name.is_empty() {
            return Err("give the query a name".into());
        }
        if prompt.is_empty() {
            return Err("describe what to look for".into());
        }
        if let Some(error) = self.parse(prompts).error() {
            return Err(error.to_string());
        }
        let presets = self.presets();
        if presets.is_empty() {
            return Err("pick at least one framing".into());
        }
        Ok(EngineCommand::AddQuery {
            name: name.to_owned(),
            prompt: prompt.to_owned(),
            presets: Some(presets),
            literal: self.literal,
        })
    }
}

/// Draws the form and returns the `AddQuery` command when Add was clicked.
pub fn show(ui: &mut Ui, form: &mut QueryForm, prompts: &PromptReader) -> Option<EngineCommand> {
    egui::Grid::new("query-form").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut form.name);
        ui.end_row();
        ui.label("Prompt");
        ui.text_edit_singleline(&mut form.prompt)
            .on_hover_text(PROMPT_HINT);
        ui.end_row();
    });
    let parse = form.parse(prompts);
    form.follow(&parse);
    let mut touched = false;
    ui.horizontal_wrapped(|ui| {
        for ((_, label), on) in SIZES.iter().zip(form.sizes.iter_mut()) {
            touched |= ui.checkbox(on, *label).changed();
        }
        ui.checkbox(&mut form.literal, "Literal")
            .on_hover_text(LITERAL_HINT);
    });
    form.touched |= touched;
    if let Some(line) = parse_line(&parse) {
        let text = RichText::new(line).small();
        ui.label(if parse.is_valid() {
            text.weak()
        } else {
            text.color(Color32::from_rgb(230, 140, 40))
        });
    }
    for note in parse_notes(&parse) {
        ui.label(RichText::new(note).small().weak());
    }
    if form.overrides(&parse) {
        ui.label(RichText::new(OVERRIDDEN).small().weak());
    }
    let ready = form.command(prompts);
    let add = ui.add_enabled(ready.is_ok(), egui::Button::new("Add query"));
    match ready {
        Ok(cmd) if add.clicked() => {
            *form = QueryForm::default();
            Some(cmd)
        }
        Ok(_) => None,
        Err(why) => {
            add.on_disabled_hover_text(why);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShotSize::{Close, Full, Medium, MediumClose};

    /// The built-in lexicon, for a detector that does not see actions
    /// (OWLv2).
    fn owl() -> PromptReader {
        PromptReader::new(ac_core::Lexicon::builtin(), false)
    }

    /// A form with `name` and `prompt` whose sizes followed the prompt.
    fn typed(name: &str, prompt: &str) -> QueryForm {
        let mut form = QueryForm {
            name: name.into(),
            prompt: prompt.into(),
            ..QueryForm::default()
        };
        let parse = form.parse(&owl());
        form.follow(&parse);
        form
    }

    /// The sizes of the command's presets.
    fn command_sizes(form: &QueryForm) -> Vec<ShotSize> {
        match form.command(&owl()) {
            Ok(EngineCommand::AddQuery {
                presets: Some(p), ..
            }) => sizes_of(&p),
            other => panic!("a command with presets, got {other:?}"),
        }
    }

    #[test]
    fn a_complete_query_form_adds_a_query() {
        let mut form = typed(" Lead singer ", "person singing");
        form.sizes = [true, false, false, true];
        form.touched = true;
        let Ok(EngineCommand::AddQuery {
            name,
            prompt,
            presets,
            literal,
        }) = form.command(&owl())
        else {
            panic!("a command");
        };
        assert_eq!(name, "Lead singer");
        assert_eq!(prompt, "person singing");
        assert_eq!(presets.map(|p| sizes_of(&p)), Some(vec![Full, Close]));
        assert!(!literal);
        assert_eq!(QueryForm::default().presets().len(), 2);
    }

    #[test]
    fn incomplete_query_forms_say_what_is_missing() {
        let mut form = QueryForm::default();
        assert_eq!(form.command(&owl()).unwrap_err(), "give the query a name");
        form.name = "Pastor".into();
        form.prompt = "   ".into();
        assert_eq!(
            form.command(&owl()).unwrap_err(),
            "describe what to look for"
        );
        form.prompt = "person at the lectern".into();
        form.sizes = [false; 4];
        assert_eq!(
            form.command(&owl()).unwrap_err(),
            "pick at least one framing"
        );
        // Shot words with nothing to frame: the parser's reason.
        form.prompt = "close-up".into();
        let why = form.command(&owl()).unwrap_err();
        assert!(why.contains("only names a shot size"), "{why}");
    }

    #[test]
    fn sizes_follow_the_prompt_until_touched() {
        let mut form = typed("Speaker", "speaker upper body");
        assert_eq!(form.sizes, [false, true, true, false]);
        assert_eq!(command_sizes(&form), [Medium, MediumClose]);
        // Retyping follows the new shot words.
        form.prompt = "speaker close-up".into();
        form.follow(&form.parse(&owl()));
        assert_eq!(command_sizes(&form), [Close]);
        // No shot words: the engine's default.
        form.prompt = "speaker".into();
        form.follow(&form.parse(&owl()));
        assert_eq!(command_sizes(&form), [Medium, Close]);
        // Once touched, the operator's choice stays.
        form.sizes = [true, false, false, false];
        form.touched = true;
        form.prompt = "speaker close-up".into();
        let parse = form.parse(&owl());
        form.follow(&parse);
        assert_eq!(command_sizes(&form), [Full]);
        assert!(form.overrides(&parse));
        // Matching the shot words by hand is not an override.
        form.sizes = [false, false, false, true];
        assert!(!form.overrides(&parse));
    }

    #[test]
    fn an_invalid_prompt_keeps_the_default_sizes() {
        let form = typed("X", "upper body");
        assert_eq!(form.sizes, QueryForm::default().sizes);
        assert!(form.command(&owl()).is_err());
    }

    #[test]
    fn literal_prompts_are_sent_as_typed() {
        let mut form = QueryForm {
            literal: true,
            ..typed("Experiment", "speaker upper body")
        };
        form.follow(&form.parse(&owl()));
        let parse = form.parse(&owl());
        assert_eq!(parse.model_phrase, "speaker upper body");
        let Ok(EngineCommand::AddQuery {
            literal, presets, ..
        }) = form.command(&owl())
        else {
            panic!("a command");
        };
        assert!(literal);
        assert_eq!(presets.map(|p| sizes_of(&p)), Some(vec![Medium, Close]));
    }

    /// Drives [`show`] through egui frames, as the window does.
    struct Window {
        /// The egui context, kept across frames.
        ctx: egui::Context,
        /// The form being shown.
        form: QueryForm,
        /// How the form reads its prompt.
        prompts: PromptReader,
        /// The newest frame's output.
        out: egui::FullOutput,
    }

    impl Window {
        /// A window showing `form`, not drawn yet.
        fn new(form: QueryForm) -> Self {
            Self {
                ctx: egui::Context::default(),
                form,
                prompts: owl(),
                out: egui::FullOutput::default(),
            }
        }

        /// Draws one frame with `events`; returns what [`show`] returned.
        fn frame(&mut self, events: Vec<egui::Event>) -> Option<EngineCommand> {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                events,
                ..Default::default()
            };
            let mut cmd = None;
            let (form, prompts) = (&mut self.form, &self.prompts);
            self.out = self.ctx.run_ui(input, |ui| {
                cmd = show(ui, form, prompts);
            });
            cmd
        }

        /// Every text the newest frame painted, with its centre.
        fn texts(&self) -> Vec<(&str, egui::Pos2)> {
            self.out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => {
                        Some((t.galley.text(), t.pos + t.galley.rect.center().to_vec2()))
                    }
                    _ => None,
                })
                .collect()
        }

        /// Where the newest frame painted exactly `text`.
        fn find(&self, text: &str) -> egui::Pos2 {
            self.texts()
                .into_iter()
                .find_map(|(t, pos)| (t == text).then_some(pos))
                .unwrap_or_else(|| panic!("{text:?} is not on screen"))
        }

        /// Clicks on `text`: press on one frame, release on the next.
        /// Returns what [`show`] returned on the release.
        fn click(&mut self, text: &str) -> Option<EngineCommand> {
            let pos = self.find(text);
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![egui::Event::PointerMoved(pos), button(true)]);
            let cmd = self.frame(vec![button(false)]);
            self.frame(Vec::new());
            cmd
        }
    }

    #[test]
    fn the_window_follows_the_prompt_until_a_size_is_clicked() {
        // The prompt is typed before the first frame; nothing calls
        // `follow` but `show`.
        let mut w = Window::new(QueryForm {
            name: "Speaker".into(),
            prompt: "speaker upper body".into(),
            ..QueryForm::default()
        });
        assert_eq!(w.frame(Vec::new()), None);
        assert_eq!(w.form.sizes, [false, true, true, false]);
        w.form.prompt = "speaker close-up".into();
        w.frame(Vec::new());
        assert_eq!(w.form.sizes, [false, false, false, true]);
        assert!(!w.form.touched);
        // A click on a size hands the sizes to the operator.
        assert_eq!(w.click("Full"), None);
        assert!(w.form.touched);
        assert_eq!(w.form.sizes, [true, false, false, true]);
        w.form.prompt = "speaker upper body".into();
        w.frame(Vec::new());
        assert_eq!(
            w.form.sizes,
            [true, false, false, true],
            "no longer follows"
        );
    }

    #[test]
    fn the_window_notes_an_action_role_under_the_parse_line() {
        let mut w = Window::new(QueryForm {
            name: "Speaker".into(),
            prompt: "speaker upper body".into(),
            ..QueryForm::default()
        });
        w.frame(Vec::new());
        let notes = parse_notes(&w.form.parse(&owl()));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("an action"), "{notes:?}");
        let line =
            w.find("Detector sees: a person speaking · Framing: Medium, Medium close (person)");
        assert!(
            w.find(&notes[0]).y > line.y,
            "the note is under the parse line"
        );
        // A detector that sees actions (the sidecar): the same parse line,
        // and no note.
        w.prompts.sees_actions = true;
        w.frame(Vec::new());
        w.find("Detector sees: a person speaking · Framing: Medium, Medium close (person)");
        let painted = w.texts();
        assert!(
            painted.iter().all(|(t, _)| !t.starts_with("Note:")),
            "{painted:?}"
        );
        assert!(parse_notes(&w.form.parse(&w.prompts)).is_empty());
    }

    #[test]
    fn the_window_adds_only_on_click_and_then_resets() {
        let mut w = Window::new(QueryForm {
            name: "Speaker".into(),
            prompt: "speaker upper body".into(),
            ..QueryForm::default()
        });
        for _ in 0..3 {
            assert_eq!(w.frame(Vec::new()), None, "drawing never adds");
        }
        let Some(EngineCommand::AddQuery {
            name,
            prompt,
            presets,
            literal,
        }) = w.click("Add query")
        else {
            panic!("Add query sends the command");
        };
        assert_eq!(
            (name.as_str(), prompt.as_str()),
            ("Speaker", "speaker upper body")
        );
        assert_eq!(
            presets.map(|p| sizes_of(&p)),
            Some(vec![Medium, MediumClose])
        );
        assert!(!literal);
        assert_eq!(w.form, QueryForm::default(), "the form resets");
        // An invalid prompt keeps Add disabled.
        w.form = QueryForm {
            name: "X".into(),
            prompt: "close-up".into(),
            ..QueryForm::default()
        };
        w.frame(Vec::new());
        assert_eq!(w.click("Add query"), None);
        assert_eq!(w.form.prompt, "close-up");
    }
}
