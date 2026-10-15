//! Tests for the prompt parser: every row of the shot and role tables, the
//! three places shot words count, spelling tolerance and the basis rule.
//! [`edge`] covers literal, invalid and hostile prompts and custom lexicons.

use super::*;
use ShotSize::{Close, Full, Medium, MediumClose};

mod edge;

/// Parses `text` with the built-in lexicon.
fn parse(text: &str) -> ParsedPrompt {
    parse_prompt(text, &Lexicon::builtin())
}

/// Asserts that `text` parses to `model_phrase`, `shots` and `basis`
/// without an error.
fn assert_parse(text: &str, model_phrase: &str, shots: &[ShotSize], basis: Basis) {
    let p = parse(text);
    assert!(p.is_valid(), "{text:?}: {:?}", p.warnings);
    assert_eq!(p.model_phrase, model_phrase, "{text:?}");
    assert_eq!(p.shots, shots, "{text:?}");
    assert_eq!(p.basis, basis, "{text:?}");
    assert!(!p.literal, "{text:?}");
}

/// The shot-word table of design §4.2, written out independently of the
/// lexicon so a change to either is caught.
const TABLE: &[(&str, &[ShotSize])] = &[
    ("close", &[Close]),
    ("close-up", &[Close]),
    ("cu", &[Close]),
    ("head and shoulders", &[Close]),
    ("headshot", &[Close]),
    ("tight", &[Close]),
    ("face", &[Close]),
    ("medium close", &[MediumClose]),
    ("medium close-up", &[MediumClose]),
    ("mcu", &[MediumClose]),
    ("chest up", &[MediumClose]),
    ("bust", &[MediumClose]),
    ("medium", &[Medium]),
    ("mid shot", &[Medium]),
    ("ms", &[Medium]),
    ("waist up", &[Medium]),
    ("upper body", &[Medium, MediumClose]),
    ("half body", &[Medium, MediumClose]),
    ("full", &[Full]),
    ("full body", &[Full]),
    ("full length", &[Full]),
    ("head to toe", &[Full]),
    ("long shot", &[Full]),
    ("fs", &[Full]),
];

#[test]
fn every_table_row_counts_at_the_end() {
    for (phrase, sizes) in TABLE {
        assert_parse(&format!("guitar {phrase}"), "guitar", sizes, Basis::Object);
    }
    assert_parse("guitar wide", "guitar", &[Full], Basis::Object);
}

#[test]
fn every_table_row_counts_at_the_start() {
    for (phrase, sizes) in TABLE {
        assert_parse(&format!("{phrase} guitar"), "guitar", sizes, Basis::Object);
        assert_parse(
            &format!("{phrase} of the guitar"),
            "the guitar",
            sizes,
            Basis::Object,
        );
    }
}

#[test]
fn every_table_row_counts_after_a_possessive() {
    for (phrase, sizes) in TABLE {
        assert_parse(
            &format!("drum kit's {phrase}"),
            "drum kit",
            sizes,
            Basis::Object,
        );
    }
}

#[test]
fn the_noahs_example_splits_who_from_what_shot() {
    let p = parse("speaker upper body");
    assert_eq!(p.subject, "speaker");
    assert_eq!(p.model_phrase, "a person speaking");
    assert_eq!(p.shots, [Medium, MediumClose]);
    assert_eq!(p.basis, Basis::Person);
    assert!(p.warnings.is_empty());
    assert_eq!(
        p.summary(),
        "Detector sees: a person speaking · Framing: Medium, Medium close (person)"
    );
}

#[test]
fn design_worked_examples() {
    assert_parse(
        "guitarist close-up",
        "a person playing guitar",
        &[Close],
        Basis::Person,
    );
    assert_parse("guitar close-up", "guitar", &[Close], Basis::Object);
    assert_parse(
        "drummer wide",
        "a person playing drums",
        &[Full],
        Basis::Person,
    );
    assert_parse("bass guitarist", "bass guitarist", &[], Basis::Person);
}

