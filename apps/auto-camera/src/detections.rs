//! `--record-detections PATH`: every detector pass the engine applied, as a
//! detection log.
//!
//! The log is what the eval runner replays instead of running the
//! detector (`eval-runner --replay PATH`, `ac_vision::replay`): one JSON
//! line per applied pass, with the frame's source, sequence number and
//! PTS, the queries looked for and every detection (query, box, score,
//! extent). The engine thread sends each pass as it applies it, on a
//! bounded channel it never waits on (`ac_engine::detection_log`), and a
//! [`TraceFile`] writes the lines on a thread of its own (`ac-detections`),
//! exactly as the trace is written ([`crate::trace`]): the file must not
//! exist, a writer that falls behind loses passes rather than slowing the
//! engine, a write error ends the log and fails the run, and a run whose
//! engine does not start removes the empty file.
//!
//! A live run's passes looked at whichever frames real time handed the
//! detector, so a replay of this log in the runner matches each request to
//! the nearest pass within a stated tolerance (`--tolerance-ms`).

use std::path::Path;

use ac_engine::{EngineOptions, PassRecord};

use crate::trace::{Line, TraceFile};

impl Line for PassRecord {
    const NOUN: &'static str = "detection log";
    const THREAD: &'static str = "ac-detections";

    fn to_json_line(&self) -> String {
        PassRecord::to_json_line(self)
    }
}

/// Creates the detection log at `path`, if one was asked for, and returns
/// it with `options` sending to it; without a path, `options` as they are.
pub fn attach(
    path: Option<&Path>,
    options: EngineOptions,
) -> Result<(Option<TraceFile<PassRecord>>, EngineOptions), String> {
    let Some(path) = path else {
        return Ok((None, options));
    };
    let (file, tx) = TraceFile::create(path)?;
    let options = EngineOptions {
        detections: Some(tx),
        ..options
    };
    Ok((Some(file), options))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ac_core::{Extent, QueryId, Rect, SourceId};
    use ac_engine::{DetectorSettings, Engine, MonitorSettings, QuerySettings, StudioConfig};
    use ac_sources::SourceConfig;
    use ac_vision::{Detection, parse_pass_log};

    use super::*;

    /// A fresh, unique path in the temp directory (not created).
    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!(
            "auto-camera-detections-{}-{nanos}-{name}",
            std::process::id()
        ))
    }

    /// A pass on frame `seq` finding one box.
    fn pass(seq: u64) -> PassRecord {
        let q = QueryId("person".into());
        PassRecord::new(
            SourceId("stage".into()),
            seq,
            Duration::from_millis(seq * 33),
            vec![q.clone()],
            vec![Detection {
                query: q,
                bbox: Rect::new(0.1, 0.2, 0.3, 0.4),
                score: 0.75,
                extent: Extent::Person,
            }],
        )
    }

    #[test]
    fn passes_are_written_one_line_each_and_read_back() {
        let path = temp_path("log.jsonl");
        let (file, tx) = TraceFile::<PassRecord>::create(&path).unwrap();
        let sent: Vec<PassRecord> = [3, 6, 9].map(pass).to_vec();
        for p in &sent {
            tx.send(p.clone()).unwrap();
        }
        drop(tx);
        assert_eq!(file.finish().lines, 3);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(parse_pass_log(&text).unwrap(), sent);
    }

    #[test]
    fn its_messages_name_the_detection_log() {
        let path = temp_path("taken.jsonl");
        std::fs::write(&path, "").unwrap();
        let err = attach(Some(&path), EngineOptions::default()).err().unwrap();
        assert!(err.starts_with("cannot create the detection log"), "{err}");
        std::fs::remove_file(&path).unwrap();
        let (file, options) = attach(None, EngineOptions::default()).unwrap();
        assert!(file.is_none() && options.detections.is_none());
        // An empty log is removed, and a missing one says which it was.
        let (file, tx) = TraceFile::<PassRecord>::create(&path).unwrap();
        drop(tx);
        std::fs::remove_file(&path).unwrap();
        let error = file.discard_if_empty().error.unwrap();
        assert!(
            error.starts_with("cannot remove the empty detection log"),
            "{error}"
        );
    }

    #[test]
    fn an_engine_started_with_attached_options_logs_every_applied_pass() {
        let mut config = StudioConfig::with_source(
            "stage",
            SourceConfig::Synthetic {
                width: 320,
                height: 180,
                fps: 30.0,
                subjects: 2,
            },
        );
        config.detector = DetectorSettings::Synthetic { score: 0.9 };
        config.discovery.rate_hz = 10.0;
        config.queries.push(QuerySettings {
            id: None,
            name: "Person".into(),
            prompt: "person".into(),
            presets: None,
            literal: false,
            priority: None,
        });
        config.output.width = 320;
        config.output.height = 180;
        config.monitors = MonitorSettings::disabled();
        let path = temp_path("engine.jsonl");
        let (file, options) = attach(Some(&path), EngineOptions::default()).unwrap();
        let engine = Engine::start_with(config, options).unwrap();
        engine
            .handle()
            .wait_until(Duration::from_secs(20), |s| {
                s.sources[0].discovery.applied >= 3
            })
            .expect("passes are applied");
        let report = engine.shutdown().unwrap();
        // The engine's sender went with its thread, so this returns.
        let written = file.unwrap().finish();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(written.error, None);
        let applied = report.final_snapshot.sources[0].discovery.applied;
        assert_eq!(written.lines, applied);
        let passes = parse_pass_log(&text).unwrap();
        assert_eq!(passes.len() as u64, applied);
        assert!(passes.iter().all(|p| p.detections.len() == 2));
    }
}
