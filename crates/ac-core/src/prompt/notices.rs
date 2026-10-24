//! Notices about shot words (those left inside the subject, and those read
//! as shot sizes where their everyday reading is likely) and about roles
//! whose detector phrase names an action.
//!
//! # Theory
//!
//! The parser reads shot words only in three places (see [`super`]); the
//! operator should hear about the rest, and about the ones it may have
//! misread, because the parse line is the only other clue:
//!
//! * **Left in the subject.** A shot phrase found inside the subject after
//!   parsing goes to the detector as typed ([`PromptWarning::InSubject`],
//!   or [`PromptWarning::AfterSubjectOnly`] for words such as "wide").
//! * **Maybe misread.** A shot word that is also an everyday word
//!   ([`WordClass`]) is read as a shot size wherever the grammar says so,
//!   but where the everyday reading is likely the parse carries a
//!   [`PromptWarning::MaybeMisread`] notice that points at literal mode:
//!   * a [`WordClass::Modifier`] ending the shot words at the start and
//!     followed right away by the subject, with no "of" or "on": "full
//!     band", "close friend of the speaker", "tight end", "Ms Smith
//!     close-up";
//!   * a [`WordClass::Noun`] starting the shot words at the end, right
//!     after a word that is neither a possessive nor a person or role word:
//!     "a rock face", "marble bust" (but not "speaker face");
//!   * a [`WordClass::Modifier`] starting the shot words at the end, right
//!     after an "-ing" word: "a man standing close".
//!
//!   These are heuristics over word order, not a grammar: "close-up guitar"
//!   and "guitar close-up" draw no notice, "guitar face" does.
//!
//! A role rewrite is shown on the parse line ("Detector sees: a person
//! speaking"), but that line cannot say that OWLv2 boxes people and
//! objects, not actions. On the rehearsal footage it gave "a person
//! speaking" to every person (the speaker in 20 of 20 frames, 300 false
//! boxes on 40 labelled frames), photos on the projection screen included.
//! So a role tagged [`RoleNeeds::Action`] draws a
//! [`PromptWarning::ActionRole`] notice, telling the operator to take that
//! person's shot by hand. It does not yet depend on the detector: a model
//! that sees actions declares it through the capability contract (design
//! Phase 2), which does not exist yet. An untagged role, such as a studio's
//! own entry without `needs`, draws none.
//!
//! # Invariants
//!
//! * Notices never change the parse; they only describe it.
//! * Pure and total over any tokens and matches the parser produced.

use super::head::is_participle;
use super::scan::{Match, Role, Table};
use super::words::{Token, phrase_key};
use super::{Lead, PromptWarning, typed};
use crate::lexicon::{RoleNeeds, WordClass};

/// Shot phrases left inside a subject, as typed, without repeats and
/// without those whose keys are in `skip`: those that may count anywhere,
/// and those that count only after a subject.
pub(super) fn shot_words_in(
    table: &Table<'_>,
    subject: &str,
    words: &[Token],
    skip: &[String],
) -> (Vec<String>, Vec<String>) {
    let (mut anywhere, mut after_only) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < words.len() {
        match table.phrase_at(words, i, words.len(), true) {
            Some(m) => {
                let phrase = &subject[words[m.start].start..words[m.end - 1].end];
                let key = phrase_key(phrase);
                let list: &mut Vec<String> = if table.entry(m).after_subject {
                    &mut after_only
                } else {
                    &mut anywhere
                };
                if !skip.contains(&key) && !list.iter().any(|f| phrase_key(f) == key) {
                    list.push(phrase.to_string());
                }
                i = m.end;
            }
            None => i += 1,
        }
    }
    (anywhere, after_only)
}

/// The notice for a subject that is `role`, when its phrase names an
/// action; see the module docs.
pub(super) fn action_role(role: Option<Role<'_>>) -> Option<PromptWarning> {
    let role = role.filter(|r| r.needs == Some(RoleNeeds::Action))?;
    Some(PromptWarning::ActionRole {
        role: role.word.to_string(),
        phrase: role.phrase.to_string(),
    })
}

/// Notices for shot words read as shot sizes where their everyday reading
/// is likely; see the module docs. `lead` and `trail` are the shot words
/// read at the start and at the end, and `e` the index of the first word of
/// `trail`.
pub(super) fn misreads(
    table: &Table<'_>,
    text: &str,
    tokens: &[Token],
    lead: &Lead,
    trail: &[Match],
    e: usize,
) -> Vec<PromptWarning> {
    let notice = |m: Match, from: usize, to: usize| PromptWarning::MaybeMisread {
        phrase: typed(text, tokens, &m).to_string(),
        words: text[tokens[from].start..tokens[to].end].to_string(),
    };
    let mut out = Vec::new();
    if lead.connector.is_none()
        && let Some(&last) = lead.found.last()
        && table.entry(last).also == Some(WordClass::Modifier)
        && lead.end < tokens.len()
    {
        out.push(notice(last, last.start, lead.end));
    }
    if let Some(&first) = trail.first()
        && first.start == e
        && e > 0
        && tokens[e - 1].possessive.is_none()
    {
        let before = &tokens[e - 1];
        let likely = match table.entry(first).also {
            Some(WordClass::Noun) => !table.names_person(&[before.stem()]),
            Some(WordClass::Modifier) => is_participle(&before.core),
            None => false,
        };
        if likely {
            out.push(notice(first, e - 1, first.end - 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::Lexicon;
    use crate::prompt::parse_prompt;

    /// The maybe-misread notices of `text`, as (phrase, words).
    fn misread(text: &str) -> Vec<(String, String)> {
        parse_prompt(text, &Lexicon::builtin())
            .warnings
            .into_iter()
            .filter_map(|w| match w {
                PromptWarning::MaybeMisread { phrase, words } => Some((phrase, words)),
                _ => None,
            })
            .collect()
    }

    /// One notice for `phrase` within `words`.
    fn one(phrase: &str, words: &str) -> Vec<(String, String)> {
        vec![(phrase.to_string(), words.to_string())]
    }

    #[test]
    fn everyday_readings_draw_a_notice() {
        assert_eq!(
            misread("close friend of the speaker"),
            one("close", "close friend")
        );
        assert_eq!(misread("full band"), one("full", "full band"));
        assert_eq!(misread("Full choir"), one("Full", "Full choir"));
        assert_eq!(misread("tight end"), one("tight", "tight end"));
        assert_eq!(misread("Ms Smith close-up"), one("Ms", "Ms Smith"));
        assert_eq!(misread("marble bust"), one("bust", "marble bust"));
        assert_eq!(misread("a rock face"), one("face", "rock face"));
        assert_eq!(
            misread("a man standing close"),
            one("close", "standing close")
        );
        assert_eq!(misread("marble busts"), one("busts", "marble busts"));
    }

    #[test]
    fn intended_shot_words_draw_none() {
        for text in [
            "close-up guitar",
            "close-up of the guitar",
            "tight on the drummer",
            "full of the band",
            "guitar close-up",
            "speaker face",
            "the singer's face",
            "guitarist close",
            "a person singing close-up",
            "drummer wide",
            "a face",
            "speaker upper body",
        ] {
            assert_eq!(misread(text), [], "{text}");
        }
    }
}
