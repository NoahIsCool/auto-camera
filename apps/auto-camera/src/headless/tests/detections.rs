//! Headless runs with `--record-detections`: a line for every applied
//! pass, which a replay answers with what the detector found, and a log
//! that cannot be created, or an engine that does not start, leaving no
//! file behind.

use std::time::Duration;

use ac_sources::SyntheticPattern;
use ac_vision::{Detector, PromptQuery, ReplayDetector, parse_pass_log};

use super::*;

/// Run options with a detection log at `log`.
fn logged(seconds: f64, log: &Path) -> HeadlessOptions {
    HeadlessOptions {
        record_detections: Some(log.to_owned()),
        ..options(seconds, None)
    }
}

#[test]
fn a_logged_run_writes_every_applied_pass_for_replay() {
    let path = temp_path("passes.jsonl");
    let s = run(
        studio(),
        Path::new("studio.toml"),
        &logged(0.6, &path),
        &QuitSignal::new(),
    );
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(s.ok, "{:?}", s.error);
    let log = s.detections.as_ref().expect("the summary reports the log");
    assert_eq!(log.path, path);
    let applied = s.sources[0].discovery_applied;
    assert!(applied >= 3, "{applied} passes");
    assert_eq!((log.records, log.missing), (applied, 0));
    let passes = parse_pass_log(&text).unwrap();
    assert_eq!(passes.len() as u64, applied);
    assert!(passes.windows(2).all(|w| w[0].seq < w[1].seq), "in order");
    // A replay of the log answers each pass's frame with its boxes, which
    // are the ground truth of the synthetic studio.
    let pattern = SyntheticPattern::new(320, 180, 30.0, 2).unwrap();
    let id = ac_core::SourceId("stage".into());
    let mut replay = ReplayDetector::new(id.clone(), passes.clone(), Duration::ZERO);
    let query = [PromptQuery::new("person", "person")];
    for pass in &passes {
        let frame = pattern.frame(&id, pass.seq);
        assert_eq!(frame.pts, pass.pts);
        let found = replay.detect(&frame, &query).unwrap();
        let boxes: Vec<_> = found.iter().map(|d| d.bbox).collect();
        let truth: Vec<_> = pattern
            .ground_truth(frame.pts)
            .iter()
            .map(|g| g.bbox)
            .collect();
        assert_eq!(boxes, truth, "frame {}", pass.seq);
    }
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["detections"]["records"], applied);
    // Without the flag there is no log.
    assert!(s.trace.is_none());
    let plain = run(
        studio(),
        Path::new("studio.toml"),
        &options(0.2, None),
        &QuitSignal::new(),
    );
    assert!(plain.ok && plain.detections.is_none(), "{:?}", plain.error);
}

#[test]
fn a_log_that_cannot_be_created_fails_the_run_and_frees_the_trace() {
    let (trace, log) = (temp_path("trace.jsonl"), temp_path("taken.jsonl"));
    std::fs::write(&log, "keep me\n").unwrap();
    let options = HeadlessOptions {
        trace: Some(trace.clone()),
        ..logged(30.0, &log)
    };
    let s = run(
        studio(),
        Path::new("studio.toml"),
        &options,
        &QuitSignal::new(),
    );
    let text = std::fs::read_to_string(&log).unwrap();
    std::fs::remove_file(&log).unwrap();
    let error = s.error.unwrap();
    assert!(
        error.starts_with("the engine did not start: cannot create the detection log"),
        "{error}"
    );
    assert_eq!(text, "keep me\n", "never overwritten");
    assert!(!trace.exists(), "the empty trace is removed");
    assert!(s.detections.is_none() && s.trace.is_none());
}

#[test]
fn a_run_whose_engine_does_not_start_leaves_no_log_behind() {
    let path = temp_path("unstarted.jsonl");
    let mut config = studio();
    config.sources.clear();
    let s = run(
        config,
        Path::new("studio.toml"),
        &logged(30.0, &path),
        &QuitSignal::new(),
    );
    let leftover = path.exists();
    let _ = std::fs::remove_file(&path);
    let error = s.error.unwrap();
    assert!(
        error.starts_with("the engine did not start") && !error.contains("detection log"),
        "{error}"
    );
    assert!(!leftover, "the empty log is removed");
    assert!(s.detections.is_none());
}
