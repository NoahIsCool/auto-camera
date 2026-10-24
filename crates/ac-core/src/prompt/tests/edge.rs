//! Tests for the prompt parser's edges: literal prompts, empty and
//! shot-word-only prompts, shot words that are nouns or sit mid-prompt,
//! very long and odd input, and custom or malformed lexicons.

use super::*;
use crate::lexicon::{Placement, RoleNeeds, RoleWord, ShotWord};

#[test]
fn literal_prompts_bypass_parsing() {
    let lexicon = Lexicon::builtin();
    let p = literal_prompt("  speaker upper body ", &lexicon);
    assert_eq!(p.subject, "speaker upper body");
    assert_eq!(p.model_phrase, "speaker upper body");
    assert!(p.shots.is_empty() && p.warnings.is_empty() && p.literal);
    assert_eq!(p.basis, Basis::Object);
    assert_eq!(
        literal_prompt("a person singing", &lexicon).basis,
        Basis::Person
    );
    assert!(literal_prompt("close-up", &lexicon).is_valid());
    assert_eq!(
        literal_prompt("close-up", &lexicon).summary(),
        "Detector sees: close-up (literal) · Framing: no shot words (object)"
    );
    for empty in ["", "  ", "?!"] {
        let p = literal_prompt(empty, &lexicon);
        assert_eq!(p.error(), Some(&PromptWarning::Empty));
        assert!(p.literal && p.model_phrase.is_empty());
    }
}

#[test]
fn empty_prompts_are_errors() {
    for text in ["", "   ", "\t\n", "?!", "-- ''", "🎸"] {
        let p = parse(text);
        assert_eq!(p.warnings, [PromptWarning::Empty], "{text:?}");
        assert!(!p.is_valid() && p.model_phrase.is_empty() && p.shots.is_empty());
        assert!(p.summary().contains("empty"));
    }
}

#[test]
fn shot_words_alone_are_errors() {
    for (text, shots) in [
        ("close-up", &[Close][..]),
        ("Upper Body", &[Medium, MediumClose]),
        ("close and medium", &[Close, Medium]),
        ("wide", &[Full]),
        ("medium shot", &[Medium]),
        ("close-up of", &[Close]),
        ("close-up of the", &[Close]),
        ("🎸 close-up", &[Close]),
    ] {
        let p = parse(text);
        assert!(
            matches!(p.error(), Some(PromptWarning::NoSubject { .. })),
            "{text:?}: {p:?}"
        );
        assert_eq!(p.shots, shots, "{text:?}");
        assert!(
            p.model_phrase.is_empty() && p.subject.is_empty(),
            "{text:?}"
        );
    }
    let message = parse("close-up").summary();
    assert!(message.contains("\"close-up\"") && message.contains("speaker close-up"));
    // An old config's "face" is told how to look for a face itself.
    let message = parse("face").summary();
    assert!(
        message.contains("\"the face\"") && message.contains("literal"),
        "{message}"
    );
}

#[test]
fn shot_words_after_an_article_or_preposition_are_nouns() {
    let p = parse("a face");
    assert_eq!((p.model_phrase.as_str(), p.shots.len()), ("a face", 0));
    assert_eq!(
        p.warnings,
        [PromptWarning::ReadAsSubject {
            phrases: vec!["face".into()],
            after: "a".into(),
        }]
    );
    assert!(p.is_valid());
    assert_parse("the bust", "the bust", &[], Basis::Object);
    assert_parse(
        "a statue of a bust",
        "a statue of a bust",
        &[],
        Basis::Object,
    );
    assert_parse("close-up of the face", "the face", &[Close], Basis::Object);
    assert_parse("close-up of face", "face", &[Close], Basis::Object);
    assert_parse("guitar and close", "guitar and close", &[], Basis::Object);
}

#[test]
fn shot_words_at_the_start_need_a_clean_lead_in() {
    // A preposition after them, or an article without "of"/"on", means
    // they are not shot words.
    for text in [
        "close to the lectern",
        "the full band",
        "full-time musician",
    ] {
        let p = parse(text);
        assert_eq!(p.model_phrase, text, "{text:?}");
        assert!(p.shots.is_empty(), "{text:?}");
    }
    assert_eq!(parse("full-time musician").basis, Basis::Person);
    assert_parse("full band", "band", &[Full], Basis::Object);
    assert_parse(
        "tight on the drummer",
        "a person playing drums",
        &[Close],
        Basis::Person,
    );
}

