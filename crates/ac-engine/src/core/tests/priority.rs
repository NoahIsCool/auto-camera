//! Prompt priority: every director candidate carries the level its
//! subject's primary query stands above the lowest priority among the
//! queries that have subjects, the director then suggests the
//! higher-priority subject, a stray alias box does not lift the wrong
//! person, a config without priorities is ranked by its list order, and a
//! studio whose queries all share one priority directs exactly as one
//! without any.

use ac_core::Extent;
use ac_director::{DirectorConfig, ScoreWeights, ShotCandidate};

use super::*;
use crate::config::SourceEntry;

/// [`config`] with two subjects and two queries instead of its one: "Band",
/// which the [`one_subject_each`] detector answers with the first subject,
/// and "Pastor", answered with the second, at priorities `band` and
/// `pastor`.
fn band_and_pastor(band: u8, pastor: u8) -> StudioConfig {
    let mut c = config(2);
    c.queries = vec![
        QuerySettings {
            literal: true,
            priority: Some(band),
            ..QuerySettings::new("Band", "band")
        },
        QuerySettings {
            literal: true,
            priority: Some(pastor),
            ..QuerySettings::new("Pastor", "pastor")
        },
    ];
    c
}

/// A detection of `query` at `bbox` with `score`.
fn detection(query: &QueryId, bbox: Rect, score: f32) -> Detection {
    Detection {
        query: query.clone(),
        bbox,
        score,
        extent: Extent::Object,
    }
}

/// Answers the prompt "band" with `boxes[0]` and "pastor" with `boxes[1]`,
/// both at score 0.9. On its pass number `stray` (counted from 1), if any,
/// it also answers "pastor" with `boxes[0]` at score 0.4: one stray box of
/// the pastor's prompt on the band member.
fn one_subject_each(boxes: [Rect; 2], stray: Option<usize>) -> ScriptedDetector {
    let mut passes = 0;
    ScriptedDetector::from_fn("one-each", move |_, queries| {
        passes += 1;
        let mut out: Vec<Detection> = queries
            .iter()
            .filter_map(|q| {
                let index = ["band", "pastor"].iter().position(|p| *p == q.prompt)?;
                Some(detection(&q.id, boxes[index], 0.9))
            })
            .collect();
        if stray == Some(passes)
            && let Some(q) = queries.iter().find(|q| q.prompt == "pastor")
        {
            out.push(detection(&q.id, boxes[0], 0.4));
        }
        Ok(out)
    })
}

/// A rig on `config` whose picture stands still, with the
/// [`one_subject_each`] detector (and its `stray` pass) on the two
/// subjects in it; not run yet.
fn still_rig(config: StudioConfig, stray: Option<usize>) -> Rig {
    let mut rig = Rig::new(config);
    let still = rig.pattern.frame(&cam(), 0);
    let truth = rig.pattern.ground_truth(still.pts);
    rig.still = Some(still);
    rig.with_detector(one_subject_each([truth[0].bbox, truth[1].bbox], stray))
}

/// [`still_rig`] without a stray box, run for 2 s.
fn running(config: StudioConfig) -> Rig {
    let mut rig = still_rig(config, None);
    rig.run_secs(2.0);
    rig
}

/// The query id `id` names.
fn query(id: &str) -> QueryId {
    QueryId(id.into())
}

/// The director's candidates as (shot id, priority level), sorted by id.
fn levels(core: &EngineCore) -> Vec<(String, u8)> {
    let mut levels: Vec<(String, u8)> = core
        .director_candidates()
        .into_iter()
        .map(|c| (c.id.0, c.priority))
        .collect();
    levels.sort();
    levels
}

/// The levels of the shots of `subject` on the rig's source.
fn subject_levels(rig: &Rig, subject: SubjectId) -> Vec<u8> {
    let prefix = format!("cam/{}/", subject.0);
    levels(&rig.core)
        .into_iter()
        .filter(|(id, _)| id.starts_with(&prefix))
        .map(|(_, level)| level)
        .collect()
}

/// The subject of source 0 whose primary query is `id`.
fn subject_of(rig: &Rig, id: &str) -> SubjectId {
    let store = rig.core.store(0).unwrap();
    let found: Vec<SubjectId> = store
        .records()
        .filter(|r| r.subject.query == query(id))
        .map(|r| r.id())
        .collect();
    assert_eq!(found.len(), 1, "one subject answers {id}");
    found[0]
}

/// The best quality among the shots of `subject`.
fn best_quality(rig: &Rig, subject: SubjectId) -> f32 {
    rig.core
        .catalog()
        .entries()
        .iter()
        .filter(|e| e.subject == Some(subject))
        .map(|e| e.quality)
        .fold(0.0, f32::max)
}

