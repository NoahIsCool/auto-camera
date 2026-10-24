//! The lexicon compiled for matching, and longest-match runs of shot
//! phrases over a prompt's words.
//!
//! # Theory
//!
//! A shot phrase matches a *window* of consecutive words whose cores, run
//! together, equal the phrase's key. Comparing run-together keys makes
//! spaces and hyphens inside a phrase optional ("close up", "close-up",
//! "closeup"), and longest match is simply the widest window that matches.
//! A window may not cross punctuation ("medium, close" is two phrases) and
//! may not be cut out of a hyphenated compound ("medium-sized").
//!
//! A *run* is a sequence of phrases, each optionally followed by a filler
//! ("medium shot") and joined by nothing, punctuation or a joiner ("close
//! and medium"). Runs are read forwards at the start of a prompt and after a
//! possessive, and backwards at its end.
//!
//! Shot words match their "s" plurals too ("close-ups", "headshots",
//! "CUs"), but not "es" ones, so "pool cues" holds no "CU". Role and person
//! words match by key too, with "s" and "es" plurals.
//!
//! # Invariants
//!
//! * Entries [`crate::Lexicon::validate`] would reject are skipped, so a
//!   malformed lexicon never makes matching fail.
//! * For equal keys the later lexicon entry wins.
//! * Every window is at most one byte longer, in key bytes, than the longest
//!   key (room for a plural "s"), so a run costs time linear in its length
//!   whatever the prompt's length.

use std::collections::{HashMap, HashSet};

use super::words::{
    FILLERS, JOINERS, Joint, Token, is_grammar_word, is_in, phrase_key, phrase_words,
};
use crate::lexicon::{Lexicon, Placement, RoleNeeds, WordClass};
use crate::model::ShotSize;

/// A usable shot word.
#[derive(Debug)]
pub(super) struct Entry<'a> {
    /// The sizes it asks for.
    pub(super) sizes: &'a [ShotSize],
    /// It counts only after a subject.
    pub(super) after_subject: bool,
    /// The caveat to show when it is used.
    pub(super) note: Option<&'a str>,
    /// Its everyday reading, if any.
    pub(super) also: Option<WordClass>,
}

/// A usable role word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Role<'a> {
    /// The role as the lexicon spells it ("speaker").
    pub(super) word: &'a str,
    /// The detector phrase, trimmed ("a person speaking").
    pub(super) phrase: &'a str,
    /// What the phrase needs the detector to see.
    pub(super) needs: Option<RoleNeeds>,
}

/// A shot phrase found in a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Match {
    /// Index of its first word.
    pub(super) start: usize,
    /// Index just past its last word (a following filler is not included).
    pub(super) end: usize,
    /// Index of its entry in the table.
    pub(super) entry: usize,
}

/// A [`Lexicon`] compiled for matching.
#[derive(Debug)]
pub(super) struct Table<'a> {
    /// Usable shot words.
    entries: Vec<Entry<'a>>,
    /// Shot-word key to entry index.
    keys: HashMap<String, usize>,
    /// Length in bytes of the longest shot-word key.
    max_key_len: usize,
    /// Role-word key to role.
    roles: HashMap<String, Role<'a>>,
    /// Keys of person and role words.
    people: HashSet<String>,
    /// Most words in any person or role word.
    max_person_words: usize,
}

impl<'a> Table<'a> {
    /// Compiles `lexicon`, skipping malformed entries.
    pub(super) fn new(lexicon: &'a Lexicon) -> Self {
        let usable = |key: &str| !key.is_empty() && !is_grammar_word(key);
        let mut entries = Vec::new();
        let mut keys = HashMap::new();
        for word in &lexicon.shots {
            let key = phrase_key(&word.phrase);
            if usable(&key) && !word.sizes.is_empty() && !word.sizes.contains(&ShotSize::Wide) {
                keys.insert(key, entries.len());
                entries.push(Entry {
                    sizes: &word.sizes,
                    after_subject: word.placement == Placement::AfterSubject,
                    note: word.note.as_deref(),
                    also: word.also,
                });
            }
        }
        let mut roles = HashMap::new();
        let mut people = HashSet::new();
        let mut max_person_words = 0;
        let person_words = lexicon.person_words.iter().map(|w| (w, None));
        let role_words = lexicon.roles.iter().map(|r| {
            let role = Role {
                word: r.word.trim(),
                phrase: r.phrase.trim(),
                needs: r.needs,
            };
            (&r.word, Some(role))
        });
        for (word, role) in person_words.chain(role_words) {
            let key = phrase_key(word);
            if !usable(&key) || role.is_some_and(|r| r.phrase.is_empty()) {
                continue;
            }
            max_person_words = max_person_words.max(phrase_words(word).len());
            if let Some(role) = role {
                roles.insert(key.clone(), role);
            }
            people.insert(key);
        }
        Self {
            max_key_len: keys.keys().map(String::len).max().unwrap_or(0),
            entries,
            keys,
            roles,
            people,
            max_person_words,
        }
    }