#[test]
fn shot_words_in_the_middle_are_flagged_not_parsed() {
    let p = parse("guitar close-up at the back");
    assert_eq!(p.model_phrase, "guitar close-up at the back");
    assert!(p.shots.is_empty() && p.is_valid());
    assert_eq!(
        p.warnings,
        [PromptWarning::InSubject {
            phrases: vec!["close-up".into()],
        }]
    );
    assert!(p.warnings[0].to_string().contains("start or end"));
    // "extreme" is not a shot word, so "close-up" does not start the prompt.
    let p = parse("extreme close-up of the singer");
    assert_eq!(p.model_phrase, "extreme close-up of the singer");
    assert!(matches!(&p.warnings[..], [PromptWarning::InSubject { .. }]));
    let p = parse("person in medium close attire full");
    assert_eq!(p.shots, [Full]);
    assert!(
        matches!(&p.warnings[..], [PromptWarning::InSubject { phrases }] if phrases == &["medium close"])
    );
}

#[test]
fn compounds_are_not_split_into_shot_words() {
    for text in [
        "guitar semi-close",
        "medium-sized guitar",
        "a close-knit band",
    ] {
        let p = parse(text);
        assert!(
            p.shots.is_empty() && p.warnings.is_empty(),
            "{text:?}: {p:?}"
        );
        assert_eq!(p.model_phrase, text);
    }
}

#[test]
fn very_long_prompts_parse_in_linear_time() {
    let long_subject = "very ".repeat(20_000) + "guitar close-up";
    let p = parse(&long_subject);
    assert_eq!(p.shots, [Close]);
    assert!(p.subject.ends_with("very guitar"));
    let only_shots = "close ".repeat(20_000);
    let p = parse(&only_shots);
    assert_eq!(
        p.error(),
        Some(&PromptWarning::NoSubject {
            phrases: vec!["close".into()],
        })
    );
    let mixed = "speaker's close-up at the lectern, ".repeat(5_000);
    let p = parse(&mixed);
    assert!(p.is_valid());
    assert_eq!(p.shots, [Close]);
    let no_spaces = "x".repeat(100_000);
    assert_eq!(parse(&no_spaces).model_phrase.len(), 100_000);
    // One word, "s", behind ten thousand apostrophes.
    let apostrophes = "'".repeat(10_000) + "s close";
    assert_eq!(parse(&apostrophes).shots, [Close]);
}

#[test]
fn odd_characters_never_panic() {
    let lexicon = Lexicon::builtin();
    for text in [
        "\u{0130}stanbul close",
        "caf\u{65}\u{301} close-up",
        "\u{200B}close\u{200B}",
        "speaker\u{2019}\u{2019}s close",
        "'s close",
        "s' close",
        "close-'up",
        "\u{FEFF}speaker upper body",
        "a\u{0}b close",
        "🎸🎸 'guitar' close",
    ] {
        let parsed = parse_prompt(text, &lexicon);
        let literal = literal_prompt(text, &lexicon);
        assert_eq!(
            parsed.model_phrase.is_empty(),
            !parsed.is_valid(),
            "{text:?}"
        );
        assert_eq!(
            literal.model_phrase.is_empty(),
            !literal.is_valid(),
            "{text:?}"
        );
    }
    assert_eq!(parse("\u{0130}stanbul close").subject, "\u{0130}stanbul");
}

#[test]
fn parsing_is_deterministic() {
    let lexicon = Lexicon::builtin();
    for text in ["speaker upper body", "a face", "close-up", "drummer wide"] {
        assert_eq!(parse_prompt(text, &lexicon), parse_prompt(text, &lexicon));
    }
}

#[test]
fn an_empty_lexicon_parses_nothing() {
    let lexicon = Lexicon::default();
    let p = parse_prompt("speaker upper body", &lexicon);
    assert_eq!(p.model_phrase, "speaker upper body");
    assert!(p.shots.is_empty() && p.warnings.is_empty());
    assert_eq!(p.basis, Basis::Object);
}

