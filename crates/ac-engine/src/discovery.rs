//! Discovery: the slow, open-vocabulary detector tier, off the frame path.
//!
//! Each source has one discovery worker thread that owns that source's
//! [`Detector`]. The engine thread decides when to ask (at most
//! `discovery.rate_hz` per source, never while a request is in flight) and
//! hands the worker the newest frame through a single-slot mailbox, so the
//! detector always works on the newest frame and requests never queue. The
//! worker answers on a channel with a [`DiscoveryResult`] carrying the frame
//! it looked at, so results are tagged with that frame's source, sequence
//! number and PTS.
//!
//! Results are applied on the engine thread unless they are stale
//! ([`staleness`]): computed on a frame older than one already applied, or
//! older than `max_result_age` relative to the newest frame of the source
//! (the subjects have moved on, and a late box would yank shots backwards).
//! Detections for queries removed in the meantime are filtered out when
//! applied.
//!
//! A panicking detector does not take the engine down: the panic is caught
//! and reported as an error result, and the worker keeps serving.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ac_media::VideoFrame;
use ac_sources::{SourceConfig, SyntheticPattern};
use ac_vision::{Detection, Detector, PromptQuery, ScriptedDetector};

use crate::config::DetectorSettings;
use crate::mailbox::{Slot, TakeError};

/// How long a worker waits for a request before re-checking for shutdown.
const WORKER_POLL: Duration = Duration::from_millis(200);

/// A request for one detector pass.
#[derive(Debug, Clone)]
pub struct DiscoveryRequest {
    /// The frame to look at (cheap: pixels are shared).
    pub frame: VideoFrame,
    /// The prompts to look for.
    pub queries: Vec<PromptQuery>,
}

/// The answer to a [`DiscoveryRequest`].
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    /// Index of the source in the engine's source list.
    pub source: usize,
    /// The frame that was looked at.
    pub frame: VideoFrame,
    /// The detections, or the detector's error message.
    pub outcome: Result<Vec<Detection>, String>,
    /// Processing time of the detector pass.
    pub latency: Duration,
}

/// Why a result is not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staleness {
    /// A result for this or a newer frame was already applied.
    Superseded,
    /// The frame is older than the age limit relative to the newest frame.
    TooOld,
}

/// Decides whether a result computed on frame `(seq, pts)` is stale; see the
/// module docs. `last_applied` is the sequence number of the newest frame
/// whose detections were applied, `newest_pts` the PTS of the newest frame.
pub fn staleness(
    seq: u64,
    pts: Duration,
    last_applied: Option<u64>,
    newest_pts: Option<Duration>,
    max_age: Duration,
) -> Option<Staleness> {
    if last_applied.is_some_and(|s| seq <= s) {
        return Some(Staleness::Superseded);
    }
    if newest_pts.is_some_and(|n| n.saturating_sub(pts) > max_age) {
        return Some(Staleness::TooOld);
    }
    None
}

/// A detector answering every query with every subject of a synthetic
/// pattern, at `score`. The pattern must match the source's parameters; it
/// is a pure function of PTS, so the boxes are the exact ground truth.
pub fn synthetic_detector(pattern: SyntheticPattern, score: f32) -> ScriptedDetector {
    ScriptedDetector::from_fn("synthetic-ground-truth", move |frame, queries| {
        let truth = pattern.ground_truth(frame.pts);
        Ok(queries
            .iter()
            .flat_map(|q| {
                truth.iter().map(|g| Detection {
                    query: q.id.clone(),
                    bbox: g.bbox,
                    score,
                })
            })
            .collect())
    })
}

/// Builds the detector for one source, or `None` when discovery is off.
pub fn build_detector(
    settings: &DetectorSettings,
    source: &SourceConfig,
) -> Result<Option<Box<dyn Detector>>, String> {
    match settings {
        DetectorSettings::None => Ok(None),
        DetectorSettings::Synthetic { score } => match source {
            SourceConfig::Synthetic {
                width,
                height,
                fps,
                subjects,
            } => {
                let pattern = SyntheticPattern::new(*width, *height, *fps, *subjects)
                    .map_err(|e| e.to_string())?;
                Ok(Some(Box::new(synthetic_detector(pattern, *score))))
            }
            SourceConfig::File { .. } => {
                Err("the synthetic detector only works on synthetic sources".into())
            }
        },
        DetectorSettings::Owl { .. } => build_owl(settings).map(Some),
    }
}

/// Builds the OWL detector (feature `onnx`).
#[cfg(feature = "onnx")]
fn build_owl(settings: &DetectorSettings) -> Result<Box<dyn Detector>, String> {
    use ac_vision::{OnnxOwlDetector, OwlConfig, OwlModel};
    let DetectorSettings::Owl {
        model,
        text_encoder,
        tokenizer,
        input_size,
        score_threshold,
        use_coreml,
    } = settings
    else {
        return Err("not an OWL detector config".into());
    };
    let model = match text_encoder {
        Some(text_encoder) => OwlModel::Split {
            text_encoder: text_encoder.clone(),
            detector: model.clone(),
        },
        None => OwlModel::Combined {
            model: model.clone(),
        },
    };
    let defaults = OwlConfig::default();
    let config = OwlConfig {
        input_size: input_size.unwrap_or(defaults.input_size),
        score_threshold: score_threshold.unwrap_or(defaults.score_threshold),
        use_coreml: use_coreml.unwrap_or(defaults.use_coreml),
        ..defaults
    };
    let detector = OnnxOwlDetector::load(model, tokenizer, config).map_err(|e| e.to_string())?;
    Ok(Box::new(detector))
}

