//! Building queries: the operator's prompt read once, its framings settled.
//!
//! # Theory
//!
//! A query starts as operator input: a name, a prompt and, optionally,
//! presets and the literal flag, given in a `[[queries]]` table or by an
//! `AddQuery` command. Both go through [`build_query`] exactly once, when
//! the query is created:
//!
//! 1. **Read the prompt** with the studio's [`Lexicon`]
//!    ([`ac_core::parse_prompt`]), or take it as typed when it is literal
//!    ([`ac_core::literal_prompt`]). "speaker upper body" becomes the
//!    detector phrase "a person speaking", the shot sizes Medium and Medium
//!    close and the person basis. A prompt that names nothing to frame
//!    (empty, or shot words only, such as "close-up") is refused with the
//!    parser's reason.
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
//! # Invariants
//!
//! * `Query::prompt` is the operator's text, trimmed; only the parse's
//!   model phrase reaches a detector.
//! * A prompt with no shot words and no whole role word yields its own
//!   trimmed text as the detector phrase and, without explicit presets,
//!   medium and close: exactly what it meant before shot words existed.
//! * A built query is valid: non-empty name, a usable prompt, at least one
//!   preset, none of them wide, unique preset ids.

use ac_core::{
    Lexicon, ParsedPrompt, Preset, Query, QueryId, ShotSize, literal_prompt, parse_prompt,
};

use crate::config::{preset_for, validate_query};

/// The prompt as the detector and framing will read it: parsed with
/// `lexicon`, or taken as typed when `literal`.
pub fn read_prompt(prompt: &str, literal: bool, lexicon: &Lexicon) -> ParsedPrompt {
    if literal {
        literal_prompt(prompt, lexicon)
    } else {
        parse_prompt(prompt, lexicon)
    }
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
    lexicon: &Lexicon,
) -> Result<Query, String> {
    let (name, prompt) = (name.trim(), prompt.trim());
    // Name and prompt first, with their own messages, before the parse.
    validate_query(name, prompt, &default_presets())?;
    let parse = read_prompt(prompt, literal, lexicon);
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
    use ShotSize::{Close, Full, Medium, MediumClose};
    use ac_core::{Basis, PromptWarning};

    /// Builds `prompt` as query "q" with the built-in lexicon.
    fn build(prompt: &str, presets: Option<Vec<Preset>>, literal: bool) -> Result<Query, String> {
        build_query(
            QueryId("q".into()),
            "Q",
            prompt,
            presets,
            literal,
            &Lexicon::builtin(),
        )
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
        let p = read_prompt("a close-up of the", false, &Lexicon::builtin());
        assert!(matches!(p.error(), Some(PromptWarning::NoSubject { .. })));
    }

    #[test]
    fn invalid_names_and_presets_are_refused() {
        let id = || QueryId("q".into());
        let lexicon = Lexicon::builtin();
        assert!(build_query(id(), " ", "guitar", None, false, &lexicon).is_err());
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
        let parse = read_prompt("drummer wide", false, &Lexicon::builtin());
        assert_eq!(settle_presets(None, &parse), shot_presets(&[Full]));
        let explicit = shot_presets(&[Close]);
        assert_eq!(settle_presets(Some(explicit.clone()), &parse), explicit);
        let plain = read_prompt("drum kit", false, &Lexicon::builtin());
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
            &lexicon,
        )
        .unwrap();
        assert_eq!(q.detector_phrase(), "a person singing");
        assert_eq!(sizes(&q), [Medium]);
    }
}
