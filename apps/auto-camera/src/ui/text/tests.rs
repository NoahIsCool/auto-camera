//! Tests for the operator-facing text.

use super::*;
use crate::ui::fixtures::{encoder_stats, preparing_snapshot, recording, snapshot, subject_shot};

#[test]
fn numbers_are_rounded_and_clamped() {
    assert_eq!(percent(0.874), "87%");
    assert_eq!(percent(1.7), "100%");
    assert_eq!(percent(-1.0), "0%");
    assert_eq!(millis(Duration::from_micros(4250)), "4.2 ms");
    assert_eq!(clock(Duration::from_secs(75)), "1:15");
    assert_eq!(clock(Duration::from_secs(3725)), "1:02:05");
    assert_eq!(clock(Duration::ZERO), "0:00");
}

#[test]
fn shot_status_reads_like_a_caption() {
    assert_eq!(
        shot_status(&subject_shot("a", ShotStatus::Tracking, 0.81)),
        "tracking · 81%"
    );
    assert_eq!(
        shot_status(&subject_shot("a", ShotStatus::Lost, 0.0)),
        "lost"
    );
    assert_eq!(
        shot_status(&subject_shot("a", ShotStatus::Unavailable, 0.0)),
        "no signal"
    );
    let mut snap = snapshot();
    assert_eq!(shot_status(&snap.shots[0]), "no signal", "no frames yet");
    snap.shots[0].status = ShotStatus::Wide;
    assert_eq!(shot_status(&snap.shots[0]), "wide");
}

#[test]
fn zoom_limited_shots_are_labelled() {
    let snap = snapshot();
    let mut shot = subject_shot("a", ShotStatus::Tracking, 0.9);
    shot.name = "Person 1 Close".into();
    assert_eq!(shot_title(&shot), "Person 1 Close");
    assert!(!shot_hover(&snap, &shot).contains("limited"));
    shot.limited_by_zoom = true;
    assert_eq!(shot_title(&shot), "Person 1 Close (limited)");
    let hover = shot_hover(&snap, &shot);
    assert!(hover.starts_with("a\nlimited: "), "{hover}");
    assert!(hover.ends_with("click: preview · double-click: cut"));
}

#[test]
fn look_alike_shots_say_which_shot_they_match() {
    let mut snap = snapshot();
    let mut medium = subject_shot("stage/1/medium", ShotStatus::Tracking, 0.9);
    medium.name = "Speaker 1 Medium".into();
    medium.limited_by_zoom = true;
    let mut chest = subject_shot("stage/1/medium_close", ShotStatus::Tracking, 0.9);
    chest.name = "Speaker 1 Medium close".into();
    chest.limited_by_zoom = true;
    chest.same_picture_as = Some(medium.id.clone());
    snap.shots.extend([medium, chest.clone()]);
    let hover = shot_hover(&snap, &chest);
    assert!(
        hover.contains("looks the same as Speaker 1 Medium: both are drawn at the zoom ceiling"),
        "{hover}"
    );
    // A twin that is gone is named by its id.
    chest.same_picture_as = Some(ShotId("stage/1/gone".into()));
    assert!(shot_hover(&snap, &chest).contains("same as stage/1/gone"));
}

#[test]
fn shot_names_fall_back_to_ids_and_dashes() {
    let snap = snapshot();
    let wide = ShotId("stage/wide".into());
    assert_eq!(shot_name(&snap, Some(&wide)), "Wide");
    assert_eq!(
        shot_name(&snap, Some(&ShotId("gone/1/close".into()))),
        "gone/1/close"
    );
    assert_eq!(shot_name(&snap, None), NONE);
    let mut snap = snapshot();
    snap.shots[0].limited_by_zoom = true;
    assert_eq!(shot_name(&snap, Some(&wide)), "Wide (limited)");
}

#[test]
fn status_bar_lines_show_rates_timing_and_drops() {
    let mut snap = snapshot();
    let now = Duration::from_secs(5);
    let src = &mut snap.sources[0];
    assert_eq!(
        source_line(src, now),
        "stage –/30 fps · 0 missed · NO SIGNAL"
    );
    src.frames = 100;
    src.fps = 29.94;
    src.missed = 3;
    src.available = true;
    assert_eq!(source_line(src, now), "stage 29.9/30 fps · 3 missed");
    src.discovery.last_latency = Some(Duration::from_millis(12));
    assert_eq!(
        source_line(src, now),
        "stage 29.9/30 fps · 3 missed · detect 12.0 ms"
    );

    snap.timing.budget_ms = 33.33;
    assert_eq!(timing_line(&snap.timing), "tick – / 33.3 ms");
    snap.timing.tick.count = 10;
    snap.timing.tick.avg_ms = 4.21;
    snap.timing.tick.max_ms = 9.0;
    snap.timing.late_ticks = 1;
    assert_eq!(
        timing_line(&snap.timing),
        "tick 4.2 ms avg · 9.0 max / 33.3 ms · 1 late · 0 skipped"
    );

    snap.timing.skipped_ticks = 4;
    snap.output.stats = Some(encoder_stats());
    assert_eq!(
        dropped_line(&snap),
        "dropped: 3 source · 4 ticks · 2 encoder"
    );
}