/// Builds the OWL detector (feature `onnx`).
#[cfg(not(feature = "onnx"))]
fn build_owl(_: &DetectorSettings) -> Result<Box<dyn Detector>, String> {
    Err("detector kind \"owl\" needs auto-camera built with `--features onnx`".into())
}

/// One source's discovery thread; see the module docs.
#[derive(Debug)]
pub struct DiscoveryWorker {
    /// Newest-request mailbox shared with the thread.
    slot: Arc<Slot<DiscoveryRequest>>,
    /// The thread, joined by [`DiscoveryWorker::stop`].
    thread: Option<JoinHandle<()>>,
}

impl DiscoveryWorker {
    /// Starts a worker for source `index` that answers on `results`.
    pub fn spawn(
        index: usize,
        name: &str,
        mut detector: Box<dyn Detector>,
        results: Sender<DiscoveryResult>,
    ) -> std::io::Result<Self> {
        let slot: Arc<Slot<DiscoveryRequest>> = Arc::new(Slot::new());
        let inbox = slot.clone();
        let thread = std::thread::Builder::new()
            .name(format!("ac-discovery-{name}"))
            .spawn(move || {
                loop {
                    let request = match inbox.take(WORKER_POLL) {
                        Ok(r) => r,
                        Err(TakeError::Timeout) => continue,
                        Err(TakeError::Closed) => break,
                    };
                    let started = Instant::now();
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        detector.detect(&request.frame, &request.queries)
                    }))
                    .unwrap_or_else(|_| {
                        Err(ac_vision::VisionError::Model("detector panicked".into()))
                    })
                    .map_err(|e| e.to_string());
                    let result = DiscoveryResult {
                        source: index,
                        frame: request.frame,
                        outcome,
                        latency: started.elapsed(),
                    };
                    if results.send(result).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            slot,
            thread: Some(thread),
        })
    }

    /// Hands the worker a request, replacing one it has not started yet.
    pub fn submit(&self, request: DiscoveryRequest) {
        self.slot.put(request);
    }

    /// Stops the worker and joins its thread (after a pass in progress
    /// finishes). Idempotent.
    pub fn stop(&mut self) {
        self.slot.close();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for DiscoveryWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::{QueryId, Rect, SourceId};
    use std::sync::mpsc::channel;

    /// `v` milliseconds.
    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn staleness_rules() {
        let age = ms(2000);
        assert_eq!(staleness(10, ms(300), None, Some(ms(400)), age), None);
        assert_eq!(staleness(10, ms(300), Some(9), None, age), None);
        assert_eq!(
            staleness(10, ms(300), Some(10), Some(ms(300)), age),
            Some(Staleness::Superseded)
        );
        assert_eq!(
            staleness(10, ms(300), None, Some(ms(2301)), age),
            Some(Staleness::TooOld)
        );
        // A result newer than the newest frame seen is fine.
        assert_eq!(staleness(10, ms(900), None, Some(ms(300)), age), None);
    }

    #[test]
    fn synthetic_detector_reports_ground_truth_for_every_query() {
        let p = SyntheticPattern::new(64, 36, 30.0, 3).unwrap();
        let mut d = synthetic_detector(p.clone(), 0.8);
        let f = p.frame(&SourceId("s".into()), 45);
        let q = [PromptQuery::new("a", "person"), PromptQuery::new("b", "x")];
        let out = d.detect(&f, &q).unwrap();
        assert_eq!(out.len(), 6);
        let truth = p.ground_truth(f.pts);
        assert_eq!(out[0].bbox, truth[0].bbox);
        assert_eq!(out[3].query, QueryId("b".into()));
        assert!(out.iter().all(|d| d.score == 0.8));
        assert!(d.detect(&f, &[]).unwrap().is_empty());
    }

    #[test]
    fn detector_factory_follows_the_settings() {
        let synthetic = SourceConfig::Synthetic {
            width: 64,
            height: 36,
            fps: 30.0,
            subjects: 1,
        };
        let file = SourceConfig::File {
            path: "a.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: false,
        };
        assert!(
            build_detector(&DetectorSettings::None, &synthetic)
                .unwrap()
                .is_none()
        );
        let s = DetectorSettings::Synthetic { score: 0.9 };
        assert!(build_detector(&s, &synthetic).unwrap().is_some());
        assert!(build_detector(&s, &file).is_err());
        let owl = DetectorSettings::Owl {
            model: "/nonexistent/owl.onnx".into(),
            text_encoder: None,
            tokenizer: "/nonexistent/tokenizer.json".into(),
            input_size: None,
            score_threshold: None,
            use_coreml: None,
        };
        assert!(build_detector(&owl, &file).is_err());
    }

    #[test]
    fn worker_answers_with_the_frame_it_saw_and_survives_panics() {
        let (tx, rx) = channel();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = calls.clone();
        let detector = ScriptedDetector::from_fn("flaky", move |_, queries| {
            if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                panic!("first call fails");
            }
            Ok(vec![Detection {
                query: queries[0].id.clone(),
                bbox: Rect::new(0.1, 0.1, 0.2, 0.2),
                score: 0.5,
            }])
        });
        let mut w = DiscoveryWorker::spawn(3, "t", Box::new(detector), tx).unwrap();
        let p = SyntheticPattern::new(16, 9, 30.0, 0).unwrap();
        let queries = vec![PromptQuery::new("q", "thing")];
        for seq in [7, 8] {
            w.submit(DiscoveryRequest {
                frame: p.frame(&SourceId("s".into()), seq),
                queries: queries.clone(),
            });
            let r = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!((r.source, r.frame.seq), (3, seq));
            if seq == 7 {
                assert!(r.outcome.unwrap_err().contains("panicked"));
            } else {
                assert_eq!(r.outcome.unwrap().len(), 1);
            }
        }
        w.stop();
        w.stop();
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    }
}