    /// The entry a match found.
    pub(super) fn entry(&self, m: Match) -> &Entry<'a> {
        &self.entries[m.entry]
    }

    /// The entry for a window's key or its "s" plural, if it may be used
    /// there.
    fn lookup(&self, key: &str, allow_after: bool) -> Option<usize> {
        let &entry = self
            .keys
            .get(key)
            .or_else(|| key.strip_suffix('s').and_then(|k| self.keys.get(k)))?;
        (allow_after || !self.entries[entry].after_subject).then_some(entry)
    }

    /// True when `tokens[start..end]` is a whole word group: not cut out of
    /// a hyphenated compound on either side.
    fn whole(tokens: &[Token], start: usize, end: usize) -> bool {
        tokens[start].joint != Joint::Hyphen
            && tokens.get(end).is_none_or(|t| t.joint != Joint::Hyphen)
    }

    /// The longest phrase starting at `start` and ending by `hi`.
    pub(super) fn phrase_at(
        &self,
        tokens: &[Token],
        start: usize,
        hi: usize,
        allow_after: bool,
    ) -> Option<Match> {
        let mut key = String::new();
        let mut best = None;
        for end in start + 1..=hi {
            let last = &tokens[end - 1];
            if end - 1 > start && !last.glued() {
                break;
            }
            key.push_str(&last.core);
            if key.len() > self.max_key_len + 1 {
                break;
            }
            if let Some(entry) = self.lookup(&key, allow_after)
                && Self::whole(tokens, start, end)
            {
                best = Some(Match { start, end, entry });
            }
        }
        best
    }

    /// The longest phrase ending at `end` and starting at or after `lo`.
    fn phrase_before(&self, tokens: &[Token], lo: usize, end: usize) -> Option<Match> {
        let mut key = String::new();
        let mut best = None;
        for start in (lo..end).rev() {
            if start + 1 < end && !tokens[start + 1].glued() {
                break;
            }
            key.insert_str(0, &tokens[start].core);
            if key.len() > self.max_key_len + 1 {
                break;
            }
            if let Some(entry) = self.lookup(&key, true)
                && Self::whole(tokens, start, end)
            {
                best = Some(Match { start, end, entry });
            }
        }
        best
    }

    /// A phrase ending at `end`, or right before a filler that does.
    fn phrase_ending(&self, tokens: &[Token], lo: usize, end: usize) -> Option<Match> {
        self.phrase_before(tokens, lo, end).or_else(|| {
            (end > lo && is_in(FILLERS, &tokens[end - 1].core))
                .then(|| self.phrase_before(tokens, lo, end - 1))
                .flatten()
        })
    }

    /// The run of phrases starting at `start`, within `..hi`: the index just
    /// past it, and its phrases in order (empty when none starts there).
    pub(super) fn run_forward(
        &self,
        tokens: &[Token],
        start: usize,
        hi: usize,
        allow_after: bool,
    ) -> (usize, Vec<Match>) {
        let mut i = start;
        let mut found = Vec::new();
        while let Some(m) = self.phrase_at(tokens, i, hi, allow_after) {
            found.push(m);
            i = m.end;
            if i < hi && is_in(FILLERS, &tokens[i].core) {
                i += 1;
            }
            if i + 1 < hi
                && is_in(JOINERS, &tokens[i].core)
                && self.phrase_at(tokens, i + 1, hi, allow_after).is_some()
            {
                i += 1;
            }
        }
        (i, found)
    }

    /// The run of phrases ending at `end`, within `lo..`: the index of its
    /// first word, and its phrases in order (empty when none ends there).
    pub(super) fn run_backward(
        &self,
        tokens: &[Token],
        lo: usize,
        end: usize,
    ) -> (usize, Vec<Match>) {
        let mut e = end;
        let mut found = Vec::new();
        while let Some(m) = self.phrase_ending(tokens, lo, e) {
            found.push(m);
            e = m.start;
            if e >= lo + 2
                && is_in(JOINERS, &tokens[e - 1].core)
                && self.phrase_ending(tokens, lo, e - 1).is_some()
            {
                e -= 1;
            }
        }
        found.reverse();
        (e, found)
    }

    /// The role a subject is exactly (`words` are its word cores,
    /// determiners already dropped).
    pub(super) fn role(&self, words: &[&str]) -> Option<Role<'a>> {
        plural_lookup(&words.concat(), |key| self.roles.get(key).copied())
    }

    /// True when `words` (stems) end in a person or role word.
    pub(super) fn names_person(&self, words: &[&str]) -> bool {
        (1..=self.max_person_words.min(words.len())).any(|n| {
            let key = words[words.len() - n..].concat();
            plural_lookup(&key, |k| self.people.contains(k).then_some(())).is_some()
        })
    }
}