#[test]
fn source_line_says_the_detector_is_preparing_and_for_how_long() {
    let mut snap = preparing_snapshot();
    let src = &mut snap.sources[0];
    assert!(src.discovery.preparing, "a fresh core with a detector");
    assert_eq!(
        source_line(src, Duration::ZERO),
        "stage –/30 fps · 0 missed · preparing detector 0:00 · NO SIGNAL"
    );
    src.frames = 900;
    src.fps = 30.0;
    src.available = true;
    assert_eq!(
        source_line(src, Duration::from_millis(125_900)),
        "stage 30.0/30 fps · 0 missed · preparing detector 2:05"
    );
    src.discovery.preparing = false;
    src.discovery.ready_after = Some(Duration::from_secs(126));
    assert_eq!(
        source_line(src, Duration::from_secs(127)),
        "stage 30.0/30 fps · 0 missed"
    );
}

#[test]
fn output_text_covers_every_state_and_stat() {
    assert_eq!(output_state(&OutputState::Running), "ON AIR");
    assert_eq!(output_state(&OutputState::Idle), "idle");
    assert_eq!(
        output_state(&OutputState::Failed("ffmpeg exited".into())),
        "failed: ffmpeg exited"
    );
    let mut stats = encoder_stats();
    let lines = encoder_lines(&stats);
    assert_eq!(lines[0], "encoder libx264");
    assert_eq!(lines[1], "frames 300 written · 2 dropped · 2 repeated");
    assert_eq!(lines[2], "30.0 fps · 4000 kbit/s · speed 1.00x");
    assert_eq!(lines[3], "encoded 0:09");
    assert_eq!(lines.len(), 4);
    stats.progress.fps = None;
    stats.last_error = Some("Connection refused".into());
    stats.audio_connected = true;
    let lines = encoder_lines(&stats);
    assert!(lines[2].starts_with("– fps"));
    assert!(lines[4].starts_with("audio 0 samples"));
    assert_eq!(lines[5], "last error: Connection refused");

    assert_eq!(destinations(&OutputView::default()), NONE);
    let mut out = recording("/tmp/show.mp4");
    assert_eq!(destinations(&out), "/tmp/show.mp4");
    out.rtmp_url = Some("rtmp://example.com/app/<key>".into());
    assert_eq!(
        destinations(&out),
        "rtmp://example.com/app/<key> + /tmp/show.mp4"
    );
}

/// A live query for `prompt`, read with the built-in lexicon, framed by
/// `sizes` (the shot words' sizes, or medium and close, when `None`).
fn query(prompt: &str, literal: bool, sizes: Option<&[ac_core::ShotSize]>) -> QueryView {
    let lexicon = ac_core::Lexicon::builtin();
    let parse = ac_engine::read_prompt(prompt, literal, &lexicon);
    let explicit = sizes.map(ac_engine::shot_presets);
    QueryView {
        id: ac_core::QueryId("q".into()),
        name: "Q".into(),
        prompt: prompt.into(),
        presets: ac_engine::queries::settle_presets(explicit, &parse),
        parse,
        matches: Vec::new(),
    }
}

/// A pass on `source` that found `count` boxes, the best at `best`, all
/// above the 0.35 floor.
fn found(source: &str, count: usize, best: Option<f32>) -> MatchView {
    MatchView {
        source: ac_core::SourceId(source.into()),
        seq: 41,
        pts: Duration::from_secs(75),
        count,
        best,
        kept: count,
        floor: 0.35,
    }
}

#[test]
fn prompt_feedback_counts_matches_and_rounds_the_best_score() {
    assert_eq!(
        match_text(&found("s", 3, Some(0.7149))),
        "3 matches · best 0.71"
    );
    assert_eq!(
        match_text(&found("s", 1, Some(0.42))),
        "1 match · best 0.42"
    );
    assert_eq!(match_text(&found("s", 0, None)), NO_MATCHES);
    // A count without a score cannot happen; it reads as nothing found.
    assert_eq!(match_text(&found("s", 2, None)), NO_MATCHES);
}

