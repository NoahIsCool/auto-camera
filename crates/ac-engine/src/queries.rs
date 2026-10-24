//! Building queries: the operator's prompt read once, its framings settled.
//!
//! # Theory
//!
//! A query starts as operator input: a name, a prompt and, optionally,
//! presets and the literal flag, given in a `[[queries]]` table or by an
//! `AddQuery` command. Both go through [`build_query`] exactly once, when
//! the query is created:
//!
//! 1. **Read the prompt** with the studio's [`PromptReader`]: parse it with
//!    the studio's [`Lexicon`] ([`ac_core::parse_prompt`]), or take it as
//!    typed when it is literal ([`ac_core::literal_prompt`]). "speaker upper
//!    body" becomes the detector phrase "a person speaking", the shot sizes
//!    Medium and Medium close and the person basis. A prompt that names
//!    nothing to frame (empty, or shot words only, such as "close-up") is
//!    refused with the parser's reason.
//! 2. **Settle the presets**, first rule that applies: presets given
//!    explicitly (a config's `presets`, a command's `Some`) always win; else
//!    one standard preset per shot size the prompt's shot words name, in
//!    typed order ([`shot_presets`]); else the default, medium and close
//!    ([`default_presets`]). Explicit presets and shot words are never
//!    merged.
//! 3. **Validate** name, prompt and presets ([`validate_query`]).
//!
//! The parse is stored on the [`Query`] and never recomputed: the detector
//! receives [`Query::detector_phrase`] on every pass, snapshots carry the
//! parse for the operator window, and framing reads its basis
//! ([`Query::framing_extent`]). See ADR 0003.
//!
//! **What the detector sees.** The parser adds a
//! [`PromptWarning::ActionRole`] notice to a prompt rewritten to a role
//! whose phrase names an action ("speaker" → "a person speaking"), because
//! OWLv2 sees people and objects, not actions. Whether that holds is the
//! detector's, not the words': the design asks for the warning "when the
//! chosen model lacks the capability" (§4.2), read from the detector's
//! declared capabilities (Phase 2, "Capability contract"). So the reader
//! carries the one capability the engine knows so far,
//! [`crate::config::DetectorSettings::sees_actions`] (the sidecar's SAM 3.1 does, OWLv2
//! and the synthetic detector do not), and [`read_prompt`] drops the notice
//! when the detector sees actions. The window's form reads its preview
//! through the same reader, so the form, a query's hover and the headless
//! summary agree on it.
//!
//! # Invariants
//!
//! * `Query::prompt` is the operator's text, trimmed; only the parse's
//!   model phrase reaches a detector.
//! * A prompt with no shot words and no whole role word yields its own
//!   trimmed text as the detector phrase and, without explicit presets,
//!   medium and close: exactly what it meant before shot words existed.
//! * A parse carries an action-role notice exactly when the parser gave it
//!   one and the detector does not see actions; the detector changes
//!   nothing else in it.
//! * A built query is valid: non-empty name, a usable prompt, at least one
//!   preset, none of them wide, unique preset ids.

use ac_core::{
    Lexicon, ParsedPrompt, Preset, PromptWarning, Query, QueryId, ShotSize, literal_prompt,
    parse_prompt,
};

use crate::config::{StudioConfig, preset_for, validate_query};

/// How a studio reads its prompts: the words it knows and what its
/// detector can see; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptReader {
    /// The prompt parser's vocabulary
    /// ([`StudioConfig::prompt_lexicon`]).
    pub lexicon: Lexicon,
    /// The detector sees what a person is doing
    /// ([`crate::config::DetectorSettings::sees_actions`]), so action roles
    /// draw no notice.
    pub sees_actions: bool,
}

impl PromptReader {
    /// A reader with `lexicon`, for a detector that does or does not see
    /// actions.
    pub fn new(lexicon: Lexicon, sees_actions: bool) -> Self {
        Self {
            lexicon,
            sees_actions,
        }
    }