#[test]
fn lexicon_entries_drive_the_parse() {
    let mut lexicon = Lexicon::builtin();
    lexicon.set_shot(ShotWord {
        phrase: "cowboy".into(),
        sizes: vec![Medium],
        placement: Placement::Anywhere,
        note: None,
        also: None,
    });
    lexicon.set_shot(ShotWord {
        phrase: "close up".into(),
        sizes: vec![MediumClose],
        placement: Placement::AfterSubject,
        note: Some("is a test caveat".into()),
        also: None,
    });
    lexicon.set_role(RoleWord {
        word: "worship leader".into(),
        phrase: "a person singing".into(),
        needs: Some(RoleNeeds::Action),
    });
    let p = parse_prompt("worship leader cowboy", &lexicon);
    assert_eq!(
        (p.model_phrase.as_str(), &p.shots[..]),
        ("a person singing", &[Medium][..])
    );
    assert_eq!(p.basis, Basis::Person);
    assert_eq!(
        p.warnings,
        [PromptWarning::ActionRole {
            role: "worship leader".into(),
            phrase: "a person singing".into(),
        }]
    );
    let p = parse_prompt("guitar close-up", &lexicon);
    assert_eq!(p.shots, [MediumClose]);
    assert_eq!(p.warnings[0].to_string(), "\"close-up\" is a test caveat");
    // Now only after a subject: at the start it is no shot word.
    assert!(parse_prompt("close-up guitar", &lexicon).shots.is_empty());
}

#[test]
fn a_role_override_brings_its_own_tag() {
    let mut lexicon = Lexicon::builtin();
    // No `needs`: the built-in action tag goes with the replaced entry.
    lexicon.set_role(RoleWord {
        word: "Speaker".into(),
        phrase: "a person at a lectern".into(),
        needs: None,
    });
    lexicon.set_role(RoleWord {
        word: "guitarist".into(),
        phrase: "a person singing".into(),
        needs: Some(RoleNeeds::Action),
    });
    let p = parse_prompt("speaker close-up", &lexicon);
    assert_eq!(p.model_phrase, "a person at a lectern");
    assert_eq!(p.warnings, []);
    let p = parse_prompt("guitarist close-up", &lexicon);
    assert_eq!(
        p.warnings,
        [PromptWarning::ActionRole {
            role: "guitarist".into(),
            phrase: "a person singing".into(),
        }]
    );
    // Nor does an untagged role in a lexicon without the built-in words.
    let bare = Lexicon {
        roles: vec![RoleWord {
            word: "pastor".into(),
            phrase: "a person speaking".into(),
            needs: None,
        }],
        ..Lexicon::default()
    };
    let p = parse_prompt("pastor", &bare);
    assert_eq!(
        (p.model_phrase.as_str(), &p.warnings[..]),
        ("a person speaking", &[][..])
    );
}

#[test]
fn malformed_lexicons_never_fail_the_parse() {
    let lexicon = Lexicon {
        shots: vec![
            ShotWord {
                phrase: String::new(),
                sizes: vec![Close],
                placement: Placement::Anywhere,
                note: None,
                also: None,
            },
            ShotWord {
                phrase: "close".into(),
                sizes: vec![ShotSize::Wide],
                placement: Placement::Anywhere,
                note: None,
                also: None,
            },
            ShotWord {
                phrase: "and".into(),
                sizes: vec![Close],
                placement: Placement::Anywhere,
                note: None,
                also: None,
            },
        ],
        roles: vec![RoleWord {
            word: "speaker".into(),
            phrase: String::new(),
            needs: Some(RoleNeeds::Action),
        }],
        person_words: vec!["   ".into()],
    };
    let p = parse_prompt("speaker and close", &lexicon);
    assert_eq!(p.model_phrase, "speaker and close");
    assert!(p.shots.is_empty() && p.is_valid());
    // A skipped role draws no notice.
    let p = parse_prompt("speaker", &lexicon);
    assert_eq!(
        (p.model_phrase.as_str(), &p.warnings[..]),
        ("speaker", &[][..])
    );
}

#[test]
fn summaries_read_as_one_line() {
    assert_eq!(
        parse("guitar").summary(),
        "Detector sees: guitar · Framing: no shot words (object)"
    );
    assert_eq!(
        parse("drummer close-up and full").summary(),
        "Detector sees: a person playing drums · Framing: Close, Full (person)"
    );
    assert!(!parse("").summary().contains('\n'));
}

#[test]
fn errors_are_exactly_empty_and_no_subject() {
    let notices = [
        PromptWarning::ReadAsSubject {
            phrases: vec![],
            after: String::new(),
        },
        PromptWarning::InSubject { phrases: vec![] },
        PromptWarning::AfterSubjectOnly { phrases: vec![] },
        PromptWarning::Note {
            phrase: String::new(),
            note: String::new(),
        },
        PromptWarning::ActionRole {
            role: String::new(),
            phrase: String::new(),
        },
    ];
    assert!(notices.iter().all(|w| !w.is_error()));
    assert!(PromptWarning::Empty.is_error());
    assert!(PromptWarning::NoSubject { phrases: vec![] }.is_error());
    assert_eq!(Basis::Person.label(), "person");
    assert_eq!(Basis::Object.label(), "object");
}
