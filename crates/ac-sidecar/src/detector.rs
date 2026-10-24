//! [`SidecarDetector`]: the sidecar behind [`ac_vision::Detector`].
//!
//! # Theory
//!
//! The engine asks a detector for queries (an id and a detector phrase,
//! such as "a person speaking"); the sidecar answers phrases. One pass
//! here is one request ([`crate::host::SamHost::detect`]):
//!
//! 1. **Plan.** Queries are grouped by their trimmed phrase, so two
//!    queries reading "a person speaking" cost one phrase on the sidecar
//!    (its time grows with the phrase count: 0.48 s for 1, 0.64 s for 3,
//!    1.05 s for 9 at 672 px, measured). Each query's threshold is
//!    [`DetectorConfig::threshold`] unless
//!    [`DetectorConfig::thresholds`] overrides it (SAM and OWLv2 scores
//!    are on different scales, so the right value is per query: the
//!    speaker was found 10/20 times at 0.5 and 14/20 at 0.3, measured);
//!    a phrase is sent with the lowest threshold of its queries. Queries
//!    with an empty phrase are not sent.
//! 2. **Prepare.** The frame becomes RGB8 with its longer side at most
//!    [`DetectorConfig::send_side`], but its shorter side not below the
//!    model's input side, so the model's own resize never stretches an
//!    axis the host shrank ([`crate::prep`]). The input side comes from
//!    the hello the detector was made after, and then from each answer
//!    (the host reports its process's hello with it), so a detector made
//!    while the first start failed learns it from its first answer and
//!    until then limits by the send side only.
//! 3. **Send and wait,** under [`DetectorConfig::request_timeout`]. The
//!    request carries the frame's [`Freshness`]: taken when `detect` was
//!    called, worth a pass for [`DetectorConfig::max_age`]. A request that
//!    waited longer for the shared process (behind a restart, or behind
//!    other sources) is skipped without a pass.
//! 4. **Map.** Each returned box becomes one [`Detection`] per query of
//!    its phrase whose own threshold it meets. Its extent is
//!    [`Extent::Person`] when the phrase's head noun is a person (the
//!    prompt parser in literal mode, [`ac_core::literal_prompt`], with
//!    the configured lexicon: "a person speaking" is a person, "a guitar"
//!    and "person's upper body" are not), else [`Extent::Object`]: SAM
//!    boxes the whole instance its phrase names. Finally
//!    [`ac_vision::sanitize_detections`] enforces the detector contract:
//!    only requested queries, boxes clipped to the frame, scores in
//!    `0..=1`.
//!
//! Because the image is the whole frame shrunk, boxes normalized to it are
//! normalized source coordinates already.
//!
//! # Errors
//!
//! A missed deadline is [`VisionError::Timeout`]; a sidecar that is down
//! or waiting to restart, and a frame skipped because it waited too long,
//! are [`VisionError::Unavailable`]; an error reply for the request is
//! [`VisionError::Model`]. All are transient: the next call may succeed.
//!
//! # Invariants
//!
//! * No queries (or only empty phrases) means no request and no
//!   detections.
//! * Results satisfy the `ac_vision` detector contract.
//! * The plan and the mapping are pure (`Plan`); only sending waits.

use std::collections::HashMap;
use std::time::Duration;

use ac_core::{Basis, Extent, Lexicon, QueryId, Rect, literal_prompt};
use ac_media::VideoFrame;
use ac_vision::{Detection, Detector, PromptQuery, VisionError, sanitize_detections};

use crate::host::{DetectRequest, Freshness, HostError};
use crate::prep::{DEFAULT_SEND_SIDE, MAX_SEND_SIDE, prepare};
use crate::protocol::{Found, Phrase};
use crate::service::SidecarClient;

/// Default score a detection needs: SAM's usual cut-off.
pub const DEFAULT_THRESHOLD: f32 = 0.5;

