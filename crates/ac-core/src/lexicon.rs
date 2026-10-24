//! The prompt parser's vocabulary, held as data: shot words, role words and
//! person words.
//!
//! # Theory
//!
//! An operator's prompt such as "speaker upper body" asks two questions at
//! once: *who* to frame ("speaker") and *what shot* to make ("upper body").
//! No detector answers both (SAM 3.1 found nothing for "speaker upper body",
//! and OWLv2 boxes objects, not shots; `/mnt/project-files/research/
//! model-picker-design.md` §1 and §4.2). [`crate::prompt`] splits the two,
//! and this module holds the words it splits by, so the mappings can be
//! reviewed, benchmarked and changed without touching the parser:
//!
//! * [`ShotWord`]s map phrases ("close-up", "upper body") to
//!   [`ShotSize`]s. The built-in table is design §4.2's; Noah decides the
//!   mappings. A shot word that is also an everyday word ([`WordClass`]:
//!   "full band", "a rock face") is marked so, and the parser tells the
//!   operator when it may have misread one.
//! * [`RoleWord`]s rewrite a role ("speaker") into the phrase a detector
//!   understands ("a person speaking"). Every entry is a hypothesis until it
//!   is benchmarked. "a person speaking" found the speaker on SAM 3.1 (10/20
//!   frames, 0 false, `sam31-vs-owlv2.md`); on OWLv2 (benchmark B0,
//!   people-first, described-person route at the 0.15 person threshold) it
//!   found the speaker in 20 of 20 frames with 300 false boxes on 40
//!   labelled frames: it picks everyone. "a person playing guitar" is
//!   measured on both models, "a person playing drums" on OWLv2 (20/20, 0
//!   false), "a person singing" on neither. To OWLv2 the bare word "speaker"
//!   most likely means a loudspeaker, which is why the rewrite matters.
//! * Each role entry says what its phrase needs the detector to see
//!   ([`RoleNeeds`], design §4.2): an *action* (speaking, singing) or an
//!   *object* the person plays (a guitar, drums). OWLv2 sees people and
//!   objects, not actions, so the parser adds a notice to every prompt
//!   rewritten to an action role, which the engine drops when the studio's
//!   detector sees actions (the sidecar's SAM 3.1). The tag keeps the data
//!   model-neutral: it says what a phrase needs, and the detector what it
//!   sees.
//! * Person words ("woman", "player") name people. A subject whose head
//!   noun is a person or role word is framed on the person basis.
//!
//! The parser's grammar words (articles, prepositions, "and", "shot") are
//! not data: they steer the parse and live in the parser.
//!
//! # Invariants
//!
//! * Phrases compare by key: lowercase, apostrophes folded, spaces and
//!   hyphens removed, so "close-up", "Close Up" and "closeup" are one
//!   phrase. A later entry with the same key replaces an earlier one, both
//!   in the parser and in [`Lexicon::set_shot`] and [`Lexicon::set_role`].
//! * [`Lexicon::validate`] states what a well-formed lexicon is. The parser
//!   never fails on a malformed one: it skips the entries `validate` rejects.
//! * Built-in data only; nothing here reads files. A configuration may build
//!   on [`Lexicon::builtin`] with the `set_*` methods.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::ShotSize;
use crate::prompt::words::{is_grammar_word, phrase_key};

/// Where in a prompt a shot word counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// At the start or the end of the prompt, or right after a possessive.
    #[default]
    Anywhere,
    /// Only after the subject ("drummer wide"): before a noun the word reads
    /// as an adjective ("wide stage").
    AfterSubject,
}

/// The everyday reading a shot word also has, which the parser may have
/// taken for a shot size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordClass {
    /// Also an adjective or a title before a noun ("full band", "close
    /// friend", "tight end", "Ms Smith"), or an adverb after a verb ("a man
    /// standing close").
    Modifier,
    /// Also a noun after another noun ("a rock face", "marble bust").
    Noun,
}

