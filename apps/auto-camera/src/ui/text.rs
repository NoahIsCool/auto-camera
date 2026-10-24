//! Operator-facing text, built from snapshot values.
//!
//! Every label the window shows that is more than a constant is built here,
//! as plain functions of snapshot data, so wording and rounding are tested
//! without a window. Units are always spelled out (`fps`, `ms`, `kbit/s`),
//! and absent values read as an en dash rather than zero, since "0 fps"
//! and "no measurement yet" mean different things during a show.

use std::time::Duration;

use ac_core::{ParsedPrompt, PromptWarning, ShotId};
use ac_engine::{
    EngineSnapshot, MatchView, OutputState, OutputView, PassOutcome, QueryView, ShotStatus,
    ShotView, SourceView, TimingView,
};
use ac_output::EncoderStats;

/// Shown for a value that is not known.
pub const NONE: &str = "–";

/// `fraction` in `0..=1` as a whole percentage.
pub fn percent(fraction: f64) -> String {
    format!("{:.0}%", (fraction.clamp(0.0, 1.0) * 100.0))
}

/// A duration in milliseconds with one decimal.
pub fn millis(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

/// A duration as `m:ss`, or `h:mm:ss` from one hour.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Marks a shot whose preset's zoom ceiling keeps it looser than its size.
pub const LIMITED: &str = "(limited)";

/// The multiview tile's badge for such a shot. It is drawn in the corner of
/// the picture, apart from the caption, so a long name never pushes it out
/// of a narrow tile.
pub const LIMITED_BADGE: &str = "LIMITED";

/// A shot's name marked [`LIMITED`] when the zoom ceiling keeps it from its
/// size, as for "Person 1 Close (limited)" on a distant speaker, so the
/// operator never takes it for a true close-up. The monitors and the
/// director's suggestion show it; a multiview tile shows the plain name and
/// [`LIMITED_BADGE`].
pub fn shot_title(shot: &ShotView) -> String {
    if shot.limited_by_zoom {
        format!("{} {LIMITED}", shot.name)
    } else {
        shot.name.clone()
    }
}

/// A shot tile's hover text: its id, why it is limited if it is, which of
/// its subject's shots it looks the same as, and what clicks do.
pub fn shot_hover(snap: &EngineSnapshot, shot: &ShotView) -> String {
    let mut text = shot.id.0.clone();
    if shot.limited_by_zoom {
        text.push_str("\nlimited: the preset's max zoom stops this shot reaching its size");
    }
    if let Some(twin) = &shot.same_picture_as {
        let name = snap.shot(twin).map_or(twin.0.as_str(), |s| s.name.as_str());
        text.push_str(&format!(
            "\nlooks the same as {name}: both are drawn at the zoom ceiling"
        ));
    }
    text.push_str("\nclick: preview · double-click: cut");
    text
}

/// A shot's health and quality, as on its multiview caption.
pub fn shot_status(shot: &ShotView) -> String {
    match shot.status {
        ShotStatus::Wide => "wide".into(),
        ShotStatus::Tracking => format!("tracking · {}", percent(f64::from(shot.quality))),
        ShotStatus::Lost => "lost".into(),
        ShotStatus::Unavailable => "no signal".into(),
    }
}

/// The name of `shot` as [`shot_title`] gives it (so the monitors and the
/// director's suggestion say "(limited)" too), its id when it is gone, or
/// [`NONE`].
pub fn shot_name(snap: &EngineSnapshot, shot: Option<&ShotId>) -> String {
    match shot {
        Some(id) => snap.shot(id).map_or_else(|| id.0.clone(), shot_title),
        None => NONE.into(),
    }
}

/// The query form's line under the prompt: what the detector will see and
/// the framings the shot words ask for (`Detector sees: a person speaking ·
/// Framing: Medium, Medium close (person)`), or why the prompt cannot be
/// used. Nothing while the prompt is empty.
pub fn parse_line(parse: &ParsedPrompt) -> Option<String> {
    match parse.error() {
        Some(PromptWarning::Empty) => None,
        _ => Some(parse.summary()),
    }
}

/// The parse's notices (not its errors), one sentence each.
pub fn parse_notes(parse: &ParsedPrompt) -> Vec<String> {
    parse
        .warnings
        .iter()
        .filter(|w| !w.is_error())
        .map(|w| format!("Note: {w}."))
        .collect()
}

/// A live query's caption: its prompt, what the detector is asked for when
/// that differs, its framings and basis, as in `“speaker upper body” → a
/// person speaking · Medium, Medium close (person)`.
pub fn query_line(q: &QueryView) -> String {
    let mut line = format!("“{}”", q.prompt);
    if q.parse.literal {
        line.push_str(" (literal)");
    } else if q.parse.model_phrase != q.prompt {
        line.push_str(&format!(" → {}", q.parse.model_phrase));
    }
    let sizes: Vec<&str> = q.presets.iter().map(|p| p.name.as_str()).collect();
    line.push_str(&format!(
        " · {} ({})",
        sizes.join(", "),
        q.parse.basis.label()
    ));
    line
}

/// A live query's hover text: the parse line, whether presets override the
/// shot words, and the parse's notices.
pub fn query_hover(q: &QueryView) -> String {
    let mut lines = vec![q.parse.summary()];
    if q.shot_words_overridden() {
        lines.push("The query's presets override the prompt's shot words.".into());
    }
    lines.extend(parse_notes(&q.parse));
    lines.join("\n")
}

/// Prompt feedback for a pass that found nothing.
pub const NO_MATCHES: &str = "no matches";

/// Prompt feedback for a query no applied pass has looked for yet.
pub const NO_PASS_YET: &str = "no detector pass yet";

/// Prompt feedback for a source whose newest detector pass failed.
pub const DETECTOR_ERROR: &str = "detector error";

/// Prompt feedback for a source whose newest answer came too late to use.
pub const TOO_OLD: &str = "detector answers too old";

/// Prompt feedback for a source whose newest request got no answer.
pub const NOT_ANSWERING: &str = "detector not answering";

/// How a prompt feedback line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Matches were found.
    Plain,
    /// Nothing usable was found, or the detector is failing (amber).
    Warning,
    /// No pass has looked for the query yet.
    Weak,
}

