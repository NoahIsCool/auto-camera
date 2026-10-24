//! Prompt parsing: the operator's prompt is not the model's prompt.
//!
//! # Theory
//!
//! Noah chose free-text prompts so that a prompt can say both who to frame
//! and what shot to make: "speaker upper body". No detector answers both at
//! once. On the rehearsal footage SAM 3.1 found nothing for "speaker upper
//! body" but found the speaker for "a person speaking", and OWLv2 boxes
//! objects, never shots (`/mnt/project-files/research/model-picker-design.md`
//! §1, §2). [`parse_prompt`] therefore splits a prompt into
//!
//! * a **subject**, what to look for ("speaker"), and the **model phrase**
//!   the detector receives in its place ("a person speaking", from the
//!   [`Lexicon`]'s role words);
//! * **shots**, the [`ShotSize`]s its shot words ask for ("upper body" asks
//!   for Medium and Medium close), which framing then produces from the
//!   subject's geometry, so shot words work on any subject;
//! * a **basis**, whether the subject is a person or an object, which
//!   decides how a shot size is measured.
//!
//! The words themselves are data ([`crate::lexicon`]); this module is the
//! grammar that applies them.
//!
//! # Grammar
//!
//! Shot words count in three places, and only there:
//!
//! * at the **end**: "speaker close-up", "drummer wide", "guitar close and
//!   medium shot";
//! * at the **start**: "close-up guitar", "a close-up of the guitar", "tight
//!   on the drummer". After an article the run must be followed by "of" or
//!   "on" ("the full band" is a band, not a full shot), and it must not be
//!   followed by a preposition ("close to the lectern"). "wide" never counts
//!   here: before a noun it is an adjective
//!   ([`crate::lexicon::Placement::AfterSubject`]);
//! * right **after a possessive**: "speaker's upper body" at the end, or
//!   "speaker's upper body at the lectern" when a preposition follows. The
//!   possessive marker is dropped from the subject.
//!
//! Matching ignores case, folds typographic apostrophes, treats spaces and
//! hyphens inside a phrase as optional ("close-up", "close up", "closeup")
//! and accepts "s" plurals ("close-ups", "headshots"). The longest phrase
//! wins ("medium close" is one phrase, not "medium" and "close"). Several
//! phrases may be listed, separated by nothing, punctuation, "and", "or" or
//! "plus", and each may be followed by "shot" or "shots". Sizes are kept in
//! the order typed, without repeats. Separators, brackets and quotes left
//! where shot words were cut off are trimmed from the subject
//! ([`trim`]), so `guitar "close-up"` sends "guitar".
//!
//! A shot word right after an article or a preposition is a noun: "a face"
//! looks for a face, "close-up of the face" is a close-up of a face. Such
//! words stay in the subject with a [`PromptWarning::ReadAsSubject`] notice.
//! Shot words anywhere else are sent to the detector as typed, with a
//! [`PromptWarning::InSubject`] notice, or [`PromptWarning::AfterSubjectOnly`]
//! for words such as "wide" that count only after the subject.
//!
//! Some shot words are everyday words too ([`crate::WordClass`]). Where the
//! everyday reading is likely, the shot size is still read, with a
//! [`PromptWarning::MaybeMisread`] notice pointing at literal mode
//! ([`notices`]): a modifier ("full", "close", "tight", "medium", "Ms") at
//! the start right before the subject ("full band", "close friend", "Ms
//! Smith"), a noun ("face", "bust") at the end right after a word that is
//! not a person ("a rock face", "marble bust"), or a modifier at the end
//! right after an "-ing" word ("a man standing close").
//!
//! **Roles** are rewritten only when the role word is the whole subject,
//! after any articles ("the pastor", "guitarists"): the measured phrases are
//! whole phrases, and splicing them into longer text is untested. "bass
//! guitarist" is sent as typed. A role whose phrase names an action
//! ([`crate::RoleNeeds::Action`]: speaker, pastor, preacher, singer) adds a
//! [`PromptWarning::ActionRole`] notice, because OWLv2 sees people and
//! objects, not actions, and gives such a query every person. The notice is
//! the same whatever the detector until detectors declare what they can see.
//!
//! **Basis**: [`Basis::Person`] when the subject's head noun is a person or
//! role word, else [`Basis::Object`]. The head noun is the last word before
//! the first preposition, joiner or participle ("a person *playing* guitar"
//! is a person; "the speaker's microphone" is a microphone; "drum kit" is a
//! kit).
//!
//! **Invalid prompts**: a prompt with no words is [`PromptWarning::Empty`];
//! one with shot words but nothing to frame ("close-up", "upper body",
//! "wide") is [`PromptWarning::NoSubject`]. Both are error-level warnings,
//! not `Err`s, so a live parse line can still show what was read; callers
//! reject such a prompt through [`ParsedPrompt::error`].
//!
//! **Literal**: [`literal_prompt`] bypasses all of this and sends the text
//! as typed; it still reports the basis, which changes framing only.
//!
//! # Invariants
//!
//! * Pure and total: no I/O, no clock, no panics, for any text and any
//!   lexicon; time and memory grow linearly with the prompt.
//! * A prompt without shot words or a whole role word parses to its own
//!   trimmed text, so prompts written before shot words keep their detector
//!   phrase.
//! * The subject is sliced from the typed text, keeping case and inner
//!   punctuation; separators and stray wrappers are trimmed only where shot
//!   words were cut.
//! * `model_phrase` is empty exactly when the parse has an error.
//! * `shots` never holds [`ShotSize::Wide`] nor a size twice.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::geometry::Extent;
use crate::lexicon::Lexicon;
use crate::model::ShotSize;

