//! Where the runner's detections come from: the studio's own detector, or
//! a detection log played back.
//!
//! A **live** run builds each source's detector from the studio config, as
//! the engine does (`ac_engine::discovery::build_detector`), and loads it
//! here, on the runner's one thread: the synthetic detector (the exact
//! ground truth of a synthetic source) and, in a build with the `onnx`
//! feature, the OWLv2 model. The sidecar is refused: it is a separate
//! process answering in real time, so its passes are recorded with
//! `auto-camera --record-detections` and replayed here.
//!
//! A **replay** run gives every source a [`ReplayDetector`] over the log's
//! passes of that source. Its tolerance is half the source's frame
//! interval unless the caller states one: a request then only ever matches
//! a pass on the same frame, which is what a log recorded by the runner
//! needs (its passes looked at exactly the frames the replay asks about).
//! A log recorded live by the app looked at whichever frames real time
//! handed its detector, so it is replayed with a wider tolerance, such as
//! half the discovery interval, and each request gets the nearest pass.
//!
//! Either way a source runs discovery exactly when the studio has a
//! detector, so a replay asks for the same frames as the run it replays: a
//! studio without one has nothing to replay, which is an error.
//!
//! # Invariants
//!
//! * One entry per configured source, in order; `None` where the source
//!   runs no discovery.
//! * A replay's answers depend on the log, the tolerance and the request
//!   alone (`ac_vision::replay`); nothing here reads a file or a clock.

use std::time::Duration;

use ac_engine::config::{DetectorSettings, StudioConfig};
use ac_engine::discovery::build_detector;
use ac_media::SourceInfo;
use ac_vision::{Detector, PassRecord, ReplayDetector};

/// Where detections come from; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub enum Detections {
    /// The studio's own detector.
    Live,
    /// A detection log played back.
    Replay {
        /// The log's passes, of any source.
        log: Vec<PassRecord>,
        /// How far from a frame's PTS a pass may be and still answer it;
        /// `None` for half the source's frame interval.
        tolerance: Option<Duration>,
    },
}

impl Detections {
    /// `"live"` or `"replay"`, for the run's report.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Replay { .. } => "replay",
        }
    }
}

/// One source's detector for the run, or `None` when it runs no
/// discovery.
pub type SourceDetector = Option<Box<dyn Detector>>;

/// Builds and loads the detector of every source of `config`, whose
/// pictures `infos` describes (same order); see the module docs.
pub fn build(
    config: &StudioConfig,
    infos: &[SourceInfo],
    detections: &Detections,
) -> Result<Vec<SourceDetector>, String> {
    match detections {
        Detections::Live => live(config),
        Detections::Replay { log, tolerance } => replay(config, infos, log, *tolerance),
    }
}

/// The studio's own detectors, loaded.
fn live(config: &StudioConfig) -> Result<Vec<SourceDetector>, String> {
    if matches!(config.detector, DetectorSettings::Sidecar(_)) {
        return Err(
            "the sidecar detector answers in real time and cannot be run here; record its \
             passes with `auto-camera --record-detections PATH` and replay them with \
             `--replay PATH`"
                .into(),
        );
    }
    config
        .sources
        .iter()
        .map(|entry| {
            let loader = build_detector(&config.detector, &entry.input)
                .map_err(|e| format!("source {:?}: {e}", entry.id))?;
            loader
                .map(|load| load().map_err(|e| format!("source {:?}: {e}", entry.id)))
                .transpose()
        })
        .collect()
}

/// A replay detector per source over `log`.
fn replay(
    config: &StudioConfig,
    infos: &[SourceInfo],
    log: &[PassRecord],
    tolerance: Option<Duration>,
) -> Result<Vec<SourceDetector>, String> {
    if matches!(config.detector, DetectorSettings::None) {
        return Err(
            "the studio has no detector ([detector] kind = \"none\"), so it makes no passes \
             to replay"
                .into(),
        );
    }
    Ok(config
        .sources
        .iter()
        .zip(infos)
        .map(|(entry, info)| {
            let tolerance = tolerance.unwrap_or_else(|| half_frame(info.fps));
            let detector = ReplayDetector::new(entry.source_id(), log.iter().cloned(), tolerance);
            Some(Box::new(detector) as Box<dyn Detector>)
        })
        .collect())
}