#[test]
fn every_role_word_is_rewritten_when_it_is_the_whole_subject() {
    for (role, phrase) in [
        ("speaker", "a person speaking"),
        ("pastor", "a person speaking"),
        ("preacher", "a person speaking"),
        ("guitarist", "a person playing guitar"),
        ("drummer", "a person playing drums"),
        ("singer", "a person singing"),
    ] {
        assert_parse(role, phrase, &[], Basis::Person);
        assert_parse(&format!("The {role}"), phrase, &[], Basis::Person);
        assert_parse(&format!("all the {role}s"), phrase, &[], Basis::Person);
        assert_parse(&format!("{role} close"), phrase, &[Close], Basis::Person);
        let p = parse(&format!("{role}'s face"));
        assert_eq!(
            (p.subject.as_str(), p.model_phrase.as_str()),
            (role, phrase)
        );
    }
}

#[test]
fn roles_inside_longer_subjects_are_sent_as_typed() {
    assert_parse("lead singer", "lead singer", &[], Basis::Person);
    assert_parse(
        "guitarist on the left close",
        "guitarist on the left",
        &[Close],
        Basis::Person,
    );
    assert_parse("speaker stand", "speaker stand", &[], Basis::Object);
    assert_parse(
        "the speaker's microphone close-up",
        "the speaker's microphone",
        &[Close],
        Basis::Object,
    );
}

#[test]
fn matching_ignores_case() {
    assert_parse(
        "SPEAKER CLOSE-UP",
        "a person speaking",
        &[Close],
        Basis::Person,
    );
    assert_parse(
        "Guitarist Upper Body",
        "a person playing guitar",
        &[Medium, MediumClose],
        Basis::Person,
    );
    assert_parse(
        "MCU of The Pastor",
        "a person speaking",
        &[MediumClose],
        Basis::Person,
    );
    assert_eq!(parse("Red Guitar CU").subject, "Red Guitar");
}

#[test]
fn hyphens_and_spaces_inside_phrases_are_optional() {
    for spelling in [
        "close-up",
        "close up",
        "closeup",
        "Close\u{2010}Up",
        "close\u{2011}up",
        "close\u{2013}up",
        "close \u{2014} up",
        "close_up",
    ] {
        assert_parse(
            &format!("guitar {spelling}"),
            "guitar",
            &[Close],
            Basis::Object,
        );
    }
    for (spelling, sizes) in [
        ("head-and-shoulders", &[Close][..]),
        ("head shot", &[Close]),
        ("upper-body", &[Medium, MediumClose]),
        ("upperbody", &[Medium, MediumClose]),
        ("full-length", &[Full]),
        ("head-to-toe", &[Full]),
        ("waist-up", &[Medium]),
        ("chest-up", &[MediumClose]),
        ("mid-shot", &[Medium]),
        ("medium-close", &[MediumClose]),
    ] {
        assert_parse(
            &format!("speaker {spelling}"),
            "a person speaking",
            sizes,
            Basis::Person,
        );
    }
}

#[test]
fn typographic_apostrophes_mark_possessives() {
    for text in [
        "speaker\u{2019}s upper body",
        "speaker\u{2018}s upper body",
        "speaker\u{02BC}s upper body",
        "speaker's upper body",
        "speakers\u{2019} upper body",
    ] {
        let p = parse(text);
        assert_eq!(p.model_phrase, "a person speaking", "{text}");
        assert_eq!(p.shots, [Medium, MediumClose], "{text}");
        assert!(p.warnings.is_empty(), "{text}: {:?}", p.warnings);
    }
    assert_eq!(parse("Speaker\u{2019}s close-up").subject, "Speaker");
}