/// Default deadline of one pass once sent: about five times the slowest
/// measured pass (1.05 s for 9 phrases at 672 px).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How detections are asked for and filtered.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorConfig {
    /// Score a detection needs, for queries without an override, in
    /// `0..=1`.
    pub threshold: f32,
    /// Per-query scores that replace `threshold`, each in `0..=1`.
    pub thresholds: HashMap<QueryId, f32>,
    /// Longest side of the image sent, in pixels (`1..=4096`), unless that
    /// would take the shorter side below the model's input side; frames
    /// are never enlarged (see [`crate::prep`]).
    pub send_side: u32,
    /// Deadline of one pass, from when it reaches the sidecar.
    pub request_timeout: Duration,
    /// Oldest a frame may be when the sidecar gets to it (see
    /// [`crate::host::Freshness`]); a request that waited longer is
    /// skipped without a pass. `None` sends every request however long it
    /// waited. The engine sets its `discovery.max_result_age_s`, past which
    /// it drops an answer anyway.
    pub max_age: Option<Duration>,
    /// Vocabulary that tells people from objects.
    pub lexicon: Lexicon,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            thresholds: HashMap::new(),
            send_side: DEFAULT_SEND_SIDE,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_age: None,
            lexicon: Lexicon::builtin(),
        }
    }
}

impl DetectorConfig {
    /// Checks the ranges in the field docs.
    pub fn validate(&self) -> Result<(), VisionError> {
        let in_range = |t: f32| t.is_finite() && (0.0..=1.0).contains(&t);
        if !in_range(self.threshold) {
            return Err(VisionError::InvalidConfig(format!(
                "sidecar threshold {} is not in 0..=1",
                self.threshold
            )));
        }
        let mut overrides: Vec<_> = self.thresholds.iter().collect();
        overrides.sort_by(|a, b| a.0.cmp(b.0));
        if let Some((id, t)) = overrides.into_iter().find(|(_, t)| !in_range(**t)) {
            return Err(VisionError::InvalidConfig(format!(
                "sidecar threshold {t} for query {:?} is not in 0..=1",
                id.0
            )));
        }
        if !(1..=MAX_SEND_SIDE).contains(&self.send_side) {
            return Err(VisionError::InvalidConfig(format!(
                "sidecar send_side {} is not in 1..={MAX_SEND_SIDE}",
                self.send_side
            )));
        }
        if self.request_timeout.is_zero() {
            return Err(VisionError::InvalidConfig(
                "sidecar request_timeout must be positive".into(),
            ));
        }
        if self.max_age.is_some_and(|a| a.is_zero()) {
            return Err(VisionError::InvalidConfig(
                "sidecar max_age must be positive".into(),
            ));
        }
        Ok(())
    }

    /// The score query `id` needs.
    pub fn threshold_for(&self, id: &QueryId) -> f32 {
        self.thresholds.get(id).copied().unwrap_or(self.threshold)
    }
}

/// One phrase to send and the queries that asked for it.
#[derive(Debug, Clone, PartialEq)]
struct Planned {
    /// The phrase, trimmed.
    text: String,
    /// What its boxes cover.
    extent: Extent,
    /// The queries, each with its threshold.
    queries: Vec<(QueryId, f32)>,
}

/// The phrases of one pass; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    /// The phrases, in the order of their first query.
    phrases: Vec<Planned>,
}

impl Plan {
    /// Groups `queries` by phrase, with thresholds and extents from
    /// `config`.
    pub(crate) fn new(queries: &[PromptQuery], config: &DetectorConfig) -> Self {
        let mut phrases: Vec<Planned> = Vec::new();
        for q in queries {
            let text = q.prompt.trim();
            if text.is_empty() {
                continue;
            }
            let threshold = config.threshold_for(&q.id);
            match phrases.iter_mut().find(|p| p.text == text) {
                Some(p) => {
                    if !p.queries.iter().any(|(id, _)| *id == q.id) {
                        p.queries.push((q.id.clone(), threshold));
                    }
                }
                None => phrases.push(Planned {
                    text: text.to_string(),
                    extent: extent_of(text, &config.lexicon),
                    queries: vec![(q.id.clone(), threshold)],
                }),
            }
        }
        Self { phrases }
    }