mod head;
mod notices;
mod scan;
mod trim;
pub(crate) mod words;

use head::{basis_of, role_of};
use notices::{action_role, misreads, shot_words_in};
use scan::{Match, Table};
use trim::clean;
use words::{CONNECTORS, DETERMINERS, JOINERS, LINK_WORDS, Token, is_in, phrase_key, tokenize};

/// How shot sizes are measured on a subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// A person: sizes are cut at body lines measured from the head.
    Person,
    /// Anything else: sizes scale the subject's box.
    Object,
}

impl Basis {
    /// Operator-facing name: "person" or "object".
    pub fn label(self) -> &'static str {
        match self {
            Basis::Person => "person",
            Basis::Object => "object",
        }
    }

    /// What a box the detector marked `detected` is framed as on this
    /// basis: a whole person only when the basis is a person too, else an
    /// object (the fill rule). See [`crate::model`], "Basis and extent".
    pub fn framing_extent(self, detected: Extent) -> Extent {
        match self {
            Basis::Person => detected,
            Basis::Object => Extent::Object,
        }
    }
}

/// Something the operator should know about a parse. [`Self::is_error`]
/// tells the two that make a prompt unusable from mere notices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptWarning {
    /// Error: the prompt has no letters or digits.
    Empty,
    /// Error: the prompt has shot words but nothing to frame.
    NoSubject {
        /// The shot phrases, as typed.
        phrases: Vec<String>,
    },
    /// Shot words after an article or preposition were read as part of the
    /// subject ("a face").
    ReadAsSubject {
        /// The phrases, as typed.
        phrases: Vec<String>,
        /// The word before them, as typed.
        after: String,
    },
    /// Shot words where they do not count (mid-prompt, or after a leading
    /// article without "of") go to the detector as typed.
    InSubject {
        /// The phrases, as typed.
        phrases: Vec<String>,
    },
    /// Shot words that count only after the subject ("drummer wide") were
    /// found elsewhere and go to the detector as typed.
    AfterSubjectOnly {
        /// The phrases, as typed.
        phrases: Vec<String>,
    },
    /// A shot word with an everyday reading ([`crate::WordClass`]) was
    /// read as a shot size where that reading is likely: "full band",
    /// "a rock face", "a man standing close".
    MaybeMisread {
        /// The shot phrase, as typed.
        phrase: String,
        /// The phrase with the word it may belong to, as typed.
        words: String,
    },
    /// The subject is a role whose detector phrase names an action
    /// ([`crate::RoleNeeds::Action`]), which OWLv2 cannot see.
    ActionRole {
        /// The role, as the lexicon spells it ("speaker").
        role: String,
        /// The detector phrase it asks for ("a person speaking").
        phrase: String,
    },
    /// A caveat the lexicon attaches to a shot word.
    Note {
        /// The phrase, as typed.
        phrase: String,
        /// The caveat.
        note: String,
    },
}