#[test]
fn possessive_shots_may_be_followed_by_a_preposition() {
    let p = parse("the speaker's upper body at the lectern");
    assert_eq!(p.subject, "the speaker at the lectern");
    assert_eq!(p.model_phrase, p.subject);
    assert_eq!(p.shots, [Medium, MediumClose]);
    assert_eq!(p.basis, Basis::Person);
    let p = parse("drummer\u{2019}s full body behind the kit");
    assert_eq!(p.subject, "drummer behind the kit");
    assert_eq!(p.shots, [Full]);
}

#[test]
fn possessives_without_shot_words_are_kept() {
    // "full" is an adjective of "kit" here, not a shot after the possessive.
    let p = parse("the drummer's full kit");
    assert_eq!(p.model_phrase, "the drummer's full kit");
    assert!(p.shots.is_empty());
    assert_eq!(p.basis, Basis::Object);
    assert_parse("person's", "person's", &[], Basis::Person);
}

#[test]
fn longest_match_wins() {
    assert_parse(
        "singer medium close",
        "a person singing",
        &[MediumClose],
        Basis::Person,
    );
    assert_parse(
        "singer medium close-up",
        "a person singing",
        &[MediumClose],
        Basis::Person,
    );
    assert_parse(
        "singer full body",
        "a person singing",
        &[Full],
        Basis::Person,
    );
    assert_parse(
        "singer head and shoulders",
        "a person singing",
        &[Close],
        Basis::Person,
    );
    assert_parse(
        "medium close guitar",
        "guitar",
        &[MediumClose],
        Basis::Object,
    );
}

#[test]
fn several_shot_words_keep_their_order_without_repeats() {
    assert_parse(
        "speaker close and medium",
        "a person speaking",
        &[Close, Medium],
        Basis::Person,
    );
    assert_parse(
        "speaker close, medium",
        "a person speaking",
        &[Close, Medium],
        Basis::Person,
    );
    assert_parse(
        "speaker medium/close",
        "a person speaking",
        &[Medium, Close],
        Basis::Person,
    );
    assert_parse(
        "drummer close-up & full body",
        "a person playing drums",
        &[Close, Full],
        Basis::Person,
    );
    assert_parse(
        "upper body speaker close",
        "a person speaking",
        &[Medium, MediumClose, Close],
        Basis::Person,
    );
    assert_parse("close-up guitar close", "guitar", &[Close], Basis::Object);
    assert_parse("guitar close cu tight", "guitar", &[Close], Basis::Object);
}

#[test]
fn shot_fillers_and_connectors_are_absorbed() {
    assert_parse(
        "speaker medium shot",
        "a person speaking",
        &[Medium],
        Basis::Person,
    );
    assert_parse(
        "speaker wide shot",
        "a person speaking",
        &[Full],
        Basis::Person,
    );
    assert_parse("guitar close shots", "guitar", &[Close], Basis::Object);
    assert_parse(
        "tight shot of the drummer",
        "a person playing drums",
        &[Close],
        Basis::Person,
    );
    assert_parse(
        "a close-up of the guitar",
        "the guitar",
        &[Close],
        Basis::Object,
    );
    assert_parse(
        "medium shot on the singer",
        "a person singing",
        &[Medium],
        Basis::Person,
    );
    assert_parse("long shot of the band", "the band", &[Full], Basis::Object);
}

#[test]
fn punctuation_where_shot_words_were_cut_is_trimmed() {
    for text in [
        "guitar, close-up",
        "guitar - close up.",
        "guitar (close)",
        "guitar: close",
        "Close-up: guitar",
        "close-up \u{2014} guitar",
        "  guitar   close  ",
    ] {
        assert_eq!(parse(text).subject, "guitar", "{text:?}");
    }
    assert_eq!(parse("guitar (left) close").subject, "guitar (left)");
    assert_eq!(parse("\"red\" guitar, close").subject, "\"red\" guitar");
}