    /// The reader of `config`'s studio: its lexicon and its detector.
    pub fn of(config: &StudioConfig) -> Self {
        Self::new(config.prompt_lexicon(), config.detector.sees_actions())
    }
}

/// The prompt as the detector and framing will read it: parsed with the
/// reader's lexicon, or taken as typed when `literal`, without the
/// action-role notice when the detector sees actions.
pub fn read_prompt(prompt: &str, literal: bool, reader: &PromptReader) -> ParsedPrompt {
    let mut parse = if literal {
        literal_prompt(prompt, &reader.lexicon)
    } else {
        parse_prompt(prompt, &reader.lexicon)
    };
    if reader.sees_actions {
        parse
            .warnings
            .retain(|w| !matches!(w, PromptWarning::ActionRole { .. }));
    }
    parse
}

/// One standard preset ([`preset_for`]) per size, in order.
pub fn shot_presets(sizes: &[ShotSize]) -> Vec<Preset> {
    sizes.iter().map(|&size| preset_for(size)).collect()
}

/// The presets of a query that names none: medium and close.
pub fn default_presets() -> Vec<Preset> {
    shot_presets(&[ShotSize::Medium, ShotSize::Close])
}

/// The presets a query gets: `explicit` when given, else those of the
/// parse's shot sizes, else [`default_presets`]; see the module docs.
pub fn settle_presets(explicit: Option<Vec<Preset>>, parse: &ParsedPrompt) -> Vec<Preset> {
    match explicit {
        Some(presets) => presets,
        None if !parse.shots.is_empty() => shot_presets(&parse.shots),
        None => default_presets(),
    }
}