impl PromptWarning {
    /// True for warnings that make the prompt unusable.
    pub fn is_error(&self) -> bool {
        matches!(self, PromptWarning::Empty | PromptWarning::NoSubject { .. })
    }
}

/// `"a", "b"`.
fn quoted(phrases: &[String]) -> String {
    let quoted: Vec<String> = phrases.iter().map(|p| format!("\"{p}\"")).collect();
    quoted.join(", ")
}

impl fmt::Display for PromptWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PromptWarning::Empty => write!(
                f,
                "the prompt is empty: type who or what to look for, such as \"speaker\" or \
                 \"guitar\""
            ),
            PromptWarning::NoSubject { phrases } => {
                let first = phrases.first().map_or("close-up", String::as_str);
                write!(
                    f,
                    "{} only names a shot size: add who or what to frame, such as \"speaker \
                     {first}\"; to look for \"{first}\" itself, write \"the {first}\" or send the \
                     prompt as typed (literal)",
                    quoted(phrases),
                )
            }
            PromptWarning::ReadAsSubject { phrases, after } => write!(
                f,
                "{} after \"{after}\" is read as part of what to look for, not as a shot size",
                quoted(phrases)
            ),
            PromptWarning::InSubject { phrases } => write!(
                f,
                "{} is not read as a shot size here and goes to the detector as typed; shot \
                 words count at the start or end of the prompt, or right after a possessive \
                 such as \"speaker's\"",
                quoted(phrases)
            ),
            PromptWarning::AfterSubjectOnly { phrases } => write!(
                f,
                "{} is not read as a shot size here and goes to the detector as typed; it \
                 counts only at the end of the prompt or right after a possessive, as in \
                 \"drummer {}\"",
                quoted(phrases),
                phrases.first().map_or("wide", String::as_str)
            ),
            PromptWarning::MaybeMisread { phrase, words } => write!(
                f,
                "\"{phrase}\" is read as a shot size here; if \"{words}\" is what to look for, \
                 send the prompt as typed (literal)"
            ),
            PromptWarning::ActionRole { role, phrase } => write!(
                f,
                "\"{role}\" asks the detector for \"{phrase}\", an action: OWLv2 sees people \
                 and objects, not actions, so every person (photos on the screen included) gets \
                 this query's shots; take the {role}'s shot by hand"
            ),
            PromptWarning::Note { phrase, note } => write!(f, "\"{phrase}\" {note}"),
        }
    }
}

/// What a prompt asks for; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedPrompt {
    /// What to look for, as typed, without its shot words ("speaker").
    pub subject: String,
    /// What the detector receives ("a person speaking"); empty when the
    /// parse has an error.
    pub model_phrase: String,
    /// Shot sizes the prompt's shot words ask for, in typed order; empty
    /// when it names none (the query's presets decide).
    pub shots: Vec<ShotSize>,
    /// How shot sizes are measured on the subject.
    pub basis: Basis,
    /// The text was sent as typed, without parsing.
    pub literal: bool,
    /// Errors and notices, in the order found.
    pub warnings: Vec<PromptWarning>,
}

impl ParsedPrompt {
    /// A parse that failed with `error`, keeping the shots it read.
    fn failed(error: PromptWarning, shots: Vec<ShotSize>, literal: bool) -> Self {
        Self {
            subject: String::new(),
            model_phrase: String::new(),
            shots,
            basis: Basis::Object,
            literal,
            warnings: vec![error],
        }
    }

    /// The first error-level warning, if any; such a prompt must not be used.
    pub fn error(&self) -> Option<&PromptWarning> {
        self.warnings.iter().find(|w| w.is_error())
    }

    /// True when the prompt can be used.
    pub fn is_valid(&self) -> bool {
        self.error().is_none()
    }

    /// One line for the operator, such as `Detector sees: a person speaking ·
    /// Framing: Medium, Medium close (person)`, or the error.
    pub fn summary(&self) -> String {
        if let Some(error) = self.error() {
            return error.to_string();
        }
        let framing = if self.shots.is_empty() {
            "no shot words".to_string()
        } else {
            let labels: Vec<&str> = self.shots.iter().map(|s| size_label(*s)).collect();
            labels.join(", ")
        };
        let literal = if self.literal { " (literal)" } else { "" };
        format!(
            "Detector sees: {}{literal} · Framing: {framing} ({})",
            self.model_phrase,
            self.basis.label()
        )
    }
}

