//! Words of a prompt: tokens that remember where they were typed, and the
//! small closed classes of grammar words the parser steers by.
//!
//! # Theory
//!
//! Matching works on normalized words, but everything the parser hands back
//! (the subject, the phrases quoted in warnings) is sliced from the
//! operator's own text, so case, punctuation and spelling survive. Each
//! token therefore carries its byte span next to its normalized form.
//!
//! A word is a run of letters, digits and apostrophes. Anything else
//! separates words, which is how every kind of hyphen is tolerated: "close-up"
//! and "close up" are the same two words. A token also records its [`Joint`]
//! to the word before. A multi-word shot phrase may only span words joined
//! by spaces or dashes, so "medium, close" stays two shot words while
//! "medium close" is one; and a phrase may not be cut out of a hyphenated
//! compound, so "medium-sized guitar" holds no shot word.
//!
//! Typographic apostrophes (’ ‘ ʼ ＇) fold to `'`. A word ending in `'s`, or
//! in a bare apostrophe ("speakers'"), is a possessive; its stem and the byte
//! offset where the marker starts are kept so the parser can drop the marker
//! ("speaker's upper body" frames "speaker"). A word wrapped in apostrophes
//! ('close') is quoted, not possessive.
//!
//! # Invariants
//!
//! * Token spans are non-empty, lie on character boundaries of the text, and
//!   are in order without overlap.
//! * `core` is never empty and never starts or ends with an apostrophe.
//! * Tokenizing is total and linear in the length of the text.

/// Characters read as an apostrophe.
const APOSTROPHES: &[char] = &['\'', '\u{2018}', '\u{2019}', '\u{02BC}', '\u{FF07}'];

/// Characters that may sit between the words of one shot phrase: spaces,
/// underscores and every common hyphen or dash.
const GLUE: &[char] = &[
    '-', '_', '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
    '\u{FE63}', '\u{FF0D}',
];

/// Articles, demonstratives, possessive pronouns and quantifiers. One may
/// open a subject ("the pastor"); a shot word right after one is a noun
/// ("a face", "the bust"), not a shot size.
pub(crate) const DETERMINERS: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "my", "our", "your", "his", "her", "their",
    "its", "some", "any", "each", "every", "all",
];

/// Prepositions and relative pronouns. One ends the head of a noun phrase
/// ("guitarist *at* the back"), may open the tail after a possessive shot
/// ("speaker's upper body *at* the lectern"), and stops shot words being
/// read at the start ("close *to* the lectern"). The [`CONNECTORS`] "of"
/// and "on" act as link words too. "that" is also a determiner.
pub(crate) const LINK_WORDS: &[&str] = &[
    "at", "in", "inside", "behind", "near", "by", "beside", "next", "to", "from", "with",
    "without", "under", "over", "above", "below", "between", "against", "around", "who", "that",
    "which", "whose", "while",
];

/// Words that may join two shot phrases ("close and medium").
pub(crate) const JOINERS: &[&str] = &["and", "or", "plus"];

/// Words that may follow shot phrases at the start of a prompt
/// ("close-up *of* the guitar", "tight *on* the drummer").
pub(crate) const CONNECTORS: &[&str] = &["of", "on"];

/// Words that may follow any shot phrase without changing it ("medium
/// shot", "close shots").
pub(crate) const FILLERS: &[&str] = &["shot", "shots"];

/// True when `word` (a token core) is in `class`.
pub(crate) fn is_in(class: &[&str], word: &str) -> bool {
    class.contains(&word)
}

/// True when `key` is a grammar word of any class, which no lexicon entry
/// may claim.
pub(crate) fn is_grammar_word(key: &str) -> bool {
    [DETERMINERS, LINK_WORDS, JOINERS, CONNECTORS, FILLERS]
        .iter()
        .any(|class| is_in(class, key))
}

/// One word of a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    /// Byte offset of the word's first character in the text.
    pub(crate) start: usize,
    /// Byte offset just past the word's last character.
    pub(crate) end: usize,
    /// The word in lowercase, apostrophes folded to `'` and trimmed from
    /// both ends.
    pub(crate) core: String,
    /// For a possessive: the byte offset where its marker (`'s` or `'`)
    /// starts, and the stem before it.
    pub(crate) possessive: Option<(usize, String)>,
    /// How the word joins the one before.
    pub(crate) joint: Joint,
}

/// What separates a word from the word before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Joint {
    /// The first word, or punctuation other than spaces and dashes sits
    /// between ("medium, close").
    Break,
    /// Whitespace, perhaps with dashes ("close up", "close - up").
    Space,
    /// Dashes or underscores only ("close-up"): one compound word.
    Hyphen,
}

impl Token {
    /// True when a phrase may run on from the word before into this one.
    pub(crate) fn glued(&self) -> bool {
        self.joint != Joint::Break
    }

    /// The word without a possessive marker: "speaker" for "speaker's".
    pub(crate) fn stem(&self) -> &str {
        self.possessive.as_ref().map_or(&self.core, |(_, s)| s)
    }

    /// Reads the word at `start..end` of `text`, or `None` when it is only
    /// apostrophes.
    fn read(text: &str, start: usize, end: usize, joint: Joint) -> Option<Self> {
        let raw = &text[start..end];
        let core = fold(raw).trim_matches('\'').to_string();
        if core.is_empty() {
            return None;
        }
        Some(Self {
            start,
            end,
            possessive: possessive(raw).map(|(at, stem)| (start + at, stem)),
            core,
            joint,
        })
    }
}

/// True for characters that belong to a word.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || APOSTROPHES.contains(&c)
}

/// Lowercases `raw` and folds every apostrophe to `'`.
fn fold(raw: &str) -> String {
    raw.chars()
        .map(|c| if APOSTROPHES.contains(&c) { '\'' } else { c })
        .flat_map(char::to_lowercase)
        .collect()
}

