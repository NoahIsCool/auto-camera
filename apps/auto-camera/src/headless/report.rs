//! The report of a run: its [`Summary`], built from the snapshots and
//! counters the run ended with.
//!
//! [`summarize`] takes each part from what knows it: the shots, subjects,
//! director and program shot from the last running snapshot; the sources,
//! queries, output and timing from the final one, published at shutdown
//! (the last running one when there is none); the tick and thread counts
//! from the engine's report ([`Totals`]); and the crop ranges, the program
//! log and the largest subject count from the [`Sampler`], since no single
//! snapshot has them. Measurements are rounded for reading ([`round`]). A
//! run whose engine never started still gets a summary, with the error and
//! no snapshots ([`failed_to_start`]).

use std::path::Path;
use std::time::Instant;

use ac_director::{ContentProfile, DirectorMode};
use ac_engine::EngineSnapshot;

use super::{
    CropRange, DirectorSummary, HeadlessOptions, MatchSummary, OutputSummary, ProgramSummary,
    QuerySummary, Sampler, ShotSummary, SourceSummary, SubjectSummary, Summary, TimingSummary,
};

/// Counters from the engine report.
pub(super) struct Totals {
    /// Ticks run.
    pub(super) ticks: u64,
    /// Threads joined.
    pub(super) threads_joined: usize,
}

/// The summary of a run whose engine never started.
pub(super) fn failed_to_start(
    path: &Path,
    options: &HeadlessOptions,
    started: Instant,
    error: String,
) -> Summary {
    summarize(
        path,
        options,
        started,
        Some(format!("the engine did not start: {error}")),
        Totals {
            ticks: 0,
            threads_joined: 0,
        },
        None,
        None,
        &Sampler::default(),
    )
}