/// A phrase that names shot sizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShotWord {
    /// The phrase, such as "close-up".
    pub phrase: String,
    /// The sizes it asks for, in order; never empty and never
    /// [`ShotSize::Wide`] (a source's wide shot is not a subject's shot).
    pub sizes: Vec<ShotSize>,
    /// Where the phrase counts.
    #[serde(default)]
    pub placement: Placement,
    /// A caveat shown to the operator whenever the phrase is used.
    #[serde(default)]
    pub note: Option<String>,
    /// The everyday reading the phrase also has, if any; the parser adds a
    /// notice where that reading is likely (`PromptWarning::MaybeMisread`).
    #[serde(default)]
    pub also: Option<WordClass>,
}

/// What a role's detector phrase needs the detector to see besides a
/// person (design §4.2), so the operator can be told when it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleNeeds {
    /// What the person is doing ("a person speaking", "a person singing").
    /// A detector that sees only boxes, as OWLv2 does, cannot tell it, so
    /// the parser adds a notice (`PromptWarning::ActionRole`).
    Action,
    /// An object the person plays or holds ("a person playing guitar"):
    /// people-first matching asks for the object and picks its holder.
    Object,
}

/// A role and the phrase a detector is asked for instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleWord {
    /// The role, such as "speaker". Its plural ("speakers") matches too.
    pub word: String,
    /// The detector phrase, such as "a person speaking".
    pub phrase: String,
    /// What the phrase needs the detector to see; `None` when untagged,
    /// which draws no notice. An entry that replaces a built-in role
    /// replaces its tag too.
    #[serde(default)]
    pub needs: Option<RoleNeeds>,
}

/// The parser's vocabulary; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lexicon {
    /// Shot words; later entries replace earlier ones with the same key.
    #[serde(default)]
    pub shots: Vec<ShotWord>,
    /// Role words; later entries replace earlier ones with the same key.
    #[serde(default)]
    pub roles: Vec<RoleWord>,
    /// Nouns that name a person ("woman", "player"); plurals formed with
    /// "s" or "es" match too, irregular ones are listed.
    #[serde(default)]
    pub person_words: Vec<String>,
}

/// A malformed lexicon entry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LexiconError {
    /// A shot word or role word has no letters or digits.
    #[error("lexicon entry {0:?} has no words")]
    NoWords(String),
    /// A shot word lists no sizes, or lists the source-wide size.
    #[error("shot word {0:?} must name at least one subject shot size (not wide)")]
    BadSizes(String),
    /// An entry claims a word the parser steers by ("the", "of", "shot").
    #[error("lexicon entry {0:?} is a grammar word the parser reserves")]
    GrammarWord(String),
    /// A role word rewrites to an empty detector phrase.
    #[error("role word {0:?} has an empty detector phrase")]
    EmptyPhrase(String),
}

/// Built-in shot words: design §4.2's table, with the everyday reading of
/// each word that has one. Cut lines and head-height multiples behind each
/// size are estimates (see `ShotSize`); the word lists are proposals until
/// Noah confirms them. "medium close-up" is listed so that, with hyphens
/// optional, the longest match beats "close-up" in "speaker medium
/// close-up". Plurals ("close-ups", "headshots") match without being listed.
const SHOT_WORDS: &[(&str, &[ShotSize], Option<WordClass>)] = {
    use ShotSize::{Close, Full, Medium, MediumClose};
    use WordClass::{Modifier, Noun};
    &[
        ("close", &[Close], Some(Modifier)),
        ("close-up", &[Close], None),
        ("cu", &[Close], None),
        ("head and shoulders", &[Close], None),
        ("headshot", &[Close], Some(Noun)),
        ("tight", &[Close], Some(Modifier)),
        ("face", &[Close], Some(Noun)),
        ("medium close", &[MediumClose], None),
        ("medium close-up", &[MediumClose], None),
        ("mcu", &[MediumClose], None),
        ("chest up", &[MediumClose], None),
        ("bust", &[MediumClose], Some(Noun)),
        ("medium", &[Medium], Some(Modifier)),
        ("mid shot", &[Medium], None),
        ("ms", &[Medium], Some(Modifier)),
        ("waist up", &[Medium], None),
        // Waist-up and chest-up both, until Noah picks one (design §8 item 6).
        ("upper body", &[Medium, MediumClose], None),
        ("half body", &[Medium, MediumClose], None),
        ("full", &[Full], Some(Modifier)),
        ("full body", &[Full], None),
        ("full length", &[Full], None),
        ("head to toe", &[Full], None),
        ("long shot", &[Full], None),
        ("fs", &[Full], None),
    ]
};

