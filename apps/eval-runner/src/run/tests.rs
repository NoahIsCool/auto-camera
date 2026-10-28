//! Tests of the run loop on small synthetic studios.

use std::io;
use std::time::Duration;

use ac_engine::config::{DetectorSettings, QuerySettings};
use ac_eval::trace::{TraceRecord, parse_trace};
use ac_sources::SourceConfig;
use ac_vision::{PassRecord, parse_pass_log};

use super::*;

/// A studio of one 160x90 synthetic source "cam" at `fps` with two
/// subjects, the synthetic detector at 10 passes a second and one query,
/// output at 160x90 and 30 fps.
fn studio(fps: f64) -> StudioConfig {
    let input = SourceConfig::Synthetic {
        width: 160,
        height: 90,
        fps,
        subjects: 2,
    };
    let mut c = StudioConfig::with_source("cam", input);
    c.detector = DetectorSettings::Synthetic { score: 0.9 };
    c.discovery.rate_hz = 10.0;
    c.queries.push(QuerySettings {
        id: None,
        name: "Person".into(),
        prompt: "person".into(),
        presets: None,
        literal: false,
    });
    c.output.width = 160;
    c.output.height = 90;
    c.output.fps = 30;
    c
}

/// What a run wrote: its report, trace and detection log.
struct Outcome {
    /// The report.
    report: RunReport,
    /// The trace, as written.
    trace: String,
    /// The detection log, as written.
    log: String,
}

/// Runs `config` with `detections` and `options`, logging passes.
fn run(config: &StudioConfig, detections: &Detections, options: &RunOptions) -> Outcome {
    let runner = Runner::new(config, detections, options).unwrap();
    let (mut trace, mut log) = (Vec::new(), Vec::new());
    let report = runner
        .run(&VirtualClock::new(), &mut trace, Some(&mut log))
        .unwrap();
    Outcome {
        report,
        trace: String::from_utf8(trace).unwrap(),
        log: String::from_utf8(log).unwrap(),
    }
}

/// A run of `seconds`.
fn seconds(s: f64) -> RunOptions {
    RunOptions {
        seconds: Some(s),
        ..RunOptions::default()
    }
}

/// A replay of `log` at the default tolerance.
fn replay(log: &str) -> Detections {
    Detections::Replay {
        log: parse_pass_log(log).unwrap(),
        tolerance: None,
    }
}