/// The subject of the director's current suggestion.
fn suggested_subject(rig: &Rig) -> Option<SubjectId> {
    let suggestion = rig.core.decision().suggestion.clone()?;
    rig.core.catalog().get(&suggestion)?.subject
}

#[test]
fn candidates_carry_the_level_of_their_subject_s_query() {
    let mut rig = running(band_and_pastor(2, 5));
    let (band, pastor) = (subject_of(&rig, "band"), subject_of(&rig, "pastor"));
    let mut expected = vec![
        ("cam/wide".to_string(), 0),
        (format!("cam/{}/medium", band.0), 0),
        (format!("cam/{}/close", band.0), 0),
        (format!("cam/{}/medium", pastor.0), 3),
        (format!("cam/{}/close", pastor.0), 3),
    ];
    expected.sort();
    assert_eq!(levels(&rig.core), expected);
    // Removing the lowest query raises the floor to the pastor's own level.
    rig.apply(EngineCommand::RemoveQuery {
        query: query("band"),
    })
    .unwrap();
    rig.run(2);
    assert!(levels(&rig.core).iter().all(|(_, level)| *level == 0));
    // A query added live (at the default, 3, two below the pastor) that
    // finds no one does not lower the floor.
    rig.apply(EngineCommand::AddQuery {
        name: "Choir".into(),
        prompt: "choir".into(),
        presets: None,
        literal: true,
    })
    .unwrap();
    assert_eq!(rig.core.priorities[&query("choir")], 3);
    rig.run_secs(2.0);
    assert!(levels(&rig.core).iter().all(|(_, level)| *level == 0));
}

#[test]
fn a_query_that_finds_no_one_moves_no_level() {
    // Pastor and Band at 5, and a Baptism prompt at 1 that matches nothing
    // all service: every shot scores as with Pastor and Band alone.
    let mut c = band_and_pastor(5, 5);
    c.queries.push(QuerySettings {
        literal: true,
        priority: Some(1),
        ..QuerySettings::new("Baptism", "baptism")
    });
    let rig = running(c);
    assert_eq!(rig.core.store(0).unwrap().len(), 2);
    assert!(levels(&rig.core).iter().all(|(_, level)| *level == 0));
}

#[test]
fn a_subject_s_level_is_its_primary_query_s_not_an_alias_s() {
    // The ground-truth detector answers both prompts with both subjects,
    // so each subject answers "band" and "pastor", one as primary query.
    let mut rig = Rig::new(band_and_pastor(1, 4));
    rig.run_secs(2.0);
    let store = rig.core.store(0).unwrap();
    assert_eq!(store.len(), 2);
    assert!(
        store
            .records()
            .all(|r| r.answers(&query("band")) && r.answers(&query("pastor")))
    );
    let (band, pastor) = (subject_of(&rig, "band"), subject_of(&rig, "pastor"));
    assert_eq!(subject_levels(&rig, band), [0, 0]);
    assert_eq!(subject_levels(&rig, pastor), [3, 3]);
}

#[test]
fn one_stray_box_of_a_higher_prompt_does_not_raise_the_wrong_subject() {
    // The detector's third pass also puts a pastor box, at 0.4, on the band
    // member, which makes "pastor" one of the band member's aliases for good.
    let mut c = band_and_pastor(1, 5);
    c.director.mode = DirectorMode::Suggest;
    let mut rig = still_rig(c, Some(3));
    // Passes run once a second: the third is answered just after 2 s.
    rig.run_secs(3.0);
    let (band, pastor) = (subject_of(&rig, "band"), subject_of(&rig, "pastor"));
    let store = rig.core.store(0).unwrap();
    assert!(store.record(band).unwrap().answers(&query("pastor")));
    // The band member stays at the band's level, under the pastor.
    assert_eq!(subject_levels(&rig, band), [0, 0]);
    assert_eq!(subject_levels(&rig, pastor), [4, 4]);
    preview_and_cut(&mut rig, "cam/wide");
    rig.run_secs(10.0);
    assert_eq!(subject_levels(&rig, band), [0, 0]);
    assert_eq!(suggested_subject(&rig), Some(pastor));
}