/// Half the frame interval at `fps` (0 for a rate that is not positive).
fn half_frame(fps: f64) -> Duration {
    Duration::try_from_secs_f64(0.5 / fps).unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ac_core::{Extent, QueryId, Rect, SourceId};
    use ac_sources::{SourceConfig, SyntheticPattern};
    use ac_vision::{Detection, PromptQuery, VisionError};

    use super::*;

    /// A studio of synthetic sources `ids` (64x36, 10 fps) with `detector`.
    fn studio(ids: &[&str], detector: DetectorSettings) -> (StudioConfig, Vec<SourceInfo>) {
        let input = SourceConfig::Synthetic {
            width: 64,
            height: 36,
            fps: 10.0,
            subjects: 1,
        };
        let mut c = StudioConfig::with_source(ids[0], input.clone());
        for id in &ids[1..] {
            let mut extra = StudioConfig::with_source(id, input.clone());
            c.sources.append(&mut extra.sources);
        }
        c.detector = detector;
        let infos = ids
            .iter()
            .map(|id| SourceInfo {
                id: SourceId((*id).into()),
                width: 64,
                height: 36,
                fps: 10.0,
                audio: None,
            })
            .collect();
        (c, infos)
    }

    /// A pass of `source` at `ms` finding one box.
    fn pass(source: &str, seq: u64, ms: u64) -> PassRecord {
        let q = QueryId("person".into());
        PassRecord::new(
            SourceId(source.into()),
            seq,
            Duration::from_millis(ms),
            vec![q.clone()],
            vec![Detection {
                query: q,
                bbox: Rect::new(0.1, 0.2, 0.3, 0.4),
                score: 0.7,
                extent: Extent::Person,
            }],
        )
    }

    #[test]
    fn a_live_studio_loads_its_own_detector_for_every_source() {
        let (c, infos) = studio(&["a", "b"], DetectorSettings::Synthetic { score: 0.9 });
        let mut detectors = build(&c, &infos, &Detections::Live).unwrap();
        assert_eq!(detectors.len(), 2);
        let p = SyntheticPattern::new(64, 36, 10.0, 1).unwrap();
        let frame = p.frame(&SourceId("a".into()), 3);
        let found = detectors[0]
            .as_mut()
            .unwrap()
            .detect(&frame, &[PromptQuery::new("person", "person")])
            .unwrap();
        assert_eq!(found[0].bbox, p.ground_truth(frame.pts)[0].bbox);
        let (none, infos) = studio(&["a"], DetectorSettings::None);
        let detectors = build(&none, &infos, &Detections::Live).unwrap();
        assert!(detectors[0].is_none(), "no detector, no discovery");
    }

    #[test]
    fn a_detector_that_cannot_be_built_names_its_source() {
        let (mut c, infos) = studio(&["a"], DetectorSettings::Synthetic { score: 0.9 });
        c.sources[0].input = SourceConfig::File {
            path: "clip.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: false,
            start_s: None,
            duration_s: None,
        };
        let err = build(&c, &infos, &Detections::Live).err().unwrap();
        assert!(
            err.contains("source \"a\"") && err.contains("synthetic"),
            "{err}"
        );
    }

    #[test]
    fn a_replay_answers_each_source_from_its_own_passes() {
        let (c, infos) = studio(&["a", "b"], DetectorSettings::Synthetic { score: 0.9 });
        let log = vec![pass("a", 1, 100), pass("b", 2, 200), pass("x", 1, 100)];
        let replay = Detections::Replay {
            log,
            tolerance: None,
        };
        assert_eq!(replay.label(), "replay");
        let mut detectors = build(&c, &infos, &replay).unwrap();
        let p = SyntheticPattern::new(64, 36, 10.0, 1).unwrap();
        let q = [PromptQuery::new("person", "person")];
        let a = detectors[0].as_mut().unwrap();
        assert_eq!(a.name(), "replay");
        assert_eq!(
            a.detect(&p.frame(&SourceId("a".into()), 1), &q)
                .unwrap()
                .len(),
            1
        );
        // Frame 2 of "a" was not recorded; "b" recorded it.
        let miss = a
            .detect(&p.frame(&SourceId("a".into()), 2), &q)
            .unwrap_err();
        assert!(matches!(miss, VisionError::NotRecorded { .. }), "{miss}");
        let b = detectors[1].as_mut().unwrap();
        assert_eq!(
            b.detect(&p.frame(&SourceId("b".into()), 2), &q)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn the_default_tolerance_is_half_a_frame_and_a_stated_one_wins() {
        let (c, infos) = studio(&["a"], DetectorSettings::Synthetic { score: 0.9 });
        let p = SyntheticPattern::new(64, 36, 10.0, 1).unwrap();
        let q = [PromptQuery::new("person", "person")];
        // A pass recorded 40 ms after frame 1 (at 100 ms) of a 10 fps
        // source: within half a frame (50 ms) of it.
        let log = vec![pass("a", 1, 140)];
        let frame = p.frame(&SourceId("a".into()), 1);
        let near = Detections::Replay {
            log: log.clone(),
            tolerance: None,
        };
        let mut d = build(&c, &infos, &near).unwrap().remove(0).unwrap();
        assert!(d.detect(&frame, &q).is_ok());
        // Frame 2, at 200 ms, is 60 ms away.
        assert!(d.detect(&p.frame(&SourceId("a".into()), 2), &q).is_err());
        let exact = Detections::Replay {
            log,
            tolerance: Some(Duration::ZERO),
        };
        let mut d = build(&c, &infos, &exact).unwrap().remove(0).unwrap();
        assert!(d.detect(&frame, &q).is_err());
        assert_eq!(half_frame(10.0), Duration::from_millis(50));
        assert_eq!(half_frame(0.0), Duration::ZERO);
    }

    #[test]
    fn nothing_is_replayed_without_a_detector_and_the_sidecar_is_not_run() {
        let (c, infos) = studio(&["a"], DetectorSettings::None);
        let replay = Detections::Replay {
            log: Vec::new(),
            tolerance: None,
        };
        let err = build(&c, &infos, &replay).err().unwrap();
        assert!(err.contains("no detector"), "{err}");
        assert_eq!(Detections::Live.label(), "live");
    }
}