/// The index of the first record where `a` and `b` differ.
fn first_difference(a: &[TraceRecord], b: &[TraceRecord]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

#[test]
fn a_live_run_traces_every_tick_on_simulated_time() {
    let out = run(&studio(30.0), &Detections::Live, &seconds(2.0));
    assert_eq!(out.report.ticks, 60);
    assert_eq!(out.report.detections, "live");
    let records = parse_trace(&out.trace).unwrap();
    assert_eq!(records.len(), 60);
    let period = Duration::from_secs_f64(1.0 / 30.0);
    for (n, r) in (0u32..).zip(&records) {
        assert_eq!(r.tick, u64::from(n) + 1);
        assert_eq!(r.pts, period * n);
        assert!(!r.late && r.skipped == 0, "never late on a virtual clock");
        // Tick n shows frame n of a source at the tick rate.
        assert_eq!(r.sources[0].frame.unwrap().seq, u64::from(n));
    }
    assert_eq!(out.report.media_s, (period * 59).as_secs_f64());
    // The operator took a wide after the first tick.
    assert!(records[0].switcher.program.is_none());
    assert!(records[1].switcher.program.is_some());
    // Discovery at 10 Hz found both subjects.
    let s = &out.report.sources[0];
    assert!(s.requests >= 15 && s.applied >= 15, "{s:?}");
    assert_eq!((s.misses, s.errors, s.recorded), (0, 0, None));
    assert_eq!(records[59].sources[0].subjects.len(), 2);
    let passes = parse_pass_log(&out.log).unwrap();
    assert_eq!(passes.len() as u64, s.applied);
    assert_eq!(out.report.logged, s.applied);
}

#[test]
fn a_replay_of_a_runs_log_reproduces_its_trace_and_its_log() {
    let config = studio(30.0);
    let live = run(&config, &Detections::Live, &seconds(2.0));
    let again = run(&config, &Detections::Live, &seconds(2.0));
    assert_eq!(live.trace, again.trace, "a live run is deterministic too");
    let replayed = run(&config, &replay(&live.log), &seconds(2.0));
    assert_eq!(replayed.trace, live.trace, "byte for byte");
    assert_eq!(replayed.log, live.log, "every pass hit and was applied");
    let s = &replayed.report.sources[0];
    assert_eq!((s.misses, s.errors), (0, 0));
    assert_eq!(s.recorded, Some(parse_pass_log(&live.log).unwrap().len()));
    assert_eq!(s.applied, live.report.sources[0].applied);
    assert_eq!(replayed.report.detections, "replay");
}

/// The live run of `studio(30.0)` for two seconds, and a replay of its
/// log without pass `index`: the live and replayed traces, the tick (from
/// 0) that asked for the pass removed, and the passes each run applied.
fn without_pass(index: usize) -> (Vec<TraceRecord>, Vec<TraceRecord>, usize, (u64, u64)) {
    let config = studio(30.0);
    let live = run(&config, &Detections::Live, &seconds(2.0));
    let mut passes = parse_pass_log(&live.log).unwrap();
    let removed: PassRecord = passes.remove(index);
    let edited: String = passes.iter().map(PassRecord::to_json_line).collect();
    let replayed = run(&config, &replay(&edited), &seconds(2.0));
    assert_eq!(replayed.report.ticks, live.report.ticks, "it still runs");
    let s = &replayed.report.sources[0];
    assert_eq!((s.misses, s.errors), (1, 0));
    assert_eq!(replayed.log, edited, "every other pass was applied");
    let a = parse_trace(&live.trace).unwrap();
    let b = parse_trace(&replayed.trace).unwrap();
    // The pass was asked for at the tick that first showed its frame.
    let asked = a
        .iter()
        .position(|r| r.sources[0].frame.unwrap().seq == removed.seq)
        .unwrap();
    let applied = |o: &Outcome| o.report.sources[0].applied;
    (a, b, asked, (applied(&live), applied(&replayed)))
}

#[test]
fn a_pass_missing_from_the_log_skips_only_that_update() {
    // Without the first pass the subjects appear one pass later: the
    // replay matches the run until the tick that would have applied it.
    let (a, b, asked, _) = without_pass(0);
    assert_eq!(asked, 0);
    assert_eq!(first_difference(&a, &b), Some(asked + 1));
    assert!(a[1].sources[0].subjects.len() == 2 && b[1].sources[0].subjects.is_empty());
    // Without one in the middle, once the subjects are tracked, nothing
    // before that tick changes either, and every later pass still hits.
    let (a, b, asked, (live, replayed)) = without_pass(7);
    assert!(asked > 20, "{asked}");
    assert_eq!(a[..=asked], b[..=asked]);
    assert_eq!(a.len(), b.len());
    assert_eq!(replayed, live - 1);
}

#[test]
fn a_replay_with_a_wider_tolerance_answers_from_the_nearest_pass() {
    // A log made on a 30 fps source replayed on a 25 fps one: no frame
    // matches exactly, but within 20 ms every request finds a pass.
    let live = run(&studio(30.0), &Detections::Live, &seconds(1.0));
    let near = Detections::Replay {
        log: parse_pass_log(&live.log).unwrap(),
        tolerance: Some(Duration::from_millis(20)),
    };
    let out = run(&studio(25.0), &near, &seconds(1.0));
    let s = &out.report.sources[0];
    assert!(s.requests > 0 && s.misses == 0, "{s:?}");
    let exact = Detections::Replay {
        log: parse_pass_log(&live.log).unwrap(),
        tolerance: Some(Duration::ZERO),
    };
    let out = run(&studio(25.0), &exact, &seconds(1.0));
    assert!(out.report.sources[0].misses > 0);
}

#[test]
fn answers_arrive_after_the_stated_latency() {
    let first_applied = |latency_ticks| {
        let options = RunOptions {
            latency_ticks,
            ..seconds(1.0)
        };
        let out = run(&studio(30.0), &Detections::Live, &options);
        let records = parse_trace(&out.trace).unwrap();
        records
            .iter()
            .position(|r| !r.sources[0].subjects.is_empty())
            .unwrap()
    };
    // Asked on the first tick, applied on tick 1 or tick 4.
    assert_eq!(first_applied(1), 1);
    assert_eq!(first_applied(4), 4);
}

#[test]
fn a_studio_without_a_detector_runs_without_discovery() {
    let mut config = studio(30.0);
    config.detector = DetectorSettings::None;
    let out = run(&config, &Detections::Live, &seconds(0.5));
    assert_eq!(out.report.ticks, 15);
    assert_eq!(out.report.sources[0].requests, 0);
    assert!(out.log.is_empty());
}

#[test]
fn passes_of_other_sources_are_counted_and_never_used() {
    let live = run(&studio(30.0), &Detections::Live, &seconds(1.0));
    let foreign = live
        .log
        .replace("\"source\":\"cam\"", "\"source\":\"other\"");
    let out = run(&studio(30.0), &replay(&foreign), &seconds(1.0));
    let passes = parse_pass_log(&live.log).unwrap().len();
    assert_eq!(out.report.foreign_passes, passes);
    let s = &out.report.sources[0];
    assert_eq!((s.recorded, s.applied), (Some(0), 0));
    assert_eq!(s.misses, s.requests);
}

#[test]
fn a_run_length_is_counted_in_ticks() {
    let pattern = || {
        let p = ac_sources::SyntheticPattern::new(16, 16, 30.0, 1).unwrap();
        vec![Input::pattern(ac_core::SourceId("p".into()), p)]
    };
    assert_eq!(tick_limit(Some(2.0), 30, &pattern()).unwrap(), Some(60));
    assert_eq!(tick_limit(Some(0.049), 30, &pattern()).unwrap(), Some(1));
    assert_eq!(tick_limit(Some(0.01), 30, &pattern()).unwrap(), Some(1));
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let err = tick_limit(Some(bad), 30, &pattern()).unwrap_err();
        assert!(err.to_string().contains("positive"), "{bad}: {err}");
    }
    assert!(tick_limit(Some(1e300), 30, &pattern()).is_err());
    let err = tick_limit(None, 30, &pattern()).unwrap_err();
    assert!(err.to_string().contains("never ends"), "{err}");
    assert_eq!(tick_limit(None, 30, &[]).unwrap(), None);
}