/// What one pass found for a query (spec M2): "3 matches · best 0.71",
/// "1 match · best 0.42", [`NO_MATCHES`], and, when some boxes score below
/// the subject store's floor (they are never framed), how many: "1 match
/// below the 0.50 floor · best 0.40", or "3 matches · best 0.71 · 1 below
/// the 0.50 floor".
pub fn match_text(m: &MatchView) -> String {
    let Some(best) = m.best.filter(|_| m.count > 0) else {
        return NO_MATCHES.into();
    };
    let below = m.count.saturating_sub(m.kept);
    let plural = |n: usize| if n == 1 { "match" } else { "matches" };
    if m.kept == 0 {
        return format!(
            "{below} {} below the {:.2} floor · best {best:.2}",
            plural(below),
            m.floor
        );
    }
    let mut text = format!("{} {} · best {best:.2}", m.kept, plural(m.kept));
    if below > 0 {
        text.push_str(&format!(" · {below} below the {:.2} floor", m.floor));
    }
    text
}

/// How `m` is drawn: amber when nothing it found can be framed.
fn match_tone(m: &MatchView) -> Tone {
    if m.kept == 0 {
        Tone::Warning
    } else {
        Tone::Plain
    }
}

/// "the last applied pass found 3 matches · best 0.71 on frame 41 at
/// 1:15", or that there was none.
fn last_pass(m: Option<&MatchView>) -> String {
    match m {
        Some(m) => format!(
            "the last applied pass found {} on frame {} at {}",
            match_text(m),
            m.seq,
            clock(m.pts)
        ),
        None => "no pass has been applied for this query yet".into(),
    }
}

/// One source's feedback for a query, with its tone and hover text. When
/// the source's newest request failed, came back too late or got no
/// answer, that is what the line says (amber, with the reason on hover),
/// since the last applied pass's numbers would look current.
fn source_feedback(source: &SourceView, m: Option<&MatchView>) -> (String, Tone, String) {
    let d = &source.discovery;
    let failing = match d.last_outcome {
        Some(PassOutcome::Failed) => Some((
            DETECTOR_ERROR.to_owned(),
            d.last_error.clone().unwrap_or_default(),
        )),
        Some(PassOutcome::Stale) => Some((
            format!("{TOO_OLD} ({} dropped)", d.stale),
            "answers arrived more than discovery.max_result_age_s after their frame, \
             so they were dropped"
                .to_owned(),
        )),
        Some(PassOutcome::TimedOut) => Some((
            format!("{NOT_ANSWERING} ({} timed out)", d.timeouts),
            "no answer within discovery.timeout_s (a model may still be loading)".to_owned(),
        )),
        Some(PassOutcome::Applied) | None => None,
    };
    if let Some((text, why)) = failing {
        return (text, Tone::Warning, format!("{why}\n{}", last_pass(m)));
    }
    match m {
        Some(m) => (
            match_text(m),
            match_tone(m),
            format!(
                "newest detector pass on {}: frame {} at {}",
                source.name,
                m.seq,
                clock(m.pts)
            ),
        ),
        None => (NO_PASS_YET.into(), Tone::Weak, String::new()),
    }
}

/// A query row's prompt feedback: one line per source that runs discovery,
/// named after the source when the studio has several, with how to draw
/// it and its hover text (the frame the pass looked at, or why the
/// detector is failing there). When no source has anything to say yet, a
/// single [`NO_PASS_YET`] line; when discovery is off everywhere, nothing.
pub fn match_lines(snap: &EngineSnapshot, q: &QueryView) -> Vec<(String, Tone, String)> {
    let several = snap.sources.len() > 1;
    let lines: Vec<(String, Tone, String)> = snap
        .sources
        .iter()
        .filter(|s| s.discovery_enabled)
        .map(|s| {
            let m = q.matches.iter().find(|m| m.source == s.id);
            let (text, tone, hover) = source_feedback(s, m);
            let text = if several {
                format!("{}: {text}", s.name)
            } else {
                text
            };
            (text, tone, hover)
        })
        .collect();
    if lines.len() > 1 && lines.iter().all(|(_, tone, _)| *tone == Tone::Weak) {
        return vec![(NO_PASS_YET.into(), Tone::Weak, String::new())];
    }
    lines
}