/// Operator-facing name of a shot size, as in the query form.
fn size_label(size: ShotSize) -> &'static str {
    match size {
        ShotSize::Wide => "Wide",
        ShotSize::Full => "Full",
        ShotSize::Medium => "Medium",
        ShotSize::MediumClose => "Medium close",
        ShotSize::Close => "Close",
    }
}

/// Sends `text` to the detector as typed (trimmed), without shot or role
/// words; the basis is still read from its head noun.
pub fn literal_prompt(text: &str, lexicon: &Lexicon) -> ParsedPrompt {
    let tokens = tokenize(text);
    if tokens.is_empty() {
        return ParsedPrompt::failed(PromptWarning::Empty, Vec::new(), true);
    }
    let table = Table::new(lexicon);
    ParsedPrompt {
        subject: text.trim().to_string(),
        model_phrase: text.trim().to_string(),
        shots: Vec::new(),
        basis: basis_of(&table, &tokens),
        literal: true,
        warnings: Vec::new(),
    }
}

/// Splits `text` into subject, model phrase, shots and basis; see the
/// module docs.
pub fn parse_prompt(text: &str, lexicon: &Lexicon) -> ParsedPrompt {
    let tokens = tokenize(text);
    if tokens.is_empty() {
        return ParsedPrompt::failed(PromptWarning::Empty, Vec::new(), false);
    }
    let table = Table::new(lexicon);
    let n = tokens.len();
    let mut warnings = Vec::new();

    let lead = leading(&table, &tokens);
    let lo = lead.end;
    let (mut e, mut trail) = table.run_backward(&tokens, lo, n);
    if !trail.is_empty() {
        // A shot word after an article or preposition is a noun ("a face").
        let after = if e == lo {
            lead.connector
        } else {
            let last = &tokens[e - 1];
            (last.possessive.is_none() && is_steering(&last.core)).then_some(e - 1)
        };
        if let Some(after) = after {
            warnings.push(PromptWarning::ReadAsSubject {
                phrases: typed_phrases(text, &tokens, &trail),
                after: text[tokens[after].start..tokens[after].end].to_string(),
            });
            (e, trail) = (n, Vec::new());
        }
    }
    let has_shots = !lead.found.is_empty() || !trail.is_empty();
    if has_shots && tokens[lo..e].iter().all(|t| is_steering(&t.core)) {
        // Nothing, or only grammar words ("close-up of the"), to frame.
        let found: Vec<Match> = lead.found.iter().chain(&trail).copied().collect();
        let phrases = typed_phrases(text, &tokens, &found);
        let shots = sizes(&table, &found);
        return ParsedPrompt::failed(PromptWarning::NoSubject { phrases }, shots, false);
    }

    let start = if lo > 0 { tokens[lo - 1].end } else { 0 };
    let end = match (trail.first(), &tokens[e - 1].possessive) {
        (None, _) => text.len(),
        (Some(_), Some((marker, _))) => *marker,
        (Some(first), None) => tokens[first.start].start,
    };
    let (subject, mid) = match after_possessive(&table, &tokens, lo, e) {
        Some((possessive, found, tail)) => {
            let marker = tokens[possessive].possessive.as_ref().map_or(0, |p| p.0);
            let head = clean(&text[start..marker], lo > 0, true);
            let tail = clean(&text[tokens[tail].start..end], false, !trail.is_empty());
            (format!("{head} {tail}"), found)
        }
        None => (
            clean(&text[start..end], lo > 0, !trail.is_empty()).to_string(),
            Vec::new(),
        ),
    };

    let kept: Vec<Match> = lead
        .found
        .iter()
        .chain(&mid)
        .chain(&trail)
        .copied()
        .collect();
    let shots = sizes(&table, &kept);
    let mut notes: Vec<(&str, &str)> = Vec::new();
    for m in &kept {
        let phrase = typed(text, &tokens, m);
        if let Some(note) = table.entry(*m).note
            && !notes
                .iter()
                .any(|&(p, n)| n == note && phrase_key(p) == phrase_key(phrase))
        {
            notes.push((phrase, note));
        }
    }

    let words = tokenize(&subject);
    let role = role_of(&table, &words);
    let basis = if role.is_some() {
        Basis::Person
    } else {
        basis_of(&table, &words)
    };
    let already: Vec<String> = warnings
        .iter()
        .filter_map(|w| match w {
            PromptWarning::ReadAsSubject { phrases, .. } => Some(phrases),
            _ => None,
        })
        .flatten()
        .map(|p| phrase_key(p))
        .collect();
    let (stray, after_only) = shot_words_in(&table, &subject, &words, &already);
    if !stray.is_empty() {
        warnings.push(PromptWarning::InSubject { phrases: stray });
    }
    if !after_only.is_empty() {
        warnings.push(PromptWarning::AfterSubjectOnly {
            phrases: after_only,
        });
    }
    warnings.extend(misreads(&table, text, &tokens, &lead, &trail, e));
    warnings.extend(notes.into_iter().map(|(phrase, note)| PromptWarning::Note {
        phrase: phrase.to_string(),
        note: note.to_string(),
    }));
    warnings.extend(action_role(role));
    ParsedPrompt {
        model_phrase: role.map_or_else(|| subject.clone(), |r| r.phrase.to_string()),
        subject,
        shots,
        basis,
        literal: false,
        warnings,
    }
}