#[test]
fn quotes_and_brackets_around_cut_shot_words_never_reach_the_detector() {
    for text in [
        "guitar \"close-up\"",
        "guitar <close-up>",
        "guitar \u{AB}close-up\u{BB}",
        "guitar *close-up*",
        "guitar \u{201C}close-up\u{201D}",
        "guitar `close-up`",
        "guitar #closeup",
        "\"close-up\" guitar",
        "\u{AB}close-up\u{BB} guitar",
        "<close-up> guitar",
    ] {
        assert_parse(text, "guitar", &[Close], Basis::Object);
    }
    // Balanced quotes in the subject stay.
    assert_parse(
        "\"lead guitar\" close-up",
        "\"lead guitar\"",
        &[Close],
        Basis::Object,
    );
}

#[test]
fn plural_shot_words_count() {
    for text in ["speaker close-ups", "speaker closeups", "speaker headshots"] {
        assert_parse(text, "a person speaking", &[Close], Basis::Person);
        assert!(parse(text).warnings.is_empty(), "{text}");
    }
    assert_parse("guitar CUs", "guitar", &[Close], Basis::Object);
    assert_parse(
        "drummer medium close-ups",
        "a person playing drums",
        &[MediumClose],
        Basis::Person,
    );
    // "es" plurals do not: "cues" is not "CU".
    assert_parse("pool cues", "pool cues", &[], Basis::Object);
}

#[test]
fn prompts_without_shot_or_role_words_are_unchanged() {
    // The example configs, the README and the engine's tests.
    for text in [
        "person",
        "guitar",
        "a person speaking at a lectern",
        "a person playing guitar",
        "person singing",
        "person at the lectern",
        "b",
        "  a person in a red shirt  ",
        "zorblax frobnicator",
        "projection screen (left), not the audience!",
    ] {
        let p = parse(text);
        assert_eq!(p.subject, text.trim(), "{text:?}");
        assert_eq!(p.model_phrase, text.trim(), "{text:?}");
        assert!(
            p.shots.is_empty() && p.warnings.is_empty(),
            "{text:?}: {p:?}"
        );
    }
}

#[test]
fn basis_follows_the_head_noun() {
    for (text, basis) in [
        ("a person playing guitar", Basis::Person),
        ("a person speaking at a lectern", Basis::Person),
        ("people on stage", Basis::Person),
        ("the woman who is singing", Basis::Person),
        ("two guitar players", Basis::Person),
        ("guitarist and drummer", Basis::Person),
        ("a singing woman", Basis::Person),
        ("guitar", Basis::Object),
        ("drum kit", Basis::Object),
        ("guitar and amp", Basis::Object),
        ("a guitar string", Basis::Object),
        ("neck of the guitar", Basis::Object),
        ("the speaker's clothing", Basis::Object),
        ("a painting of a man", Basis::Object),
        ("with a guitar", Basis::Object),
    ] {
        assert_eq!(parse(text).basis, basis, "{text:?}");
    }
}

#[test]
fn wide_counts_only_after_a_subject_and_carries_a_note() {
    let p = parse("drum kit wide");
    assert_eq!(
        (p.model_phrase.as_str(), &p.shots[..]),
        ("drum kit", &[Full][..])
    );
    assert!(matches!(
        &p.warnings[..],
        [PromptWarning::Note { phrase, note }] if phrase == "wide" && note.contains("later")
    ));
    assert!(p.is_valid());
    assert_parse(
        "drummer\u{2019}s wide at the back",
        "drummer at the back",
        &[Full],
        Basis::Person,
    );
    // Before a noun it is an adjective; at the start it is ambiguous with
    // the camera's own wide shot.
    for text in [
        "wide stage",
        "wide shot of the drummer",
        "drummer wide at the back",
    ] {
        let p = parse(text);
        assert_eq!((p.model_phrase.as_str(), p.shots.len()), (text, 0));
        assert_eq!(
            p.warnings,
            [PromptWarning::AfterSubjectOnly {
                phrases: vec!["wide".into()],
            }],
            "{text:?}"
        );
        assert!(p.warnings[0].to_string().contains("\"drummer wide\""));
    }
}
