//! The head noun of a subject: whether it names a person, and whether the
//! whole subject is a role.
//!
//! # Theory
//!
//! In an English noun phrase the head noun, the word the phrase is about,
//! is the last word of the first chunk: "bass *guitarist* on the left",
//! "the speaker's *microphone*", "a *person* playing guitar". The chunk
//! ends at the first preposition, relative pronoun or joiner, or at an
//! "-ing" participle that follows a noun ("a person *playing*"; after an
//! article the "-ing" word is a noun or an adjective, as in "a painting" or
//! "a singing woman"). A possessive is never the head, so a participle cut
//! right after one moves on a word ("the speaker's *clothing*").
//!
//! This is a heuristic, not a grammar: it reads ordinary camera prompts
//! right and fails safe, because a wrong basis only changes how a shot size
//! is measured, never what the detector is asked for.
//!
//! A role is rewritten only when it is the whole subject after any
//! articles ("the pastor", "all the guitarists"); see the parser docs.
//!
//! # Invariants
//!
//! * Pure and total over any tokens, including none.
//! * The first word always belongs to the chunk.

use super::Basis;
use super::scan::Table;
use super::words::{CONNECTORS, DETERMINERS, JOINERS, LINK_WORDS, Token, is_in};

/// The detector phrase when `words` are exactly a role word after any
/// articles.
pub(super) fn role_of<'a>(table: &Table<'a>, words: &[Token]) -> Option<&'a str> {
    let skip = words
        .iter()
        .take(words.len().saturating_sub(1))
        .take_while(|t| is_in(DETERMINERS, &t.core))
        .count();
    let cores: Vec<&str> = words[skip..].iter().map(|t| t.core.as_str()).collect();
    table.role_phrase(&cores)
}

/// The basis from the head noun of `words`; see the module docs.
pub(super) fn basis_of(table: &Table<'_>, words: &[Token]) -> Basis {
    let mut end = words.len();
    for (i, t) in words.iter().enumerate().skip(1) {
        if is_in(LINK_WORDS, &t.core) || is_in(JOINERS, &t.core) || is_in(CONNECTORS, &t.core) {
            end = i;
            break;
        }
        if is_participle(&t.core) && !is_in(DETERMINERS, &words[i - 1].core) {
            end = if words[i - 1].possessive.is_some() {
                i + 1
            } else {
                i
            };
            break;
        }
    }
    let stems: Vec<&str> = words[..end].iter().map(Token::stem).collect();
    if table.names_person(&stems) {
        Basis::Person
    } else {
        Basis::Object
    }
}

/// True for an "-ing" word long enough to be a participle ("playing", but
/// not "ring" or "thing").
fn is_participle(word: &str) -> bool {
    word.strip_suffix("ing")
        .is_some_and(|stem| stem.chars().count() >= 3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::Lexicon;
    use crate::prompt::words::tokenize;

    /// The basis of `text` with the built-in lexicon.
    fn basis(text: &str) -> Basis {
        let lexicon = Lexicon::builtin();
        basis_of(&Table::new(&lexicon), &tokenize(text))
    }

    #[test]
    fn participles_end_the_chunk_only_after_a_noun() {
        assert!(is_participle("playing") && is_participle("string"));
        assert!(!is_participle("ring") && !is_participle("thing") && !is_participle("ing"));
        assert_eq!(basis("a person playing guitar"), Basis::Person);
        assert_eq!(basis("a singing woman"), Basis::Person);
        assert_eq!(basis("a painting of a man"), Basis::Object);
    }

    #[test]
    fn a_possessive_is_never_the_head() {
        assert_eq!(basis("the speaker's clothing"), Basis::Object);
        assert_eq!(basis("the speaker's microphone"), Basis::Object);
        assert_eq!(basis("the speaker's"), Basis::Person);
    }

    #[test]
    fn no_words_is_an_object_and_no_role() {
        let lexicon = Lexicon::builtin();
        let table = Table::new(&lexicon);
        assert_eq!(basis_of(&table, &[]), Basis::Object);
        assert_eq!(role_of(&table, &[]), None);
        assert_eq!(role_of(&table, &tokenize("the")), None);
        assert_eq!(
            role_of(&table, &tokenize("the the singer")),
            Some("a person singing")
        );
    }
}