    /// True when there is nothing to send.
    pub(crate) fn is_empty(&self) -> bool {
        self.phrases.is_empty()
    }

    /// The phrases as sent: each with the lowest threshold of its queries.
    pub(crate) fn phrases(&self) -> Vec<Phrase> {
        self.phrases
            .iter()
            .map(|p| Phrase {
                text: p.text.clone(),
                threshold: p.queries.iter().map(|(_, t)| *t).fold(1.0, f32::min),
            })
            .collect()
    }

    /// Turns the sidecar's boxes into detections; see the module docs.
    pub(crate) fn detections(&self, found: &[Found], queries: &[PromptQuery]) -> Vec<Detection> {
        let mut out = Vec::new();
        for f in found {
            let Some(planned) = self.phrases.get(f.phrase as usize) else {
                continue;
            };
            if !f.score.is_finite() {
                continue;
            }
            let score = f.score.clamp(0.0, 1.0);
            let [x, y, w, h] = f.bbox;
            for (query, threshold) in &planned.queries {
                if score >= *threshold {
                    out.push(Detection {
                        query: query.clone(),
                        bbox: Rect::new(x, y, w, h),
                        score,
                        extent: planned.extent,
                    });
                }
            }
        }
        sanitize_detections(out, queries)
    }
}

/// What a box found for `phrase` covers: a whole person when the phrase's
/// head noun is a person.
fn extent_of(phrase: &str, lexicon: &Lexicon) -> Extent {
    match literal_prompt(phrase, lexicon).basis {
        Basis::Person => Extent::Person,
        Basis::Object => Extent::Object,
    }
}

impl From<HostError> for VisionError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Config(m) | HostError::Invalid(m) => VisionError::InvalidConfig(m),
            HostError::Timeout { what, after } => VisionError::Timeout { what, after },
            HostError::Rejected(m) => {
                VisionError::Model(format!("the sidecar rejected the request: {m}"))
            }
            e @ (HostError::Spawn(_)
            | HostError::Unavailable { .. }
            | HostError::Expired { .. }
            | HostError::Stopped) => VisionError::Unavailable(e.to_string()),
        }
    }
}

/// A [`Detector`] answered by the shared sidecar; see the module docs.
#[derive(Debug)]
pub struct SidecarDetector {
    /// The model's name, from its hello.
    name: String,
    /// The model's input side, from the newest hello seen (see the module
    /// docs); `None` before one.
    input_side: Option<u32>,
    /// The service.
    client: SidecarClient,
    /// How detections are asked for and filtered.
    config: DetectorConfig,
}

impl SidecarDetector {
    /// A detector named `name` on `client`, for a model whose input side
    /// is `input_side`; made by [`SidecarClient::detector`] once the model
    /// has loaded (or its first start failed: then both are unknown).
    pub(crate) fn new(
        name: String,
        input_side: Option<u32>,
        client: SidecarClient,
        config: DetectorConfig,
    ) -> Self {
        Self {
            name,
            input_side,
            client,
            config,
        }
    }

    /// The configuration.
    pub fn config(&self) -> &DetectorConfig {
        &self.config
    }
}

impl Detector for SidecarDetector {
    fn name(&self) -> &str {
        &self.name
    }