#[test]
fn four_levels_outweigh_a_smaller_quality_lead() {
    let suggest = |band, pastor| {
        let mut c = band_and_pastor(band, pastor);
        c.director.mode = DirectorMode::Suggest;
        let mut rig = running(c);
        preview_and_cut(&mut rig, "cam/wide");
        rig.run_secs(1.0);
        let (b, p) = (subject_of(&rig, "band"), subject_of(&rig, "pastor"));
        let lead = best_quality(&rig, b) - best_quality(&rig, p);
        (lead, suggested_subject(&rig) == Some(p))
    };
    // The band member is tracked better, by more than the margin but less
    // than four levels (0.2), so at equal priorities the band is suggested.
    let (lead, pastor) = suggest(3, 3);
    assert!(lead > 0.1 && lead < 0.2, "the band leads by {lead}");
    assert!(!pastor);
    // Four levels up, the pastor is suggested despite that lead.
    let (lead, pastor) = suggest(1, 5);
    assert!(lead > 0.1 && lead < 0.2, "the band leads by {lead}");
    assert!(pastor);
}

/// Two sources, "a" and "b", showing the same still picture, with a
/// detector that answers the prompt `prompts[i]` on source `i` only, with
/// the same box at score 0.9: one subject per source, tracked and framed
/// alike to the bit, each found by a different query.
struct Twins {
    core: EngineCore,
    /// The picture both sources repeat.
    still: VideoFrame,
    /// The clock of the picture.
    pattern: SyntheticPattern,
    /// Where the subject stands.
    bbox: Rect,
    /// The prompt each source's detector answers.
    prompts: [&'static str; 2],
    /// Results to apply on the next tick.
    pending: Vec<DiscoveryResult>,
}

impl Twins {
    /// Twins for `config` (whose source list is replaced) and `prompts`.
    fn new(config: StudioConfig, prompts: [&'static str; 2]) -> Self {
        let mut c = config;
        let first = c.sources[0].clone();
        c.sources = ["a", "b"]
            .into_iter()
            .map(|id| SourceEntry {
                id: id.into(),
                ..first.clone()
            })
            .collect();
        let infos = c
            .sources
            .iter()
            .map(|s| SourceInfo {
                id: s.source_id(),
                width: W,
                height: H,
                fps: FPS,
                audio: None,
            })
            .collect();
        let core = EngineCore::new(&c, infos, &[true, true]).unwrap();
        let pattern = SyntheticPattern::new(W, H, FPS, 2).unwrap();
        let still = pattern.frame(&cam(), 0);
        let bbox = pattern.ground_truth(still.pts)[1].bbox;
        Self {
            core,
            still,
            pattern,
            bbox,
            prompts,
            pending: Vec::new(),
        }
    }

    /// Runs `secs` seconds of ticks, answering each request on the next.
    fn run_secs(&mut self, secs: f64) {
        for _ in 0..(secs * FPS).round() as usize {
            let tick = self.core.ticks();
            let now = Duration::from_secs_f64(tick as f64 / FPS);
            let feeds: Vec<SourceFeed> = self
                .core
                .source_ids()
                .into_iter()
                .map(|source| SourceFeed {
                    state: SourceState::Running,
                    frame: Some(VideoFrame {
                        source,
                        seq: tick,
                        pts: self.pattern.frame_rate().pts(tick),
                        ..self.still.clone()
                    }),
                    detector_loading: false,
                })
                .collect();
            let results = std::mem::take(&mut self.pending);
            let out = self.core.tick(now, &feeds, results);
            for (i, req) in out.requests {
                let found = req
                    .queries
                    .iter()
                    .filter(|q| q.prompt == self.prompts[i])
                    .map(|q| detection(&q.id, self.bbox, 0.9))
                    .collect();
                self.pending.push(DiscoveryResult {
                    source: i,
                    queries: req.queries.iter().map(|q| q.id.clone()).collect(),
                    frame: req.frame,
                    outcome: Ok(found),
                    latency: Duration::ZERO,
                });
            }
        }
    }

    /// The (preset, quality) of every subject shot of source `prefix`.
    fn qualities(&self, prefix: &str) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = self
            .core
            .catalog()
            .entries()
            .iter()
            .filter(|e| e.subject.is_some() && e.shot.id.0.starts_with(prefix))
            .map(|e| (e.preset.as_ref().unwrap().id.clone(), e.quality))
            .collect();
        out.sort_by(|x, y| x.0.cmp(&y.0));
        out
    }
}

#[test]
fn equal_quality_suggests_the_higher_priority_query_s_shot() {
    // Source "b" is listed second, so it loses an exact tie: only its
    // query's priority can win it. Both ways round: band on "a" and
    // pastor on "b", then the other way.
    for (prompts, ranked) in [(["band", "pastor"], (1, 5)), (["pastor", "band"], (5, 1))] {
        let suggested = |(band, pastor)| {
            let mut c = band_and_pastor(band, pastor);
            c.director.mode = DirectorMode::Suggest;
            let mut twins = Twins::new(c, prompts);
            twins.run_secs(2.0);
            twins
                .core
                .apply(&EngineCommand::SetPreview {
                    shot: shot("a/wide"),
                })
                .unwrap();
            twins.core.apply(&EngineCommand::Cut).unwrap();
            twins.run_secs(1.0);
            let (a, b) = (twins.qualities("a/"), twins.qualities("b/"));
            assert_eq!(a.len(), 2, "{a:?}");
            assert_eq!(a, b, "equal quality");
            let suggestion = twins.core.decision().suggestion.clone().unwrap();
            assert!(
                twins
                    .core
                    .catalog()
                    .get(&suggestion)
                    .unwrap()
                    .subject
                    .is_some()
            );
            suggestion.0.split('/').next().unwrap().to_string()
        };
        assert_eq!(suggested((3, 3)), "a", "{prompts:?}: the tie goes to a");
        assert_eq!(suggested(ranked), "b", "{prompts:?} at {ranked:?}");
    }
}

#[test]
fn without_priorities_the_list_order_ranks_the_subjects() {
    let unranked = |mut c: StudioConfig| {
        for q in &mut c.queries {
            q.priority = None;
        }
        c
    };
    // Band listed first (5), Pastor second (4).
    let mut rig = running(unranked(band_and_pastor(3, 3)));
    let (band, pastor) = (subject_of(&rig, "band"), subject_of(&rig, "pastor"));
    assert_eq!(subject_levels(&rig, band), [1, 1]);
    assert_eq!(subject_levels(&rig, pastor), [0, 0]);
    // A query added live joins the end of the list, below the pastor.
    rig.apply(EngineCommand::AddQuery {
        name: "Choir".into(),
        prompt: "choir".into(),
        presets: None,
        literal: true,
    })
    .unwrap();
    assert_eq!(rig.core.priorities[&query("choir")], 3);
    // Listed the other way round, the pastor ranks first.
    let mut c = unranked(band_and_pastor(3, 3));
    c.queries.reverse();
    let rig = running(c);
    let (band, pastor) = (subject_of(&rig, "band"), subject_of(&rig, "pastor"));
    assert_eq!(subject_levels(&rig, band), [0, 0]);
    assert_eq!(subject_levels(&rig, pastor), [1, 1]);
}

/// One tick of a run: the director's candidates, its decision and the
/// program shot.
type Tick = (Vec<ShotCandidate>, DirectorDecision, Option<ShotId>);

/// Every tick of 20 s of unattended (Auto, music) directing of `config`,
/// with the director's `priority_weight` set to `weight`.
fn auto_run(config: StudioConfig, weight: f32) -> Vec<Tick> {
    let mut c = config;
    c.director.mode = DirectorMode::Auto;
    c.director.profile = ContentProfile::Music;
    let mut rig = still_rig(c, None);
    let scoring = ScoreWeights {
        priority_weight: weight,
        ..rig.core.director.config().scoring
    };
    rig.core
        .director
        .set_config(DirectorConfig {
            scoring,
            ..rig.core.director.config().clone()
        })
        .unwrap();
    rig.run_secs(2.0);
    (0..(20.0 * FPS) as usize)
        .map(|_| {
            rig.step();
            (
                rig.core.director_candidates(),
                rig.core.decision().clone(),
                rig.core.switcher().program().cloned(),
            )
        })
        .collect()
}

#[test]
fn equal_priorities_everywhere_direct_exactly_as_without_priorities() {
    // Today's scoring: the director ignores priority levels, here those of
    // the list order of an unranked config.
    let mut c = band_and_pastor(3, 3);
    for q in &mut c.queries {
        q.priority = None;
    }
    let mut today = auto_run(c, 0.0);
    assert!(
        today
            .iter()
            .any(|(cands, _, _)| cands.iter().any(|c| c.priority > 0))
    );
    let takes = today.windows(2).filter(|w| w[0].2 != w[1].2).count();
    assert!(takes >= 3, "the run switches between shots ({takes} takes)");
    for (cands, _, _) in &mut today {
        for c in cands {
            c.priority = 0;
        }
    }
    // Same candidates (so the same scores), decisions and program, tick by
    // tick, at either end of the range and in the middle.
    let weight = ScoreWeights::default().priority_weight;
    for level in [1, 3, 5] {
        let ranked = auto_run(band_and_pastor(level, level), weight);
        assert!(ranked == today, "both at {level} directs differently");
    }
    // Unequal priorities do change the run, so the comparison can fail.
    assert!(auto_run(band_and_pastor(1, 5), weight) != today);
}
