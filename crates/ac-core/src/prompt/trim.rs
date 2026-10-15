//! Trimming a subject where shot words were cut off it.
//!
//! # Theory
//!
//! The subject is sliced from the operator's text, so whatever stood between
//! it and the shot words cut from it stays at the cut: the comma of "guitar,
//! close-up", the bracket of "guitar (close-up)", the quote of `guitar
//! "close-up"`. The detector would receive those characters as part of its
//! phrase (OWLv2 only folds case and whitespace), so they are trimmed, on
//! the cut side only:
//!
//! * separators, `, ; : . ! ? / + & |` and hyphens or dashes, on either
//!   side;
//! * an opening bracket or quote, `( [ { < « ‹ “ „`, or a `#`, at the end of
//!   a subject whose shot words came after it; a closing one, `) ] } > » ›
//!   ”`, at the start of a subject whose shot words came before it;
//! * a straight double quote, asterisk or backtick, which opens and closes
//!   alike, only when the subject holds an odd number of them: `guitar
//!   *close-up*` loses its stray `*`, `"lead guitar" close-up` keeps its
//!   balanced quotes.
//!
//! The uncut side is never touched, so "guitar (left) close-up" keeps its
//! bracket.
//!
//! # Invariants
//!
//! * The result is a substring of the input.
//! * Only whitespace and the characters above are removed, and only at the
//!   cut ends.

/// Separators trimmed from whichever side of a subject was cut.
const SEPARATORS: &[char] = &[
    ',', ';', ':', '.', '!', '?', '/', '+', '&', '|', '-', '\u{2013}', '\u{2014}',
];

/// Openers trimmed from the end of a subject cut after it.
const OPENERS: &[char] = &[
    '(', '[', '{', '<', '\u{AB}', '\u{2039}', '\u{201C}', '\u{201E}', '#',
];

/// Closers trimmed from the start of a subject cut before it.
const CLOSERS: &[char] = &[')', ']', '}', '>', '\u{BB}', '\u{203A}', '\u{201D}'];

/// Marks that open and close alike; trimmed only when unbalanced.
const SYMMETRIC: &[char] = &['"', '*', '`'];

/// True when `c` is a symmetric mark that `text` holds an odd number of.
fn unbalanced(text: &str, c: char) -> bool {
    SYMMETRIC.contains(&c) && text.matches(c).count() % 2 == 1
}

/// `text` trimmed of whitespace, and of separators and stray wrappers on
/// the sides where shot words were cut; see the module docs.
pub(super) fn clean(text: &str, cut_before: bool, cut_after: bool) -> &str {
    let mut text = text.trim();
    loop {
        let len = text.len();
        if cut_before {
            text = text.trim_start_matches(|c: char| {
                c.is_whitespace() || SEPARATORS.contains(&c) || CLOSERS.contains(&c)
            });
            if let Some(c) = text.chars().next().filter(|&c| unbalanced(text, c)) {
                text = &text[c.len_utf8()..];
            }
        }
        if cut_after {
            text = text.trim_end_matches(|c: char| {
                c.is_whitespace() || SEPARATORS.contains(&c) || OPENERS.contains(&c)
            });
            if let Some(c) = text.chars().next_back().filter(|&c| unbalanced(text, c)) {
                text = &text[..text.len() - c.len_utf8()];
            }
        }
        if text.len() == len {
            return text;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stray_wrappers_go_on_the_cut_side_only() {
        for (text, before, after, out) in [
            ("guitar \"", false, true, "guitar"),
            ("guitar <", false, true, "guitar"),
            ("guitar \u{AB}", false, true, "guitar"),
            ("guitar *", false, true, "guitar"),
            ("guitar \u{201C}", false, true, "guitar"),
            ("guitar #", false, true, "guitar"),
            ("guitar, (", false, true, "guitar"),
            ("\" guitar", true, false, "guitar"),
            ("\u{BB} guitar", true, false, "guitar"),
            ("> guitar", true, false, "guitar"),
            // Uncut sides and balanced marks are kept.
            ("\"lead guitar\"", true, true, "\"lead guitar\""),
            ("guitar (left)", false, true, "guitar (left)"),
            ("(left) guitar", false, true, "(left) guitar"),
            ("guitar \"", false, false, "guitar \""),
            ("*lead* guitar *", false, true, "*lead* guitar"),
        ] {
            assert_eq!(clean(text, before, after), out, "{text:?}");
        }
    }

    #[test]
    fn nothing_but_wrappers_trims_to_nothing() {
        assert_eq!(clean(" \" ( * ", false, true), "");
        assert_eq!(clean("", true, true), "");
    }
}
