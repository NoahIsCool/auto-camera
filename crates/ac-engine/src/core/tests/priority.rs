//! Prompt priority: every director candidate carries the level its
//! subject's highest-priority query stands above the lowest live query, the
//! director then suggests the higher-priority subject, and a studio whose
//! queries all share one priority directs exactly as one without any.

use ac_core::Extent;
use ac_director::ShotCandidate;

use super::*;

/// [`config`] with two subjects and two queries instead of its one: "Band",
/// which the [`one_subject_each`] detector answers with the first subject,
/// and "Pastor", answered with the second, at priorities `band` and
/// `pastor`.
fn band_and_pastor(band: u8, pastor: u8) -> StudioConfig {
    let mut c = config(2);
    c.queries = vec![
        QuerySettings {
            literal: true,
            priority: band,
            ..QuerySettings::new("Band", "band")
        },
        QuerySettings {
            literal: true,
            priority: pastor,
            ..QuerySettings::new("Pastor", "pastor")
        },
    ];
    c
}

/// Answers the prompt "band" with `boxes[0]` and "pastor" with `boxes[1]`,
/// both at score 0.9.
fn one_subject_each(boxes: [Rect; 2]) -> ScriptedDetector {
    ScriptedDetector::from_fn("one-each", move |_, queries| {
        Ok(queries
            .iter()
            .filter_map(|q| {
                let index = ["band", "pastor"].iter().position(|p| *p == q.prompt)?;
                Some(Detection {
                    query: q.id.clone(),
                    bbox: boxes[index],
                    score: 0.9,
                    extent: Extent::Object,
                })
            })
            .collect())
    })
}

/// A rig on `config` whose picture stands still, with the
/// [`one_subject_each`] detector on the two subjects in it, run for 2 s.
fn running(config: StudioConfig) -> Rig {
    let mut rig = Rig::new(config);
    let still = rig.pattern.frame(&cam(), 0);
    let truth = rig.pattern.ground_truth(still.pts);
    rig.still = Some(still);
    let mut rig = rig.with_detector(one_subject_each([truth[0].bbox, truth[1].bbox]));
    rig.run_secs(2.0);
    rig
}

/// The query id `id` names.
fn query(id: &str) -> QueryId {
    QueryId(id.into())
}

/// The director's candidates as (shot id, priority level), sorted by id.
fn levels(rig: &Rig) -> Vec<(String, u8)> {
    let mut levels: Vec<(String, u8)> = rig
        .core
        .director_candidates()
        .into_iter()
        .map(|c| (c.id.0, c.priority))
        .collect();
    levels.sort();
    levels
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
    assert_eq!(levels(&rig), expected);
    // Removing the lowest query raises the floor to the pastor's own level.
    rig.apply(EngineCommand::RemoveQuery {
        query: query("band"),
    })
    .unwrap();
    rig.run(2);
    assert!(levels(&rig).iter().all(|(_, level)| *level == 0));
    // A query added live has the default priority, 3: two below the pastor.
    rig.apply(EngineCommand::AddQuery {
        name: "Choir".into(),
        prompt: "choir".into(),
        presets: None,
        literal: true,
    })
    .unwrap();
    rig.run(2);
    let pastor_shots: Vec<u8> = levels(&rig)
        .into_iter()
        .filter(|(id, _)| id.starts_with(&format!("cam/{}/", pastor.0)))
        .map(|(_, level)| level)
        .collect();
    assert_eq!(pastor_shots, [2, 2]);
}

#[test]
fn a_subject_takes_the_highest_priority_of_the_queries_it_answers() {
    // The ground-truth detector answers both prompts with both subjects, so
    // each subject answers "band" and "pastor": every shot is a pastor's.
    let mut rig = Rig::new(band_and_pastor(1, 4));
    rig.run_secs(2.0);
    let store = rig.core.store(0).unwrap();
    assert_eq!(store.len(), 2);
    assert!(
        store
            .records()
            .all(|r| r.answers(&query("band")) && r.answers(&query("pastor")))
    );
    let levels = levels(&rig);
    assert_eq!(levels.len(), 5);
    for (id, level) in &levels {
        let want = if id == "cam/wide" { 0 } else { 3 };
        assert_eq!(*level, want, "{id}");
    }
}

#[test]
fn the_higher_priority_query_s_subject_is_suggested() {
    for (band, pastor, favourite) in [(1, 5, "pastor"), (5, 1, "band")] {
        let mut c = band_and_pastor(band, pastor);
        c.director.mode = DirectorMode::Suggest;
        let mut rig = running(c);
        preview_and_cut(&mut rig, "cam/wide");
        rig.run_secs(1.0);
        // The two subjects are tracked about equally well: closer than the
        // four levels (0.2) between their queries, either way round.
        let (b, p) = (subject_of(&rig, "band"), subject_of(&rig, "pastor"));
        let gap = (best_quality(&rig, b) - best_quality(&rig, p)).abs();
        assert!(gap < 0.2, "quality gap {gap}");
        let suggestion = rig.core.decision().suggestion.clone().unwrap();
        let subject = rig.core.catalog().get(&suggestion).unwrap().subject;
        assert_eq!(subject, Some(subject_of(&rig, favourite)), "{suggestion:?}");
    }
}

/// One tick of a run: the director's candidates, its decision and the
/// program shot.
type Tick = (Vec<ShotCandidate>, DirectorDecision, Option<ShotId>);

/// Every tick of 20 s of unattended (Auto, music) directing of
/// [`band_and_pastor`] at `band` and `pastor`.
fn auto_run(band: u8, pastor: u8) -> Vec<Tick> {
    let mut c = band_and_pastor(band, pastor);
    c.director.mode = DirectorMode::Auto;
    c.director.profile = ContentProfile::Music;
    let mut rig = running(c);
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
    // A config without priorities gives both queries the default, 3.
    let unranked = auto_run(
        QuerySettings::DEFAULT_PRIORITY,
        QuerySettings::DEFAULT_PRIORITY,
    );
    assert!(
        unranked
            .iter()
            .all(|(cands, _, _)| cands.iter().all(|c| c.priority == 0))
    );
    let takes = unranked.windows(2).filter(|w| w[0].2 != w[1].2).count();
    assert!(takes >= 3, "the run switches between shots ({takes} takes)");
    // Same candidates (so the same scores), decisions and program, tick by
    // tick, at either end of the range.
    for level in [QuerySettings::MIN_PRIORITY, QuerySettings::MAX_PRIORITY] {
        let ranked = auto_run(level, level);
        assert!(ranked == unranked, "both at {level} directs differently");
    }
    // Unequal priorities do change the run, so the comparison can fail.
    assert!(auto_run(1, 5) != unranked);
}