/// One source's health for the status bar: frames, the newest successful
/// detector pass's time, and how many requests failed, came back too late
/// or got no answer.
pub fn source_line(source: &SourceView) -> String {
    let fps = if source.frames == 0 {
        NONE.to_owned()
    } else {
        format!("{:.1}", source.fps)
    };
    let mut line = format!(
        "{} {fps}/{:.0} fps · {} missed",
        source.name, source.nominal_fps, source.missed
    );
    let d = &source.discovery;
    if let Some(latency) = d.last_latency {
        line.push_str(&format!(" · detect {}", millis(latency)));
    }
    for (n, what) in [
        (d.errors, "detector errors"),
        (d.stale, "too old"),
        (d.timeouts, "timed out"),
    ] {
        if n > 0 {
            line.push_str(&format!(" · {n} {what}"));
        }
    }
    if !source.available {
        line.push_str(" · NO SIGNAL");
    }
    line
}

/// How a source's status-bar line is drawn: amber while its newest
/// detector request failed, came back too late or got no answer.
pub fn source_tone(source: &SourceView) -> Tone {
    match source.discovery.last_outcome {
        Some(PassOutcome::Failed | PassOutcome::Stale | PassOutcome::TimedOut) => Tone::Warning,
        Some(PassOutcome::Applied) | None => Tone::Plain,
    }
}

/// A source's status-bar hover text: the newest detector error, if any.
pub fn source_hover(source: &SourceView) -> String {
    source
        .discovery
        .last_error
        .as_ref()
        .map(|e| format!("newest detector error: {e}"))
        .unwrap_or_default()
}

/// Tick timing for the status bar.
pub fn timing_line(timing: &TimingView) -> String {
    if timing.tick.count == 0 {
        return format!("tick {NONE} / {:.1} ms", timing.budget_ms);
    }
    format!(
        "tick {:.1} ms avg · {:.1} max / {:.1} ms · {} late · {} skipped",
        timing.tick.avg_ms,
        timing.tick.max_ms,
        timing.budget_ms,
        timing.late_ticks,
        timing.skipped_ticks
    )
}

/// Frames lost anywhere in the pipeline: replaced in a source mailbox,
/// skipped ticks, and frames the encoder queue dropped.
pub fn dropped_line(snap: &EngineSnapshot) -> String {
    let missed: u64 = snap.sources.iter().map(|s| s.missed).sum();
    let encoder = snap.output.stats.as_ref().map_or(0, |s| s.frames_dropped);
    format!(
        "dropped: {missed} source · {} ticks · {encoder} encoder",
        snap.timing.skipped_ticks
    )
}

/// The output lifecycle as a short word.
pub fn output_state(state: &OutputState) -> String {
    match state {
        OutputState::Idle => "idle".into(),
        OutputState::Starting => "starting…".into(),
        OutputState::Running => "ON AIR".into(),
        OutputState::Stopping => "finishing…".into(),
        OutputState::Finished => "finished".into(),
        OutputState::Failed(e) => format!("failed: {e}"),
    }
}

/// Encoder statistics, one fact per line.
pub fn encoder_lines(stats: &EncoderStats) -> Vec<String> {
    let p = &stats.progress;
    let opt = |v: Option<f64>, digits: usize, unit: &str| {
        v.map_or(NONE.to_owned(), |v| format!("{v:.digits$}{unit}"))
    };
    let mut lines = vec![
        format!("encoder {}", stats.encoder.ffmpeg_name()),
        format!(
            "frames {} written · {} dropped · {} repeated",
            stats.frames_written, stats.frames_dropped, stats.frames_repeated
        ),
        format!(
            "{} fps · {} kbit/s · speed {}",
            opt(p.fps, 1, ""),
            opt(p.bitrate_kbps, 0, ""),
            opt(p.speed, 2, "x")
        ),
    ];
    if let Some(t) = p.out_time {
        lines.push(format!("encoded {}", clock(t)));
    }
    if stats.audio_samples_in > 0 || stats.audio_connected {
        lines.push(format!(
            "audio {} samples · {} dropped · {} silence",
            stats.audio_samples_written, stats.audio_samples_dropped, stats.audio_silence_samples
        ));
    }
    if let Some(e) = &stats.last_error {
        lines.push(format!("last error: {e}"));
    }
    lines
}

/// Where the output goes, for the output panel.
pub fn destinations(output: &OutputView) -> String {
    let mut parts = Vec::new();
    if let Some(u) = &output.rtmp_url {
        parts.push(u.clone());
    }
    if let Some(p) = &output.record_path {
        parts.push(p.display().to_string());
    }
    if parts.is_empty() {
        NONE.into()
    } else {
        parts.join(" + ")
    }
}

#[cfg(test)]
mod tests;