/// Builds the summary from the last running snapshot (shots, subjects and
/// director), the final one (output and counters, including whether a
/// detector was still preparing when the run ended) and the samples (crop
/// ranges).
#[allow(clippy::too_many_arguments)]
pub(super) fn summarize(
    path: &Path,
    options: &HeadlessOptions,
    started: Instant,
    error: Option<String>,
    totals: Totals,
    last: Option<&EngineSnapshot>,
    fin: Option<&EngineSnapshot>,
    sampler: &Sampler,
) -> Summary {
    let end = fin.or(last);
    let sources = end
        .map(|s| {
            s.sources
                .iter()
                .map(|v| SourceSummary {
                    id: v.id.0.clone(),
                    size: [v.width, v.height],
                    fps: round(v.fps, 2),
                    frames: v.frames,
                    missed: v.missed,
                    discovery_preparing: v.discovery.preparing,
                    detector_ready_s: v.discovery.ready_after.map(|d| round(d.as_secs_f64(), 2)),
                    discovery_requests: v.discovery.requests,
                    discovery_applied: v.discovery.applied,
                    discovery_stale: v.discovery.stale,
                    discovery_errors: v.discovery.errors,
                    discovery_timeouts: v.discovery.timeouts,
                    discovery_last_ms: v
                        .discovery
                        .last_latency
                        .map(|d| round(d.as_secs_f64() * 1e3, 1)),
                    discovery_last_error: v.discovery.last_error.clone(),
                    subjects_created: v.vision.created,
                    subjects_retired: v.vision.retired,
                })
                .collect()
        })
        .unwrap_or_default();
    let shots = last
        .map(|s| {
            s.shots
                .iter()
                .map(|v| {
                    // A shot only in the final snapshot was never sampled.
                    let crop = sampler
                        .crops
                        .get(&v.id.0)
                        .copied()
                        .unwrap_or_else(|| CropRange::new(v.crop));
                    ShotSummary {
                        id: v.id.0.clone(),
                        name: v.name.clone(),
                        status: v.status,
                        quality: (v.quality * 100.0).round() / 100.0,
                        limited_by_zoom: v.limited_by_zoom,
                        same_picture_as: v.same_picture_as.as_ref().map(|t| t.0.clone()),
                        crop_width_min: round(crop.width[0], 4),
                        crop_width_max: round(crop.width[1], 4),
                        crop_height_min: round(crop.height[0], 4),
                        crop_height_max: round(crop.height[1], 4),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let queries = end
        .map(|s| {
            s.queries
                .iter()
                .map(|q| QuerySummary {
                    id: q.id.0.clone(),
                    name: q.name.clone(),
                    prompt: q.prompt.clone(),
                    detector_phrase: q.parse.model_phrase.clone(),
                    literal: q.parse.literal,
                    shot_words: q.parse.shots.clone(),
                    presets: q.presets.iter().map(|p| p.id.clone()).collect(),
                    basis: q.parse.basis,
                    parse: q.parse.summary(),
                    notes: q
                        .parse
                        .warnings
                        .iter()
                        .filter(|w| !w.is_error())
                        .map(ToString::to_string)
                        .collect(),
                    matches: q
                        .matches
                        .iter()
                        .map(|m| MatchSummary {
                            source: m.source.0.clone(),
                            count: m.count,
                            kept: m.kept,
                            floor: round(f64::from(m.floor), 2),
                            best: m.best.map(|b| round(f64::from(b), 2)),
                            seq: m.seq,
                            pts_seconds: round(m.pts.as_secs_f64(), 3),
                        })
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default();
    let subjects = last
        .map(|s| {
            s.subjects
                .iter()
                .map(|v| SubjectSummary {
                    id: v.id.0,
                    source: v.source.0.clone(),
                    query: v.query.0.clone(),
                    aliases: v.aliases.iter().map(|q| q.0.clone()).collect(),
                    state: v.state,
                    bbox: [v.bbox.x, v.bbox.y, v.bbox.w, v.bbox.h].map(|c| round(c, 4)),
                })
                .collect()
        })
        .unwrap_or_default();
    let director = last.map_or(
        DirectorSummary {
            mode: DirectorMode::Off,
            profile: ContentProfile::Talk,
            takes: 0,
            reason: String::new(),
        },
        |s| DirectorSummary {
            mode: s.director.mode,
            profile: s.director.profile,
            takes: s.director.takes,
            reason: s.director.reason_text.clone(),
        },
    );
    let timing = end.map(|s| s.timing).unwrap_or_default();
    let output = options.record.as_ref().and(end).map(|s| {
        let o = &s.output;
        OutputSummary {
            state: format!("{:?}", o.state).to_lowercase(),
            record_path: o.record_path.clone(),
            rtmp_url: o.rtmp_url.clone(),
            frames_pushed: o.frames_pushed,
            frames_written: o.stats.as_ref().map_or(0, |st| st.frames_written),
            frames_dropped: o.stats.as_ref().map_or(0, |st| st.frames_dropped),
            encoder: o
                .stats
                .as_ref()
                .map(|st| st.encoder.ffmpeg_name().to_owned()),
            exit_code: o.stats.as_ref().and_then(|st| st.exit_code),
            last_error: o.stats.as_ref().and_then(|st| st.last_error.clone()),
        }
    });
    Summary {
        ok: error.is_none(),
        error,
        config: path.to_owned(),
        seconds: options.seconds,
        wall_seconds: round(started.elapsed().as_secs_f64(), 2),
        ticks: totals.ticks,
        media_seconds: end.map_or(0.0, |s| round(s.media_time.as_secs_f64(), 3)),
        threads_joined: totals.threads_joined,
        sources,
        queries,
        max_subjects: sampler.max_subjects,
        shots,
        subjects,
        program: ProgramSummary {
            last: last.and_then(|s| s.switcher.program.as_ref().map(|p| p.0.clone())),
            changes: end.map_or(0, |s| s.program_changes),
            log: sampler
                .log
                .iter()
                .map(|(t, s)| (round(*t, 2), s.clone()))
                .collect(),
        },
        director,
        timing: TimingSummary {
            budget_ms: round(timing.budget_ms, 2),
            avg_ms: round(timing.tick.avg_ms, 2),
            max_ms: round(timing.tick.max_ms, 2),
            late_ticks: timing.late_ticks,
            skipped_ticks: timing.skipped_ticks,
        },
        output,
        render_issues: end.map(|s| s.render_issues.clone()).unwrap_or_default(),
    }
}

/// Rounds `v` to `digits` decimals for a readable summary.
pub(super) fn round(v: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    (v * scale).round() / scale
}