/// Builds the query `id` from operator input; see the module docs.
/// The error names the query and says what is wrong.
pub fn build_query(
    id: QueryId,
    name: &str,
    prompt: &str,
    presets: Option<Vec<Preset>>,
    literal: bool,
    reader: &PromptReader,
) -> Result<Query, String> {
    let (name, prompt) = (name.trim(), prompt.trim());
    // Name and prompt first, with their own messages, before the parse.
    validate_query(name, prompt, &default_presets())?;
    let parse = read_prompt(prompt, literal, reader);
    if let Some(error) = parse.error() {
        return Err(format!("query {name:?}: {error}"));
    }
    let presets = settle_presets(presets, &parse);
    validate_query(name, prompt, &presets)?;
    Ok(Query::new(id, name, prompt, parse, presets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DetectorSettings;
    use ShotSize::{Close, Full, Medium, MediumClose};
    use ac_core::Basis;

    /// The built-in lexicon, for a detector that does not see actions
    /// (OWLv2).
    fn owl() -> PromptReader {
        PromptReader::new(Lexicon::builtin(), false)
    }

    /// Builds `prompt` as query "q" with [`owl`].
    fn build(prompt: &str, presets: Option<Vec<Preset>>, literal: bool) -> Result<Query, String> {
        build_query(QueryId("q".into()), "Q", prompt, presets, literal, &owl())
    }

    /// The sizes of `q`'s presets.
    fn sizes(q: &Query) -> Vec<ShotSize> {
        q.presets.iter().map(|p| p.size).collect()
    }

    #[test]
    fn shot_words_pick_the_presets_and_roles_the_detector_phrase() {
        let q = build("speaker upper body", None, false).unwrap();
        assert_eq!(q.prompt, "speaker upper body");
        assert_eq!(q.detector_phrase(), "a person speaking");
        assert_eq!(sizes(&q), [Medium, MediumClose]);
        let ids: Vec<&str> = q.presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["medium", "medium_close"]);
        assert_eq!(q.basis(), Basis::Person);
        // Typed order is kept.
        let q = build("drummer full and close", None, false).unwrap();
        assert_eq!(sizes(&q), [Full, Close]);
        assert_eq!(q.detector_phrase(), "a person playing drums");
    }

    #[test]
    fn explicit_presets_always_win_over_shot_words() {
        let q = build("guitarist close-up", Some(shot_presets(&[Full])), false).unwrap();
        assert_eq!(sizes(&q), [Full]);
        // The parse is still what the detector and the window see.
        assert_eq!(q.parse.shots, [Close]);
        assert_eq!(q.detector_phrase(), "a person playing guitar");
    }

    #[test]
    fn prompts_without_shot_or_role_words_keep_their_phrase_and_presets() {
        for prompt in [
            "person",
            "a person speaking at a lectern",
            "a person playing guitar",
        ] {
            let q = build(&format!("  {prompt} "), None, false).unwrap();
            assert_eq!(q.prompt, prompt);
            assert_eq!(q.detector_phrase(), prompt);
            assert_eq!(q.presets, default_presets());
            assert!(q.parse.warnings.is_empty(), "{prompt}: {:?}", q.parse);
        }
    }

    #[test]
    fn literal_prompts_are_sent_as_typed() {
        let q = build(" speaker upper body ", None, true).unwrap();
        assert_eq!(q.detector_phrase(), "speaker upper body");
        assert!(q.parse.literal && q.parse.shots.is_empty());
        assert_eq!(q.presets, default_presets());
        // A literal prompt may be nothing but shot words.
        assert_eq!(
            build("close-up", None, true).unwrap().detector_phrase(),
            "close-up"
        );
    }

    #[test]
    fn prompts_with_nothing_to_frame_are_refused_with_the_reason() {
        let err = build("close-up", None, false).unwrap_err();
        assert!(
            err.starts_with("query \"Q\": ") && err.contains("only names a shot size"),
            "{err}"
        );
        assert!(build("upper body", Some(default_presets()), false).is_err());
        let err = build("   ", None, false).unwrap_err();
        assert!(err.contains("prompt must not be empty"), "{err}");
        // Only grammar words around the shot word are nothing to frame too.
        let p = read_prompt("a close-up of the", false, &owl());
        assert!(matches!(p.error(), Some(PromptWarning::NoSubject { .. })));
    }

    #[test]
    fn invalid_names_and_presets_are_refused() {
        let id = || QueryId("q".into());
        assert!(build_query(id(), " ", "guitar", None, false, &owl()).is_err());
        assert!(build("guitar", Some(Vec::new()), false).is_err());
        let wide = Some(vec![preset_for(ShotSize::Wide)]);
        assert!(build("guitar", wide, false).unwrap_err().contains("wide"));
        let twice = Some(shot_presets(&[Close, Close]));
        assert!(
            build("guitar", twice, false)
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn settle_presets_follows_the_precedence() {
        let parse = read_prompt("drummer wide", false, &owl());
        assert_eq!(settle_presets(None, &parse), shot_presets(&[Full]));
        let explicit = shot_presets(&[Close]);
        assert_eq!(settle_presets(Some(explicit.clone()), &parse), explicit);
        let plain = read_prompt("drum kit", false, &owl());
        assert_eq!(settle_presets(None, &plain), default_presets());
    }

    /// The rows of the README's prompt table, read from the README itself:
    /// (prompt, detector phrase, framing), as written.
    fn readme_rows() -> Vec<(String, String, String)> {
        const README: &str = include_str!("../../../README.md");
        const HEADER: &str = "| Prompt | Detector sees | Framing |";
        let mut lines = README.lines().skip_while(|l| l.trim() != HEADER);
        assert!(lines.next().is_some(), "the README has a prompt table");
        lines.next(); // |---|---|---|
        lines
            .take_while(|l| l.trim_start().starts_with('|'))
            .map(|l| {
                let cells: Vec<&str> = l
                    .trim()
                    .trim_matches('|')
                    .split('|')
                    .map(str::trim)
                    .collect();
                assert_eq!(cells.len(), 3, "{l}");
                let prompt = cells[0].trim_matches('`');
                (
                    prompt.to_string(),
                    cells[1].to_string(),
                    cells[2].to_string(),
                )
            })
            .collect()
    }

    /// The README's "Framing" cell for a query built without presets.
    fn framing_cell(q: &Query) -> String {
        let sizes = if q.parse.shots.is_empty() {
            "the query's presets".to_string()
        } else {
            let names: Vec<&str> = q.presets.iter().map(|p| p.name.as_str()).collect();
            names.join(", ")
        };
        format!("{sizes} ({})", q.basis().label())
    }

    #[test]
    fn the_readme_prompt_table_holds() {
        let rows = readme_rows();
        assert!(rows.len() >= 6, "{rows:?}");
        for (prompt, phrase, framing) in rows {
            let q = build(&prompt, None, false).unwrap();
            assert_eq!(q.detector_phrase(), phrase, "{prompt}");
            assert_eq!(framing_cell(&q), framing, "{prompt}");
            if q.parse.shots.is_empty() {
                assert_eq!(q.presets, default_presets(), "{prompt}");
            }
        }
    }

    #[test]
    fn a_custom_lexicon_is_used() {
        let mut lexicon = Lexicon::builtin();
        lexicon.set_role(ac_core::RoleWord {
            word: "worship leader".into(),
            phrase: "a person singing".into(),
            needs: None,
        });
        let q = build_query(
            QueryId("w".into()),
            "W",
            "worship leader medium",
            None,
            false,
            &PromptReader::new(lexicon, false),
        )
        .unwrap();
        assert_eq!(q.detector_phrase(), "a person singing");
        assert_eq!(sizes(&q), [Medium]);
    }

    /// True when `parse` carries an action-role notice.
    fn notes_an_action(parse: &ParsedPrompt) -> bool {
        parse
            .warnings
            .iter()
            .any(|w| matches!(w, PromptWarning::ActionRole { .. }))
    }

    #[test]
    fn the_action_role_notice_is_only_for_a_detector_that_cannot_see_actions() {
        let sam = PromptReader::new(Lexicon::builtin(), true);
        for prompt in ["speaker upper body", "the singers", "pastor close-up"] {
            let (without, with) = (
                read_prompt(prompt, false, &owl()),
                read_prompt(prompt, false, &sam),
            );
            assert!(notes_an_action(&without), "{prompt}: {without:?}");
            assert!(!notes_an_action(&with), "{prompt}: {with:?}");
            // Nothing else changes.
            let mut kept = without.clone();
            kept.warnings
                .retain(|w| !matches!(w, PromptWarning::ActionRole { .. }));
            assert_eq!(with, kept, "{prompt}");
        }
        // Other notices stay with either detector.
        let both = read_prompt("singer wide", false, &sam);
        assert!(!notes_an_action(&both));
        assert_eq!(both.warnings.len(), 1, "{both:?}");
        let q = build_query(QueryId("s".into()), "S", "speaker", None, false, &sam).unwrap();
        assert_eq!(q.detector_phrase(), "a person speaking");
        assert!(q.parse.warnings.is_empty(), "{:?}", q.parse);
        assert!(notes_an_action(
            &build("speaker", None, false).unwrap().parse
        ));
        // The studio's reader follows its detector's kind.
        let source = ac_sources::SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 1,
        };
        let mut config = StudioConfig::with_source("stage", source);
        assert_eq!(PromptReader::of(&config), owl());
        config.detector = DetectorSettings::Sidecar(crate::config::SidecarSettings::new(["sam"]));
        assert_eq!(PromptReader::of(&config), sam);
        let owl_kind = DetectorSettings::Owl {
            model: "detector.onnx".into(),
            text_encoder: None,
            tokenizer: "tokenizer.json".into(),
            input_size: None,
            score_threshold: None,
            use_coreml: None,
            people_first: None,
            person_threshold: None,
        };
        for kind in [
            owl_kind,
            DetectorSettings::None,
            DetectorSettings::Synthetic { score: 0.9 },
        ] {
            assert!(!kind.sees_actions(), "{kind:?}");
        }
    }
}