#[test]
fn prompt_feedback_says_how_many_boxes_fall_below_the_store_floor() {
    let below = MatchView {
        kept: 0,
        floor: 0.5,
        ..found("s", 1, Some(0.4))
    };
    assert_eq!(
        match_text(&below),
        "1 match below the 0.50 floor · best 0.40"
    );
    let mixed = MatchView {
        kept: 2,
        floor: 0.5,
        ..found("s", 3, Some(0.71))
    };
    assert_eq!(
        match_text(&mixed),
        "2 matches · best 0.71 · 1 below the 0.50 floor"
    );
    let mut snap = snapshot();
    snap.sources[0].discovery_enabled = true;
    let mut q = snap.queries[0].clone();
    q.matches = vec![MatchView {
        source: snap.sources[0].id.clone(),
        ..below
    }];
    assert_eq!(match_lines(&snap, &q)[0].1, Tone::Warning, "nothing framed");
    q.matches[0] = MatchView {
        source: snap.sources[0].id.clone(),
        ..mixed
    };
    assert_eq!(match_lines(&snap, &q)[0].1, Tone::Plain);
}

#[test]
fn a_failing_detector_replaces_the_old_feedback_with_its_reason() {
    use ac_engine::PassOutcome;
    let mut snap = snapshot();
    snap.sources[0].discovery_enabled = true;
    let mut q = snap.queries[0].clone();
    // The startup failed before any pass: not "no detector pass yet".
    let d = &mut snap.sources[0].discovery;
    d.errors = 4;
    d.last_error = Some("detector unavailable: the sidecar exited before hello".into());
    d.last_outcome = Some(PassOutcome::Failed);
    let lines = match_lines(&snap, &q);
    assert_eq!(lines[0].0, DETECTOR_ERROR);
    assert_eq!(lines[0].1, Tone::Warning);
    assert!(lines[0].2.contains("exited before hello"), "{}", lines[0].2);
    assert!(
        lines[0].2.contains("no pass has been applied"),
        "{}",
        lines[0].2
    );
    // Mid-show: the old numbers move to the hover text.
    q.matches = vec![found("stage", 3, Some(0.71))];
    let lines = match_lines(&snap, &q);
    assert_eq!(lines[0].0, DETECTOR_ERROR);
    assert!(
        lines[0]
            .2
            .contains("last applied pass found 3 matches · best 0.71 on frame 41 at 1:15"),
        "{}",
        lines[0].2
    );
    let d = &mut snap.sources[0].discovery;
    d.stale = 5;
    d.last_outcome = Some(PassOutcome::Stale);
    assert_eq!(
        match_lines(&snap, &q)[0].0,
        "detector answers too old (5 dropped)"
    );
    let d = &mut snap.sources[0].discovery;
    d.timeouts = 1;
    d.last_outcome = Some(PassOutcome::TimedOut);
    assert_eq!(
        match_lines(&snap, &q)[0].0,
        "detector not answering (1 timed out)"
    );
    snap.sources[0].discovery.last_outcome = Some(PassOutcome::Applied);
    assert_eq!(match_lines(&snap, &q)[0].0, "3 matches · best 0.71");
}

#[test]
fn the_status_bar_counts_detector_failures_and_warns() {
    use ac_engine::PassOutcome;
    let mut snap = snapshot();
    let s = &mut snap.sources[0];
    s.discovery_enabled = true;
    let plain = source_line(s, Duration::ZERO);
    assert!(!plain.contains("detect "), "{plain}");
    assert_eq!(source_tone(s), Tone::Plain);
    assert_eq!(source_hover(s), "");
    s.discovery.last_latency = Some(Duration::from_millis(640));
    s.discovery.errors = 3;
    s.discovery.stale = 2;
    s.discovery.timeouts = 1;
    s.discovery.last_error = Some("the sidecar is unavailable: crashed".into());
    s.discovery.last_outcome = Some(PassOutcome::Failed);
    let line = source_line(s, Duration::ZERO);
    assert!(
        line.ends_with("detect 640.0 ms · 3 detector errors · 2 too old · 1 timed out · NO SIGNAL"),
        "{line}"
    );
    assert_eq!(source_tone(s), Tone::Warning);
    assert_eq!(
        source_hover(s),
        "newest detector error: the sidecar is unavailable: crashed"
    );
}