/// "wide" after a subject ("drummer wide"): today a full shot of the
/// subject alone. Design §4.2 asks for the subject together with its
/// attached objects (spec V4, "drum kit with drummer's head"), which needs
/// attachment boxes and is later work. The camera's own wide shot stays the
/// source's Wide shot.
const WIDE: &str = "wide";

/// The caveat shown whenever "wide" is used.
const WIDE_NOTE: &str = "frames the full body for now; framing the subject together with its \
                         instrument or kit is later work";

/// Built-in roles: design §4.2's role lexicon, with what each phrase needs.
/// Every entry is a hypothesis until benchmarked on the rehearsal footage.
/// The guitarist's and drummer's phrases name an instrument, which
/// people-first matching asks for as an object; the others describe an
/// action.
const ROLE_WORDS: &[(&str, &str, RoleNeeds)] = {
    use RoleNeeds::{Action, Object};
    &[
        ("speaker", "a person speaking", Action),
        ("pastor", "a person speaking", Action),
        ("preacher", "a person speaking", Action),
        ("guitarist", "a person playing guitar", Object),
        ("drummer", "a person playing drums", Object),
        ("singer", "a person singing", Action),
    ]
};

/// Built-in person words: the people-first matcher's person words
/// (`ac_vision::matching`) and common names for band members.
const PERSON_WORDS: &[&str] = &[
    "person",
    "people",
    "man",
    "men",
    "woman",
    "women",
    "someone",
    "somebody",
    "guy",
    "girl",
    "boy",
    "kid",
    "child",
    "children",
    "human",
    "lady",
    "ladies",
    "individual",
    "player",
    "musician",
    "performer",
    "vocalist",
    "bassist",
    "pianist",
    "keyboardist",
    "leader",
];

impl Lexicon {
    /// The built-in vocabulary; see the module docs.
    pub fn builtin() -> Self {
        let mut lexicon = Self {
            shots: SHOT_WORDS
                .iter()
                .map(|(phrase, sizes, also)| ShotWord {
                    phrase: (*phrase).into(),
                    sizes: sizes.to_vec(),
                    placement: Placement::Anywhere,
                    note: None,
                    also: *also,
                })
                .collect(),
            roles: ROLE_WORDS
                .iter()
                .map(|&(word, phrase, needs)| RoleWord {
                    word: word.into(),
                    phrase: phrase.into(),
                    needs: Some(needs),
                })
                .collect(),
            person_words: PERSON_WORDS.iter().map(|w| (*w).into()).collect(),
        };
        lexicon.shots.push(ShotWord {
            phrase: WIDE.into(),
            sizes: vec![ShotSize::Full],
            placement: Placement::AfterSubject,
            note: Some(WIDE_NOTE.into()),
            also: None,
        });
        lexicon
    }

    /// Adds `word`, replacing any shot word with the same key.
    pub fn set_shot(&mut self, word: ShotWord) {
        let key = phrase_key(&word.phrase);
        self.shots.retain(|w| phrase_key(&w.phrase) != key);
        self.shots.push(word);
    }

    /// Adds `role`, replacing any role word with the same key.
    pub fn set_role(&mut self, role: RoleWord) {
        let key = phrase_key(&role.word);
        self.roles.retain(|r| phrase_key(&r.word) != key);
        self.roles.push(role);
    }

    /// Adds a person word unless it is already listed.
    pub fn add_person_word(&mut self, word: &str) {
        let key = phrase_key(word);
        if !self.person_words.iter().any(|w| phrase_key(w) == key) {
            self.person_words.push(word.into());
        }
    }