/// The marker offset (relative to `raw`) and stem of a possessive word.
fn possessive(raw: &str) -> Option<(usize, String)> {
    let stem_of = |at: usize| {
        let stem = fold(&raw[..at]).trim_matches('\'').to_string();
        (!stem.is_empty()).then_some((at, stem))
    };
    let mut rev = raw.char_indices().rev();
    let (last_at, last) = rev.next()?;
    if matches!(last, 's' | 'S') {
        // "speaker's": an apostrophe right before a final s.
        return match rev.next() {
            Some((at, c)) if APOSTROPHES.contains(&c) => stem_of(at),
            _ => None,
        };
    }
    if !APOSTROPHES.contains(&last) || raw.starts_with(APOSTROPHES) {
        // No marker, or a quoted word ('close').
        return None;
    }
    // "speakers'": the marker starts at the first of the closing apostrophes.
    let mut at = last_at;
    for (i, c) in rev {
        if !APOSTROPHES.contains(&c) {
            break;
        }
        at = i;
    }
    stem_of(at)
}

/// Splits `text` into words; see the module docs.
pub(crate) fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut word_start = None;
    // What the gap since the last word held besides dashes.
    let (mut space, mut other) = (false, false);
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        let in_word = i < text.len() && is_word_char(c);
        match word_start {
            None if in_word => word_start = Some(i),
            Some(start) if !in_word => {
                word_start = None;
                let joint = if tokens.is_empty() || other {
                    Joint::Break
                } else if space {
                    Joint::Space
                } else {
                    Joint::Hyphen
                };
                match Token::read(text, start, i, joint) {
                    Some(token) => {
                        tokens.push(token);
                        (space, other) = (false, false);
                    }
                    // A run of apostrophes only is punctuation.
                    None => other = true,
                }
            }
            _ => {}
        }
        if !in_word {
            if c.is_whitespace() {
                space = true;
            } else if !GLUE.contains(&c) {
                other = true;
            }
        }
    }
    tokens
}

/// The words of a lexicon phrase, as token cores.
pub(crate) fn phrase_words(phrase: &str) -> Vec<String> {
    tokenize(phrase).into_iter().map(|t| t.core).collect()
}

/// The matching key of a phrase: its words run together, so "close-up",
/// "close up" and "closeup" share the key "closeup".
pub(crate) fn phrase_key(phrase: &str) -> String {
    phrase_words(phrase).concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cores of `text`'s tokens.
    fn cores(text: &str) -> Vec<String> {
        tokenize(text).into_iter().map(|t| t.core).collect()
    }

    #[test]
    fn hyphens_and_punctuation_separate_words() {
        assert_eq!(cores("Close-Up, guitar!"), ["close", "up", "guitar"]);
        assert_eq!(
            cores("close\u{2010}up close\u{2013}up"),
            ["close", "up", "close", "up"]
        );
        assert!(cores("  ?! -- ").is_empty());
    }

    #[test]
    fn spans_slice_the_original_text() {
        let text = "  Speaker\u{2019}s  UPPER-body ";
        let tokens = tokenize(text);
        let typed: Vec<&str> = tokens.iter().map(|t| &text[t.start..t.end]).collect();
        assert_eq!(typed, ["Speaker\u{2019}s", "UPPER", "body"]);
        assert_eq!(tokens[0].core, "speaker's");
    }

    #[test]
    fn joints_tell_spaces_dashes_and_punctuation_apart() {
        let joints: Vec<Joint> = tokenize("medium close, up/full - body close-up x_y '' z")
            .iter()
            .map(|t| t.joint)
            .collect();
        use Joint::{Break, Hyphen, Space};
        assert_eq!(
            joints,
            [
                Break, Space, Break, Break, Space, Space, Hyphen, Space, Hyphen, Break
            ]
        );
    }

    #[test]
    fn possessives_keep_stem_and_marker_offset() {
        for (text, stem, marker) in [
            ("speaker's", "speaker", 7),
            ("Speaker\u{2019}s", "speaker", 7),
            ("speakers'", "speakers", 8),
            ("speakers\u{2019}", "speakers", 8),
            ("James'", "james", 5),
        ] {
            let token = &tokenize(text)[0];
            assert_eq!(token.possessive, Some((marker, stem.to_string())), "{text}");
            assert_eq!(token.stem(), stem);
            assert_eq!(&text[..marker].to_lowercase(), stem);
        }
    }

    #[test]
    fn quoted_words_and_bare_markers_are_not_possessive() {
        for text in ["'close'", "o'clock", "speakers", "'s", "''"] {
            assert!(
                tokenize(text).iter().all(|t| t.possessive.is_none()),
                "{text}"
            );
        }
        assert_eq!(cores("'close'"), ["close"]);
        assert!(cores("''").is_empty());
    }

    #[test]
    fn keys_ignore_case_spaces_and_hyphens() {
        for phrase in ["close-up", "Close Up", "closeup", "CLOSE\u{2011}UP"] {
            assert_eq!(phrase_key(phrase), "closeup");
        }
        assert_eq!(phrase_key(" -- "), "");
    }

    #[test]
    fn non_ascii_words_survive() {
        assert_eq!(cores("Café Ünïcode 🎸 x"), ["café", "ünïcode", "x"]);
    }

    #[test]
    fn fillers_joiners_and_connectors_are_not_determiners_or_links() {
        for word in FILLERS.iter().chain(JOINERS).chain(CONNECTORS) {
            assert!(!is_in(DETERMINERS, word) && !is_in(LINK_WORDS, word));
        }
        assert!(is_grammar_word("the") && is_grammar_word("shot") && !is_grammar_word("close"));
    }
}