/// True for a word after which a shot word must be a noun.
fn is_steering(word: &str) -> bool {
    [DETERMINERS, LINK_WORDS, JOINERS, CONNECTORS]
        .iter()
        .any(|class| is_in(class, word))
}

/// The shot words read at the start of a prompt.
#[derive(Debug, Default)]
struct Lead {
    /// Index of the first word after them (and after their connector).
    end: usize,
    /// The phrases, in order.
    found: Vec<Match>,
    /// Index of the "of" or "on" after them, if any.
    connector: Option<usize>,
}

/// Reads shot words at the start of a prompt; see the module docs.
fn leading(table: &Table<'_>, tokens: &[Token]) -> Lead {
    let n = tokens.len();
    let article = is_in(DETERMINERS, &tokens[0].core);
    let (mut i, found) = table.run_forward(tokens, usize::from(article), n, false);
    if found.is_empty() {
        return Lead::default();
    }
    let connector = (i < n && is_in(CONNECTORS, &tokens[i].core)).then_some(i);
    i += usize::from(connector.is_some());
    let blocked = tokens
        .get(i)
        .is_some_and(|t| is_in(LINK_WORDS, &t.core) || is_in(JOINERS, &t.core));
    if blocked || (article && connector.is_none()) {
        return Lead::default();
    }
    Lead {
        end: i,
        found,
        connector,
    }
}

/// Finds shot words right after a possessive inside `lo..e` and followed by
/// a preposition: the possessive's index, the phrases, and the
/// preposition's index.
fn after_possessive(
    table: &Table<'_>,
    tokens: &[Token],
    lo: usize,
    e: usize,
) -> Option<(usize, Vec<Match>, usize)> {
    (lo..e.saturating_sub(1)).find_map(|i| {
        tokens[i].possessive.as_ref()?;
        let (j, found) = table.run_forward(tokens, i + 1, e, true);
        let preposition =
            j < e && (is_in(LINK_WORDS, &tokens[j].core) || is_in(CONNECTORS, &tokens[j].core));
        (!found.is_empty() && preposition).then_some((i, found, j))
    })
}

/// The typed text of a phrase.
fn typed<'t>(text: &'t str, tokens: &[Token], m: &Match) -> &'t str {
    &text[tokens[m.start].start..tokens[m.end - 1].end]
}

/// The typed text of each phrase, without repeats (by key), so a prompt
/// repeating one word many times yields one phrase.
fn typed_phrases(text: &str, tokens: &[Token], found: &[Match]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in found {
        let phrase = typed(text, tokens, m);
        if !out.iter().any(|p| phrase_key(p) == phrase_key(phrase)) {
            out.push(phrase.to_string());
        }
    }
    out
}

/// The sizes of `found`, in order, without repeats.
fn sizes(table: &Table<'_>, found: &[Match]) -> Vec<ShotSize> {
    let mut out = Vec::new();
    for m in found {
        for &size in table.entry(*m).sizes {
            if !out.contains(&size) {
                out.push(size);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