    /// Checks every entry; the first problem found is returned.
    pub fn validate(&self) -> Result<(), LexiconError> {
        for shot in &self.shots {
            check_entry(&shot.phrase)?;
            if shot.sizes.is_empty() || shot.sizes.contains(&ShotSize::Wide) {
                return Err(LexiconError::BadSizes(shot.phrase.clone()));
            }
        }
        for role in &self.roles {
            check_entry(&role.word)?;
            if role.phrase.trim().is_empty() {
                return Err(LexiconError::EmptyPhrase(role.word.clone()));
            }
        }
        self.person_words.iter().try_for_each(|w| check_entry(w))
    }
}

/// Checks that an entry has words and claims no grammar word.
fn check_entry(phrase: &str) -> Result<(), LexiconError> {
    let key = phrase_key(phrase);
    if key.is_empty() {
        Err(LexiconError::NoWords(phrase.into()))
    } else if is_grammar_word(&key) {
        Err(LexiconError::GrammarWord(phrase.into()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShotSize::{Close, Full, Medium, MediumClose};

    /// The shot word with `phrase`'s key.
    fn shot<'a>(lexicon: &'a Lexicon, phrase: &str) -> Option<&'a ShotWord> {
        let key = phrase_key(phrase);
        lexicon.shots.iter().find(|w| phrase_key(&w.phrase) == key)
    }

    #[test]
    fn builtin_is_valid() {
        assert_eq!(Lexicon::builtin().validate(), Ok(()));
        assert_eq!(Lexicon::default().validate(), Ok(()));
    }

    #[test]
    fn builtin_holds_the_design_table() {
        let lexicon = Lexicon::builtin();
        let rows: &[(&[&str], &[ShotSize])] = &[
            (
                &[
                    "close",
                    "close-up",
                    "cu",
                    "head and shoulders",
                    "headshot",
                    "tight",
                    "face",
                ],
                &[Close],
            ),
            (&["medium close", "mcu", "chest up", "bust"], &[MediumClose]),
            (&["medium", "mid shot", "ms", "waist up"], &[Medium]),
            (&["upper body", "half body"], &[Medium, MediumClose]),
            (
                &[
                    "full",
                    "full body",
                    "full length",
                    "head to toe",
                    "long shot",
                    "fs",
                ],
                &[Full],
            ),
        ];
        for (phrases, sizes) in rows {
            for phrase in *phrases {
                let word = shot(&lexicon, phrase).unwrap_or_else(|| panic!("{phrase}"));
                assert_eq!(word.sizes, *sizes, "{phrase}");
                assert_eq!(word.placement, Placement::Anywhere, "{phrase}");
            }
        }
        let also = |phrase: &str| shot(&lexicon, phrase).and_then(|w| w.also);
        for phrase in ["close", "tight", "full", "medium", "ms"] {
            assert_eq!(also(phrase), Some(WordClass::Modifier), "{phrase}");
        }
        for phrase in ["face", "bust", "headshot"] {
            assert_eq!(also(phrase), Some(WordClass::Noun), "{phrase}");
        }
        assert_eq!(also("close-up"), None);
        let wide = shot(&lexicon, "wide").expect("wide");
        assert_eq!(wide.sizes, [Full]);
        assert_eq!(wide.placement, Placement::AfterSubject);
        assert!(wide.note.as_deref().is_some_and(|n| n.contains("later")));
    }

    #[test]
    fn builtin_holds_the_role_lexicon() {
        let lexicon = Lexicon::builtin();
        let phrase = |word: &str| {
            lexicon
                .roles
                .iter()
                .find(|r| r.word == word)
                .map(|r| r.phrase.as_str())
        };
        for word in ["speaker", "pastor", "preacher"] {
            assert_eq!(phrase(word), Some("a person speaking"));
        }
        assert_eq!(phrase("guitarist"), Some("a person playing guitar"));
        assert_eq!(phrase("drummer"), Some("a person playing drums"));
        assert_eq!(phrase("singer"), Some("a person singing"));
        assert_eq!(phrase("bassist"), None);
    }

    #[test]
    fn builtin_roles_say_what_their_phrases_need() {
        let lexicon = Lexicon::builtin();
        let needs = |word: &str| {
            lexicon
                .roles
                .iter()
                .find(|r| r.word == word)
                .map(|r| r.needs)
        };
        for word in ["speaker", "pastor", "preacher", "singer"] {
            assert_eq!(needs(word), Some(Some(RoleNeeds::Action)), "{word}");
        }
        for word in ["guitarist", "drummer"] {
            assert_eq!(needs(word), Some(Some(RoleNeeds::Object)), "{word}");
        }
        assert!(lexicon.roles.iter().all(|r| r.needs.is_some()));
    }

    #[test]
    fn set_shot_replaces_by_key() {
        let mut lexicon = Lexicon::builtin();
        let before = lexicon.shots.len();
        lexicon.set_shot(ShotWord {
            phrase: "Close Up".into(),
            sizes: vec![MediumClose],
            placement: Placement::Anywhere,
            note: None,
            also: None,
        });
        assert_eq!(lexicon.shots.len(), before);
        assert_eq!(
            shot(&lexicon, "close-up").map(|w| &w.sizes[..]),
            Some(&[MediumClose][..])
        );
        lexicon.set_shot(ShotWord {
            phrase: "cowboy".into(),
            sizes: vec![Medium],
            placement: Placement::Anywhere,
            note: None,
            also: None,
        });
        assert_eq!(lexicon.shots.len(), before + 1);
    }

    #[test]
    fn set_role_and_person_words_replace_by_key() {
        let mut lexicon = Lexicon::builtin();
        let roles = lexicon.roles.len();
        lexicon.set_role(RoleWord {
            word: "Speaker".into(),
            phrase: "a person at a lectern".into(),
            needs: None,
        });
        assert_eq!(lexicon.roles.len(), roles);
        // The whole entry is replaced, its tag too.
        assert_eq!(
            lexicon.roles.last().map(|r| (r.phrase.as_str(), r.needs)),
            Some(("a person at a lectern", None))
        );
        let people = lexicon.person_words.len();
        lexicon.add_person_word("WOMAN");
        assert_eq!(lexicon.person_words.len(), people);
        lexicon.add_person_word("organist");
        assert_eq!(lexicon.person_words.len(), people + 1);
    }

    #[test]
    fn validate_rejects_malformed_entries() {
        let word = |phrase: &str, sizes: &[ShotSize]| ShotWord {
            phrase: phrase.into(),
            sizes: sizes.to_vec(),
            placement: Placement::Anywhere,
            note: None,
            also: None,
        };
        let with_shot = |w: ShotWord| Lexicon {
            shots: vec![w],
            ..Lexicon::default()
        };
        assert_eq!(
            with_shot(word(" - ", &[Close])).validate(),
            Err(LexiconError::NoWords(" - ".into()))
        );
        assert_eq!(
            with_shot(word("snug", &[])).validate(),
            Err(LexiconError::BadSizes("snug".into()))
        );
        assert_eq!(
            with_shot(word("snug", &[ShotSize::Wide])).validate(),
            Err(LexiconError::BadSizes("snug".into()))
        );
        assert_eq!(
            with_shot(word("Shot", &[Close])).validate(),
            Err(LexiconError::GrammarWord("Shot".into()))
        );
        let with_role = |word: &str, phrase: &str| Lexicon {
            roles: vec![RoleWord {
                word: word.into(),
                phrase: phrase.into(),
                needs: None,
            }],
            ..Lexicon::default()
        };
        assert_eq!(
            with_role("host", "  ").validate(),
            Err(LexiconError::EmptyPhrase("host".into()))
        );
        assert_eq!(
            with_role("the", "a person").validate(),
            Err(LexiconError::GrammarWord("the".into()))
        );
        let people = Lexicon {
            person_words: vec!["".into()],
            ..Lexicon::default()
        };
        assert_eq!(people.validate(), Err(LexiconError::NoWords("".into())));
    }

    #[test]
    fn errors_read_as_sentences() {
        let text = LexiconError::BadSizes("snug".into()).to_string();
        assert!(text.contains("snug") && text.contains("not wide"));
    }
}
