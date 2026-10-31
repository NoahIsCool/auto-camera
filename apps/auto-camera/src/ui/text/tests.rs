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
fn source_line_says_why_a_source_is_off_the_air() {
    let mut snap = snapshot();
    let now = Duration::ZERO;
    let src = &mut snap.sources[0];
    // A file missing at start: no rate known yet, retried, and why.
    src.nominal_fps = 0.0;
    src.state = SourceState::Offline;
    src.error = Some("cannot open source: cannot open later.mkv".into());
    assert_eq!(
        source_line(src, now),
        "stage –/– fps · 0 missed · OFFLINE: cannot open source: cannot open later.mkv"
    );
    assert_eq!(source_tone(src), Tone::Warning);
    src.state = SourceState::Reconnecting;
    assert_eq!(
        source_line(src, now),
        "stage –/– fps · 0 missed · RECONNECTING: cannot open source: cannot open later.mkv"
    );
    assert_eq!(source_tone(src), Tone::Warning);
    // Retried without a reason given, and stopped by an error.
    src.error = None;
    assert!(source_line(src, now).ends_with(" · RECONNECTING"));
    src.state = SourceState::Stopped;
    src.error = Some("the source supervisor panicked".into());
    assert!(
        source_line(src, now).ends_with(" · NO SIGNAL: the source supervisor panicked"),
        "{}",
        source_line(src, now)
    );
    assert_eq!(source_tone(src), Tone::Warning);
    src.error = None;
    assert!(source_line(src, now).ends_with(" · NO SIGNAL"));
    assert_eq!(source_tone(src), Tone::Plain, "it ended");
    // Back on air: its rate is known and no reason is shown.
    src.state = SourceState::Running;
    src.error = None;
    src.available = true;
    src.frames = 30;
    src.fps = 30.0;
    src.nominal_fps = 30.0;
    assert_eq!(source_line(src, now), "stage 30.0/30 fps · 0 missed");
    assert_eq!(source_tone(src), Tone::Plain);
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

#[test]
fn the_stream_line_names_the_state_bitrate_and_outages() {
    let mut s = StreamHealth {
        state: StreamState::Connected,
        stalled: false,
        outages: 0,
        reconnects: 0,
        bitrate_kbps: Some(811.6),
    };
    assert_eq!(stream_line(&s), "stream connected · 812 kbit/s");
    assert_eq!(stream_tone(&s), Tone::Plain);
    s.bitrate_kbps = None;
    assert_eq!(stream_line(&s), "stream connected · – kbit/s");

    s.state = StreamState::Reconnecting { attempt: 3 };
    s.outages = 1;
    assert_eq!(
        stream_line(&s),
        "stream reconnecting (attempt 3) · 1 outage"
    );
    assert_eq!(stream_tone(&s), Tone::Warning);
    s.state = StreamState::Connected;
    s.stalled = true;
    s.outages = 2;
    assert_eq!(
        stream_line(&s),
        "stream connected · – kbit/s · stalled, dropping packets · 2 outages"
    );
    assert_eq!(stream_tone(&s), Tone::Warning, "a stall is a warning");

    s = StreamHealth {
        state: StreamState::Connecting,
        ..s
    };
    assert_eq!(
        stream_line(&s),
        "stream connecting… · no answer · 2 outages"
    );
    s.stalled = false;
    assert_eq!(stream_tone(&s), Tone::Weak);
    s.state = StreamState::Down;
    assert_eq!(stream_tone(&s), Tone::Warning);
}

#[test]
fn the_stream_line_shows_only_while_a_streaming_session_is_live() {
    let mut out = OutputView {
        state: OutputState::Running,
        ..recording("/tmp/show.mp4")
    };
    assert_eq!(stream_status(&out), None, "no stats yet");
    out.stats = Some(encoder_stats());
    assert_eq!(stream_status(&out), None, "this session only records");
    let mut stats = encoder_stats();
    stats.stream = Some(StreamHealth {
        state: StreamState::Reconnecting { attempt: 1 },
        stalled: false,
        outages: 1,
        reconnects: 0,
        bitrate_kbps: Some(0.0),
    });
    out.stats = Some(stats);
    let line = "stream reconnecting (attempt 1) · 1 outage".to_owned();
    assert_eq!(stream_status(&out), Some((line.clone(), Tone::Warning)));
    out.state = OutputState::Stopping;
    assert_eq!(stream_status(&out), Some((line, Tone::Warning)));
    for state in [OutputState::Idle, OutputState::Finished] {
        out.state = state;
        assert_eq!(stream_status(&out), None, "ffmpeg is gone");
    }
}

/// A live query for `prompt`, read with the built-in lexicon for a
/// detector that does not see actions (OWLv2), framed by `sizes` (the shot
/// words' sizes, or medium and close, when `None`).
fn query(prompt: &str, literal: bool, sizes: Option<&[ac_core::ShotSize]>) -> QueryView {
    query_for(&reader(false), prompt, literal, sizes)
}

/// The built-in lexicon, for a detector that does or does not see actions.
fn reader(sees_actions: bool) -> ac_engine::PromptReader {
    ac_engine::PromptReader::new(ac_core::Lexicon::builtin(), sees_actions)
}

/// [`query`], read with `reader`.
fn query_for(
    reader: &ac_engine::PromptReader,
    prompt: &str,
    literal: bool,
    sizes: Option<&[ac_core::ShotSize]>,
) -> QueryView {
    let parse = ac_engine::read_prompt(prompt, literal, reader);
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
    let line = match_lines(&snap, &q).remove(0);
    assert_eq!(line.0, "detector not answering (1 timed out)");
    // Only a shared detector process is blamed on a restart.
    assert!(!snap.shared_detector);
    assert!(line.2.contains("slower than that, or stuck"), "{}", line.2);
    assert!(!line.2.contains("sidecar"), "{}", line.2);
    snap.shared_detector = true;
    let hover = match_lines(&snap, &q).remove(0).2;
    assert!(hover.contains("restarted sidecar"), "{hover}");
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
fn a_preparing_detector_reads_as_preparing_not_as_not_answering() {
    use ac_engine::PassOutcome;
    let mut snap = preparing_snapshot();
    let q = snap.queries[0].clone();
    assert!(snap.sources[0].discovery_enabled && snap.sources[0].discovery.preparing);
    let preparing = (
        PREPARING.to_string(),
        Tone::Weak,
        "loading the model; the window and program keep running".to_string(),
    );
    assert_eq!(match_lines(&snap, &q), std::slice::from_ref(&preparing));
    assert_eq!(source_tone(&snap.sources[0]), Tone::Plain, "not amber");
    // Preparing wins over a timeout on record, which is what a load used
    // to look like.
    let mut timed_out = snap.clone();
    let d = &mut timed_out.sources[0].discovery;
    d.timeouts = 3;
    d.last_outcome = Some(PassOutcome::TimedOut);
    assert_eq!(
        match_lines(&timed_out, &q),
        std::slice::from_ref(&preparing)
    );
    // A shared detector starting: every source prepares, and one line
    // says so.
    let mut second = snap.sources[0].clone();
    second.id = ac_core::SourceId("pulpit".into());
    second.name = "Pulpit".into();
    snap.sources.push(second);
    assert_eq!(match_lines(&snap, &q), [preparing]);
    // Detectors of their own load apart: each source says where it is.
    snap.sources[0].discovery.preparing = false;
    let lines = match_lines(&snap, &q);
    let texts: Vec<(&str, Tone)> = lines.iter().map(|l| (l.0.as_str(), l.1)).collect();
    assert_eq!(
        texts,
        [
            ("stage: no detector pass yet", Tone::Weak),
            ("Pulpit: preparing the detector", Tone::Weak),
        ]
    );
    // Once it is ready, a request that gets no answer is amber again.
    let d = &mut snap.sources[1].discovery;
    d.preparing = false;
    d.timeouts = 1;
    d.last_outcome = Some(PassOutcome::TimedOut);
    let lines = match_lines(&snap, &q);
    assert_eq!(lines[1].0, "Pulpit: detector not answering (1 timed out)");
    assert_eq!(lines[1].1, Tone::Warning);
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
fn an_action_role_draws_no_note_on_a_detector_that_sees_actions() {
    // The form reads its preview, and the engine its queries, through the
    // studio's reader: OWLv2's notes the action, the sidecar's does not.
    let read = |sees_actions| {
        parse_notes(&ac_engine::read_prompt(
            "speaker upper body",
            false,
            &reader(sees_actions),
        ))
    };
    assert_eq!(read(false).len(), 1);
    assert!(read(true).is_empty(), "{:?}", read(true));
    let hover = query_hover(&query_for(&reader(true), "speaker upper body", false, None));
    assert_eq!(
        hover,
        "Detector sees: a person speaking · Framing: Medium, Medium close (person)"
    );
    // Other notes stay.
    let notes = parse_notes(&ac_engine::read_prompt("singer wide", false, &reader(true)));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("Note: \"wide\""), "{notes:?}");
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

/// The program input's line in `snap`, without its tone.
fn line(snap: &EngineSnapshot) -> Option<String> {
    drift_line(snap).map(|(line, _)| line)
}

/// A board as program audio, running with `correction_ppm`.
fn board(correction_ppm: f64) -> ac_engine::AudioInputView {
    ac_engine::AudioInputView {
        id: ac_core::SourceId("board".into()),
        name: "Soundboard".into(),
        state: SourceState::Running,
        error: None,
        format: Some((48_000, 2)),
        pick: None,
        program: true,
        blocks: 100,
        dropped: 0,
        unusable: 0,
        unheard: false,
        correction_ppm,
        drift_resets: 0,
        drift_steps: 0,
    }
}

#[test]
fn the_program_inputs_drift_correction_is_shown() {
    let mut snap = snapshot();
    assert_eq!(line(&snap), None, "no audio input");
    let listed = ac_engine::AudioInputView {
        program: false,
        ..board(-37.26)
    };
    snap.audio_inputs = vec![listed.clone()];
    assert_eq!(line(&snap), None, "not program audio");
    snap.audio_inputs[0].program = true;
    assert_eq!(
        drift_line(&snap),
        Some(("Soundboard drift correction -37.3 ppm".into(), Tone::Plain))
    );
    snap.audio_inputs[0].drift_resets = 1;
    assert!(line(&snap).unwrap().ends_with(" · 1 reset"));
    snap.audio_inputs[0].drift_steps = 1;
    assert!(
        line(&snap)
            .unwrap()
            .ends_with(" · 1 reset · 1 step made up")
    );
    snap.audio_inputs[0] = ac_engine::AudioInputView {
        correction_ppm: 0.0,
        drift_resets: 3,
        drift_steps: 2,
        ..listed
    };
    snap.audio_inputs[0].program = true;
    assert_eq!(
        line(&snap).as_deref(),
        Some("Soundboard drift correction 0.0 ppm · 3 resets · 2 steps made up")
    );
    snap.audio_inputs[0].drift_resets = 0;
    assert_eq!(
        line(&snap).as_deref(),
        Some("Soundboard drift correction 0.0 ppm · 2 steps made up")
    );
}

#[test]
fn a_program_input_that_is_not_running_shows_why_not_a_stale_correction() {
    // Unplugged mid-show: the correction it had is not being applied, so
    // the line says the board is offline and why, in amber.
    let mut snap = snapshot();
    let gone = "audio device \"SQ\" went away: Device disconnected";
    snap.audio_inputs = vec![ac_engine::AudioInputView {
        state: SourceState::Offline,
        error: Some(gone.into()),
        ..board(-101.1)
    }];
    let expected = format!("Soundboard offline: {gone}");
    assert_eq!(drift_line(&snap), Some((expected, Tone::Warning)));
    // Being opened again, with the reason it went.
    snap.audio_inputs[0].state = SourceState::Reconnecting;
    let (text, tone) = drift_line(&snap).unwrap();
    assert!(
        text.starts_with("Soundboard reconnecting: audio device"),
        "{text}"
    );
    assert_eq!(tone, Tone::Warning);
    // Never connected (not plugged in at start, or a build without the
    // device backend): no "0.0 ppm" as if its blocks passed as they are.
    let missing = "no audio device with UID \"AppleUSBAudioEngine:SQ\"";
    snap.audio_inputs[0] = ac_engine::AudioInputView {
        state: SourceState::Offline,
        error: Some(missing.into()),
        format: None,
        blocks: 0,
        ..board(0.0)
    };
    let (text, tone) = drift_line(&snap).unwrap();
    assert_eq!(text, format!("Soundboard offline: {missing}"));
    assert_eq!(tone, Tone::Warning);
    // Starting is weak; stopped (a recording that ended) is amber.
    snap.audio_inputs[0] = ac_engine::AudioInputView {
        state: SourceState::Starting,
        ..board(0.0)
    };
    let starting = Some(("Soundboard starting".to_owned(), Tone::Weak));
    assert_eq!(drift_line(&snap), starting);
    snap.audio_inputs[0].state = SourceState::Stopped;
    let stopped = Some(("Soundboard stopped".to_owned(), Tone::Warning));
    assert_eq!(drift_line(&snap), stopped);
    // Not running, the error is the drift line's alone.
    assert_eq!(drift_notice(&snap), None);
    // Running with an error (a discontinuity it delivered through): the
    // correction, still applied, and the error on a line of its own.
    snap.audio_inputs[0].state = SourceState::Running;
    snap.audio_inputs[0].error = Some("audio discontinuity (1 so far)".into());
    let running = "Soundboard drift correction 0.0 ppm".to_owned();
    assert_eq!(drift_line(&snap), Some((running, Tone::Plain)));
    let notice = drift_notice(&snap);
    assert_eq!(notice.as_deref(), Some("audio discontinuity (1 so far)"));
}

#[test]
fn a_running_inputs_notice_leaves_its_correction_and_steps_in_view() {
    // One harmless timestamp glitch, or a loss the correction made up, is
    // kept as the board's error for the rest of the connection: the drift
    // line stays plain with the correction and its steps, and the notice
    // goes on a weak line under it.
    let mut snap = snapshot();
    let glitch = "audio discontinuity (1 so far): host time of 1 block(s) from block 10 was up \
                  to 30.0 ms off, then came back: no audio missing";
    let gap = "audio discontinuity (1 so far; the device reported 1 overload): 10.7 ms of \
               audio missing before block 1406 (its host time jumped ahead)";
    for notice in [glitch, gap] {
        snap.audio_inputs = vec![ac_engine::AudioInputView {
            error: Some(notice.into()),
            drift_steps: 1,
            ..board(-37.26)
        }];
        assert_eq!(
            drift_line(&snap),
            Some((
                "Soundboard drift correction -37.3 ppm · 1 step made up".into(),
                Tone::Plain
            ))
        );
        assert_eq!(drift_notice(&snap).as_deref(), Some(notice));
    }
    // No notice, no line for it.
    snap.audio_inputs[0].error = None;
    assert_eq!(drift_notice(&snap), None);
    // Not program audio: neither line.
    snap.audio_inputs[0].error = Some(glitch.into());
    snap.audio_inputs[0].program = false;
    assert_eq!((drift_line(&snap), drift_notice(&snap)), (None, None));
}

#[test]
fn a_program_input_not_heard_for_its_rate_shows_why_alone() {
    // Plugged in at 96 kHz after the output started on the 48 kHz stand-in:
    // it runs, but the pacer drops its blocks, so its correction would
    // mislead. Its error names it, so the line is the error alone, amber.
    let mut snap = snapshot();
    let why = ac_engine::program_audio::unheard("Soundboard", 96_000, 48_000);
    snap.audio_inputs = vec![ac_engine::AudioInputView {
        error: Some(why.clone()),
        format: Some((96_000, 2)),
        unheard: true,
        drift_steps: 1,
        ..board(-12.0)
    }];
    assert_eq!(drift_line(&snap), Some((why, Tone::Warning)));
    assert_eq!(drift_notice(&snap), None, "the error is the drift line's");
    // Without the error (a view built by hand) it still says so.
    snap.audio_inputs[0].error = None;
    let bare = "Soundboard is not heard at its rate".to_owned();
    assert_eq!(drift_line(&snap), Some((bare, Tone::Warning)));
    // Heard again after a restart: the correction is back.
    snap.audio_inputs[0].unheard = false;
    let (text, tone) = drift_line(&snap).unwrap();
    assert_eq!(
        text,
        "Soundboard drift correction -12.0 ppm · 1 step made up"
    );
    assert_eq!(tone, Tone::Plain);
}