    fn detect(
        &mut self,
        frame: &VideoFrame,
        queries: &[PromptQuery],
    ) -> Result<Vec<Detection>, VisionError> {
        let freshness = self.config.max_age.map(Freshness::now);
        let plan = Plan::new(queries, &self.config);
        if plan.is_empty() {
            return Ok(Vec::new());
        }
        let image = prepare(frame, self.config.send_side, self.input_side).ok_or_else(|| {
            VisionError::InvalidConfig(format!(
                "frame data of {} bytes does not match {}x{} RGBA",
                frame.data.len(),
                frame.width,
                frame.height
            ))
        })?;
        let request = DetectRequest {
            source: frame.source.0.clone(),
            seq: frame.seq,
            pts: frame.pts,
            image,
            phrases: plan.phrases(),
            freshness,
        };
        let reply = self.client.detect(request, self.config.request_timeout)?;
        self.input_side = reply.input_side.or(self.input_side);
        Ok(plan.detections(&reply.detections, queries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A query.
    fn q(id: &str, prompt: &str) -> PromptQuery {
        PromptQuery::new(id, prompt)
    }

    /// A box for phrase `phrase` at `score`.
    fn found(phrase: u32, score: f32) -> Found {
        Found {
            phrase,
            bbox: [0.1, 0.2, 0.3, 0.4],
            score,
        }
    }

    #[test]
    fn defaults_are_valid() {
        let c = DetectorConfig::default();
        assert!(c.validate().is_ok());
        assert_eq!((c.threshold, c.send_side), (0.5, 1008));
        assert_eq!(c.request_timeout, Duration::from_secs(5));
    }

    #[test]
    fn out_of_range_settings_are_rejected() {
        let base = DetectorConfig::default();
        let cases = [
            DetectorConfig {
                threshold: 1.2,
                ..base.clone()
            },
            DetectorConfig {
                threshold: f32::NAN,
                ..base.clone()
            },
            DetectorConfig {
                thresholds: HashMap::from([(QueryId("speaker".into()), -0.1)]),
                ..base.clone()
            },
            DetectorConfig {
                send_side: 0,
                ..base.clone()
            },
            DetectorConfig {
                send_side: MAX_SEND_SIDE + 1,
                ..base.clone()
            },
            DetectorConfig {
                request_timeout: Duration::ZERO,
                ..base
            },
        ];
        for c in cases {
            assert!(
                matches!(c.validate(), Err(VisionError::InvalidConfig(_))),
                "{c:?}"
            );
        }
    }

    #[test]
    fn overrides_replace_the_default_threshold() {
        let c = DetectorConfig {
            threshold: 0.5,
            thresholds: HashMap::from([(QueryId("speaker".into()), 0.3)]),
            ..DetectorConfig::default()
        };
        assert_eq!(c.threshold_for(&QueryId("speaker".into())), 0.3);
        assert_eq!(c.threshold_for(&QueryId("guitar".into())), 0.5);
    }

    #[test]
    fn queries_sharing_a_phrase_share_one_slot_at_the_lowest_threshold() {
        let c = DetectorConfig {
            thresholds: HashMap::from([(QueryId("pastor".into()), 0.3)]),
            ..DetectorConfig::default()
        };
        let queries = [
            q("speaker", "a person speaking"),
            q("guitar", " a guitar "),
            q("pastor", "a person speaking "),
            q("blank", "   "),
            q("speaker", "a person speaking"),
        ];
        let plan = Plan::new(&queries, &c);
        assert_eq!(
            plan.phrases(),
            [
                Phrase {
                    text: "a person speaking".into(),
                    threshold: 0.3
                },
                Phrase {
                    text: "a guitar".into(),
                    threshold: 0.5
                },
            ]
        );
        assert_eq!(plan.phrases[0].queries.len(), 2, "duplicates once");
    }

    #[test]
    fn nothing_to_send_without_phrases() {
        let c = DetectorConfig::default();
        assert!(Plan::new(&[], &c).is_empty());
        assert!(Plan::new(&[q("a", " "), q("b", "")], &c).is_empty());
    }

    #[test]
    fn each_query_keeps_its_own_threshold() {
        let c = DetectorConfig {
            thresholds: HashMap::from([(QueryId("pastor".into()), 0.3)]),
            ..DetectorConfig::default()
        };
        let queries = [
            q("speaker", "a person speaking"),
            q("pastor", "a person speaking"),
        ];
        let plan = Plan::new(&queries, &c);
        let out = plan.detections(&[found(0, 0.4), found(0, 0.6)], &queries);
        let ids: Vec<&str> = out.iter().map(|d| d.query.0.as_str()).collect();
        assert_eq!(ids, ["pastor", "speaker", "pastor"]);
    }

    #[test]
    fn people_are_whole_people_and_things_are_objects() {
        let c = DetectorConfig::default();
        let queries = [
            q("speaker", "a person speaking"),
            q("guitar", "a guitar"),
            q("body", "person's upper body"),
            q("players", "guitarists"),
        ];
        let plan = Plan::new(&queries, &c);
        let found: Vec<Found> = (0..4).map(|i| found(i, 0.9)).collect();
        let extents: Vec<Extent> = plan
            .detections(&found, &queries)
            .iter()
            .map(|d| d.extent)
            .collect();
        assert_eq!(
            extents,
            [
                Extent::Person,
                Extent::Object,
                Extent::Object,
                Extent::Person
            ]
        );
    }

    #[test]
    fn the_lexicon_decides_who_is_a_person() {
        let mut lexicon = Lexicon::builtin();
        let queries = [q("w", "a worship leader"), q("z", "a zookeeper")];
        let before = Plan::new(
            &queries,
            &DetectorConfig {
                lexicon: lexicon.clone(),
                ..DetectorConfig::default()
            },
        );
        assert_eq!(before.phrases[1].extent, Extent::Object);
        lexicon.add_person_word("zookeeper");
        let after = Plan::new(
            &queries,
            &DetectorConfig {
                lexicon,
                ..DetectorConfig::default()
            },
        );
        assert_eq!(after.phrases[0].extent, Extent::Person);
        assert_eq!(after.phrases[1].extent, Extent::Person);
    }

    #[test]
    fn boxes_are_clamped_and_bad_values_dropped() {
        let c = DetectorConfig {
            threshold: 0.0,
            ..DetectorConfig::default()
        };
        let queries = [q("a", "a guitar")];
        let plan = Plan::new(&queries, &c);
        let raw = [
            Found {
                phrase: 0,
                bbox: [-0.1, 0.9, 0.3, 0.3],
                score: 1.4,
            },
            Found {
                phrase: 0,
                bbox: [1.2, 0.1, 0.1, 0.1],
                score: 0.9,
            },
            Found {
                phrase: 0,
                bbox: [0.1, 0.1, 0.1, 0.1],
                score: f32::NAN,
            },
            Found {
                phrase: 3,
                bbox: [0.1, 0.1, 0.1, 0.1],
                score: 0.9,
            },
            Found {
                phrase: 0,
                bbox: [0.1, 0.1, 0.0, 0.1],
                score: 0.9,
            },
        ];
        let out = plan.detections(&raw, &queries);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].score, 1.0);
        assert!(Rect::FULL.contains_rect(&out[0].bbox));
        assert!((out[0].bbox.w - 0.2).abs() < 1e-9 && (out[0].bbox.h - 0.1).abs() < 1e-9);
    }

    #[test]
    fn only_requested_queries_are_answered() {
        let c = DetectorConfig::default();
        let plan = Plan::new(&[q("a", "a guitar")], &c);
        // Mapped against a narrower query list (the engine removed one).
        assert!(plan.detections(&[found(0, 0.9)], &[q("b", "x")]).is_empty());
    }

    #[test]
    fn host_errors_map_to_vision_errors() {
        let timeout = VisionError::from(HostError::Timeout {
            what: "the sidecar's result".into(),
            after: Duration::from_secs(5),
        });
        assert!(matches!(timeout, VisionError::Timeout { .. }));
        let down = VisionError::from(HostError::Unavailable {
            reason: "crashed".into(),
            retry_in: None,
        });
        assert!(matches!(down, VisionError::Unavailable(m) if m.contains("crashed")));
        assert!(matches!(
            VisionError::from(HostError::Stopped),
            VisionError::Unavailable(_)
        ));
        assert!(matches!(
            VisionError::from(HostError::Rejected("oom".into())),
            VisionError::Model(m) if m.contains("oom")
        ));
        assert!(matches!(
            VisionError::from(HostError::Invalid("x".into())),
            VisionError::InvalidConfig(_)
        ));
    }
}