/// Looks `key` up as given, then without an "s" or "es" plural ending.
fn plural_lookup<T>(key: &str, get: impl Fn(&str) -> Option<T>) -> Option<T> {
    get(key)
        .or_else(|| key.strip_suffix('s').and_then(&get))
        .or_else(|| key.strip_suffix("es").and_then(&get))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::words::tokenize;

    /// The typed text of each phrase `run` finds in `text`, read backwards
    /// from the end.
    fn trailing(text: &str) -> Vec<String> {
        let lexicon = Lexicon::builtin();
        let table = Table::new(&lexicon);
        let tokens = tokenize(text);
        let (_, found) = table.run_backward(&tokens, 0, tokens.len());
        found
            .iter()
            .map(|m| text[tokens[m.start].start..tokens[m.end - 1].end].to_string())
            .collect()
    }

    #[test]
    fn longest_window_wins() {
        assert_eq!(trailing("x medium close-up"), ["medium close-up"]);
        assert_eq!(trailing("x medium close"), ["medium close"]);
        assert_eq!(trailing("x head and shoulders"), ["head and shoulders"]);
    }

    #[test]
    fn punctuation_splits_phrases_and_joiners_join_them() {
        assert_eq!(trailing("x medium, close"), ["medium", "close"]);
        assert_eq!(trailing("x close and medium shot"), ["close", "medium"]);
        assert_eq!(trailing("x and close"), ["close"]);
    }

    #[test]
    fn shot_words_match_their_s_plurals_only() {
        assert_eq!(trailing("x close-ups"), ["close-ups"]);
        assert_eq!(trailing("x closeups"), ["closeups"]);
        assert_eq!(trailing("x headshots"), ["headshots"]);
        assert_eq!(trailing("x CUs"), ["CUs"]);
        assert_eq!(trailing("x medium close-ups"), ["medium close-ups"]);
        assert!(trailing("pool cues").is_empty());
        assert!(trailing("x close-upss").is_empty());
    }

    #[test]
    fn compounds_are_not_cut() {
        assert!(trailing("x full-time").is_empty());
        assert!(trailing("x semi-close").is_empty());
        assert_eq!(trailing("x full-length"), ["full-length"]);
    }

    #[test]
    fn placement_limits_forward_runs() {
        let lexicon = Lexicon::builtin();
        let table = Table::new(&lexicon);
        let tokens = tokenize("wide close");
        assert_eq!(table.run_forward(&tokens, 0, 2, false).0, 0);
        assert_eq!(table.run_forward(&tokens, 0, 2, true).0, 2);
    }

    #[test]
    fn malformed_entries_are_skipped() {
        use crate::lexicon::{RoleWord, ShotWord};
        let lexicon = Lexicon {
            shots: vec![
                ShotWord {
                    phrase: "--".into(),
                    sizes: vec![ShotSize::Close],
                    placement: Placement::Anywhere,
                    note: None,
                    also: None,
                },
                ShotWord {
                    phrase: "snug".into(),
                    sizes: vec![ShotSize::Wide],
                    placement: Placement::Anywhere,
                    note: None,
                    also: None,
                },
                ShotWord {
                    phrase: "of".into(),
                    sizes: vec![ShotSize::Close],
                    placement: Placement::Anywhere,
                    note: None,
                    also: None,
                },
            ],
            roles: vec![RoleWord {
                word: "host".into(),
                phrase: " ".into(),
                needs: Some(RoleNeeds::Action),
            }],
            person_words: vec![String::new()],
        };
        let table = Table::new(&lexicon);
        assert!(table.entries.is_empty() && table.roles.is_empty() && table.people.is_empty());
        let tokens = tokenize("snug of host");
        assert_eq!(table.run_backward(&tokens, 0, 3), (3, vec![]));
        assert!(!table.names_person(&["host"]));
    }

    #[test]
    fn plurals_match_people_and_roles() {
        let lexicon = Lexicon::builtin();
        let table = Table::new(&lexicon);
        assert!(table.names_person(&["two", "women"]));
        assert!(table.names_person(&["guitar", "players"]));
        assert!(table.names_person(&["bass", "guitarists"]));
        assert!(!table.names_person(&["bus"]) && !table.names_person(&[]));
        assert_eq!(
            table.role(&["preachers"]),
            Some(Role {
                word: "preacher",
                phrase: "a person speaking",
                needs: Some(RoleNeeds::Action),
            })
        );
        assert_eq!(table.role(&["bass", "guitarist"]), None);
    }
}
