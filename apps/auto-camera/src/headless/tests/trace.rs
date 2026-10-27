//! Headless runs with `--trace`: a line for every tick, and a trace file
//! that cannot be created failing the run before the engine starts.

use super::*;

/// Run options with a trace at `trace`.
fn traced(seconds: f64, trace: &Path) -> HeadlessOptions {
    HeadlessOptions {
        trace: Some(trace.to_owned()),
        ..options(seconds, None)
    }
}

#[test]
fn a_traced_run_writes_a_line_for_every_tick() {
    let path = temp_path("run.jsonl");
    let s = run(
        studio(),
        Path::new("studio.toml"),
        &traced(0.6, &path),
        &QuitSignal::new(),
    );
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(s.ok, "{:?}", s.error);
    let trace = s.trace.as_ref().expect("the summary reports the trace");
    assert_eq!(trace.path, path);
    assert_eq!((trace.records, trace.missing), (s.ticks, 0));
    let records = ac_eval::parse_trace(&text).unwrap();
    let ticks: Vec<u64> = records.iter().map(|r| r.tick).collect();
    assert_eq!(ticks, (1..=s.ticks).collect::<Vec<u64>>());
    // The trace ends where the summary does.
    let last = records.last().unwrap();
    assert_eq!(
        last.switcher.program.as_ref().map(|p| p.0.as_str()),
        s.program.last.as_deref()
    );
    assert_eq!(round(last.pts.as_secs_f64(), 3), s.media_seconds);
    let late = records.iter().filter(|r| r.late).count() as u64;
    let skipped: u64 = records.iter().map(|r| r.skipped).sum();
    assert_eq!(
        (late, skipped),
        (s.timing.late_ticks, s.timing.skipped_ticks)
    );
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["trace"]["records"], s.ticks);
    // Without `--trace` there is no trace.
    let plain = run(
        studio(),
        Path::new("studio.toml"),
        &options(0.2, None),
        &QuitSignal::new(),
    );
    assert!(plain.ok && plain.trace.is_none(), "{:?}", plain.error);
}

#[test]
fn a_trace_that_cannot_be_created_fails_the_run_at_once() {
    let path = temp_path("taken.jsonl");
    std::fs::write(&path, "keep me\n").unwrap();
    let s = run(
        studio(),
        Path::new("studio.toml"),
        &traced(30.0, &path),
        &QuitSignal::new(),
    );
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(!s.ok);
    let error = s.error.unwrap();
    assert!(
        error.starts_with("the engine did not start: cannot create the trace"),
        "{error}"
    );
    assert_eq!((s.ticks, s.trace.is_none()), (0, true));
    assert_eq!(text, "keep me\n", "never overwritten");
}

#[test]
fn a_run_whose_engine_does_not_start_still_closes_its_trace() {
    let path = temp_path("unstarted.jsonl");
    // A studio without sources is refused when the engine starts.
    let mut config = studio();
    config.sources.clear();
    let s = run(
        config,
        Path::new("studio.toml"),
        &traced(30.0, &path),
        &QuitSignal::new(),
    );
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(!s.ok);
    assert!(s.error.unwrap().starts_with("the engine did not start"));
    let trace = s.trace.expect("the trace file was made");
    assert_eq!((trace.records, trace.missing), (0, 0));
    assert!(text.is_empty());
}
