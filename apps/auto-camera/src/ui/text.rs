//! Operator-facing text, built from snapshot values.
//!
//! Every label the window shows that is more than a constant is built here,
//! as plain functions of snapshot data, so wording and rounding are tested
//! without a window. Units are always spelled out (`fps`, `ms`, `kbit/s`),
//! and absent values read as an en dash rather than zero, since "0 fps"
//! and "no measurement yet" mean different things during a show.

use std::time::Duration;

use ac_core::ShotId;
use ac_engine::{
    EngineSnapshot, OutputState, OutputView, ShotStatus, ShotView, SourceView, TimingView,
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

/// A shot's name as its multiview tile shows it: marked [`LIMITED`] when the
/// zoom ceiling keeps it from its size, as for "Person 1 Close (limited)" on
/// a distant speaker, so the operator never takes it for a true close-up.
pub fn shot_title(shot: &ShotView) -> String {
    if shot.limited_by_zoom {
        format!("{} {LIMITED}", shot.name)
    } else {
        shot.name.clone()
    }
}

/// A shot tile's hover text: its id, why it is limited if it is, and what
/// clicks do.
pub fn shot_hover(shot: &ShotView) -> String {
    let limited = if shot.limited_by_zoom {
        "\nlimited: the preset's max zoom stops this shot reaching its size"
    } else {
        ""
    };
    format!("{}{limited}\nclick: preview · double-click: cut", shot.id.0)
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

/// The name of `shot`, or [`NONE`].
pub fn shot_name(snap: &EngineSnapshot, shot: Option<&ShotId>) -> String {
    match shot {
        Some(id) => snap
            .shot(id)
            .map_or_else(|| id.0.clone(), |s| s.name.clone()),
        None => NONE.into(),
    }
}

/// One source's health for the status bar.
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
    if let Some(latency) = source.discovery.last_latency {
        line.push_str(&format!(" · detect {}", millis(latency)));
    }
    if !source.available {
        line.push_str(" · NO SIGNAL");
    }
    line
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