#[test]
fn bad_options_and_studios_are_refused_before_running() {
    let config = studio(30.0);
    let zero = RunOptions {
        latency_ticks: 0,
        ..seconds(1.0)
    };
    let err = Runner::new(&config, &Detections::Live, &zero).unwrap_err();
    assert!(err.to_string().contains("at least one tick"), "{err}");
    let err = Runner::new(&config, &Detections::Live, &RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("never ends"), "{err}");
    let mut bad = config.clone();
    bad.output.width = 0;
    assert!(matches!(
        Runner::new(&bad, &Detections::Live, &seconds(1.0)),
        Err(RunError::Setup(_))
    ));
}

/// A writer that always fails.
struct Broken;

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_trace_that_cannot_be_written_ends_the_run() {
    let runner = Runner::new(&studio(30.0), &Detections::Live, &seconds(1.0)).unwrap();
    let err = runner
        .run(&VirtualClock::new(), &mut Broken, None)
        .unwrap_err();
    assert!(matches!(err, RunError::Write(_)), "{err}");
    assert!(err.to_string().contains("disk full"), "{err}");
}

#[test]
fn the_clock_moves_by_exactly_the_ticks_run() {
    let clock = VirtualClock::new();
    let runner = Runner::new(&studio(30.0), &Detections::Live, &seconds(1.0)).unwrap();
    let report = runner.run(&clock, &mut io::sink(), None).unwrap();
    assert_eq!(report.ticks, 30);
    // It slept up to the last tick and no further.
    let period = Duration::from_secs_f64(1.0 / 30.0);
    assert_eq!(clock.now(), period * 29);
    assert_eq!(report.logged, 0, "no log was asked for");
}