#[test]
fn prompt_feedback_lines_name_sources_only_when_there_are_several() {
    let mut snap = snapshot();
    let mut q = snap.queries[0].clone();
    assert!(q.matches.is_empty());
    assert!(match_lines(&snap, &q).is_empty(), "discovery is off");
    snap.sources[0].discovery_enabled = true;
    assert_eq!(
        match_lines(&snap, &q),
        [(NO_PASS_YET.to_string(), Tone::Weak, String::new())]
    );
    q.matches = vec![found("stage", 2, Some(0.9))];
    let lines = match_lines(&snap, &q);
    assert_eq!(lines[0].0, "2 matches · best 0.90");
    assert_eq!(lines[0].1, Tone::Plain);
    assert_eq!(
        lines[0].2,
        "newest detector pass on stage: frame 41 at 1:15"
    );
    let mut second = snap.sources[0].clone();
    second.id = ac_core::SourceId("pulpit".into());
    second.name = "Pulpit".into();
    snap.sources.push(second);
    // Neither source has a pass yet: one line, not one per source.
    let empty = QueryView {
        matches: Vec::new(),
        ..q.clone()
    };
    assert_eq!(
        match_lines(&snap, &empty),
        [(NO_PASS_YET.to_string(), Tone::Weak, String::new())]
    );
    q.matches.push(found("pulpit", 0, None));
    let lines = match_lines(&snap, &q);
    let texts: Vec<(&str, Tone)> = lines.iter().map(|l| (l.0.as_str(), l.1)).collect();
    assert_eq!(
        texts,
        [
            ("stage: 2 matches · best 0.90", Tone::Plain),
            ("Pulpit: no matches", Tone::Warning),
        ]
    );
}

#[test]
fn the_parse_line_says_what_the_detector_sees_and_the_framing() {
    let lexicon = ac_core::Lexicon::builtin();
    let parse = |text: &str| ac_core::parse_prompt(text, &lexicon);
    assert_eq!(
        parse_line(&parse("speaker upper body")).as_deref(),
        Some("Detector sees: a person speaking · Framing: Medium, Medium close (person)")
    );
    assert_eq!(
        parse_line(&parse("guitar")).as_deref(),
        Some("Detector sees: guitar · Framing: no shot words (object)")
    );
    // Nothing typed yet: no line; shot words only: the reason.
    assert_eq!(parse_line(&parse("  ")), None);
    let line = parse_line(&parse("close-up")).unwrap();
    assert!(line.contains("only names a shot size"), "{line}");
    let literal = ac_core::literal_prompt("speaker upper body", &lexicon);
    assert!(
        parse_line(&literal)
            .unwrap()
            .contains("speaker upper body (literal)")
    );
}

#[test]
fn parse_notices_are_listed_but_errors_are_not() {
    let lexicon = ac_core::Lexicon::builtin();
    let notes = parse_notes(&ac_core::parse_prompt("drummer wide", &lexicon));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with("Note: \"wide\"") && notes[0].ends_with('.'),
        "{notes:?}"
    );
    assert!(parse_notes(&ac_core::parse_prompt("close-up", &lexicon)).is_empty());
    assert!(parse_notes(&ac_core::parse_prompt("guitar", &lexicon)).is_empty());
}

#[test]
fn an_action_role_is_noted_in_the_form_and_the_query_hover() {
    let lexicon = ac_core::Lexicon::builtin();
    let notes = parse_notes(&ac_core::parse_prompt("speaker upper body", &lexicon));
    assert_eq!(
        notes,
        [
            "Note: \"speaker\" asks the detector for \"a person speaking\", an action: OWLv2 sees \
          people and objects, not actions, so every person (photos on the screen included) gets \
          this query's shots; take the speaker's shot by hand."
        ]
    );
    let hover = query_hover(&query("speaker upper body", false, None));
    assert_eq!(
        hover.lines().collect::<Vec<_>>(),
        [
            "Detector sees: a person speaking · Framing: Medium, Medium close (person)",
            notes[0].as_str(),
        ]
    );
    // An object role draws none.
    assert!(parse_notes(&ac_core::parse_prompt("guitarist close-up", &lexicon)).is_empty());
    assert!(!query_hover(&query("guitarist close-up", false, None)).contains("Note"));
}

#[test]
fn live_queries_show_what_the_detector_is_asked_for() {
    use ac_core::ShotSize::{Close, Full};
    assert_eq!(
        query_line(&query("speaker upper body", false, None)),
        "“speaker upper body” → a person speaking · Medium, Medium close (person)"
    );
    assert_eq!(
        query_line(&query("guitar", false, None)),
        "“guitar” · Medium, Close (object)"
    );
    // Taken literally the head noun is "body": an object, as typed.
    assert_eq!(
        query_line(&query("speaker upper body", true, None)),
        "“speaker upper body” (literal) · Medium, Close (object)"
    );
    let overridden = query("guitarist close-up", false, Some(&[Full]));
    assert!(overridden.shot_words_overridden());
    let hover = query_hover(&overridden);
    assert!(
        hover.starts_with("Detector sees: a person playing guitar"),
        "{hover}"
    );
    assert!(
        hover.contains("override the prompt's shot words"),
        "{hover}"
    );
    let agreed = query("guitarist close-up", false, Some(&[Close]));
    assert!(!query_hover(&agreed).contains("override"));
    // The form sends sizes widest first; that is not an override.
    let reordered = query("guitarist close and full", false, Some(&[Full, Close]));
    assert!(!reordered.shot_words_overridden());
    assert!(query_hover(&query("drummer wide", false, None)).contains("\nNote: \"wide\""));
}
