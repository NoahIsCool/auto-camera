//! Tests for people-first prompt matching, on synthetic boxes.

use ac_core::{QueryId, Rect};

use super::*;

/// A box from corner coordinates.
fn r(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// A model detection for model query `id`.
fn det(id: &str, bbox: Rect, score: f32) -> Detection {
    Detection {
        query: QueryId(id.into()),
        bbox,
        score,
    }
}

/// A plan for `prompts` (ids `q0`, `q1`, …) at the default thresholds:
/// objects 0.3, people 0.15.
fn plan(prompts: &[&str]) -> MatchPlan {
    let queries: Vec<PromptQuery> = prompts
        .iter()
        .enumerate()
        .map(|(i, p)| PromptQuery::new(&format!("q{i}"), p))
        .collect();
    MatchPlan::new(&queries, |_| None, MatchParams::default())
}

/// The id the plan gave the model query asking for `prompt`.
fn model_id(plan: &MatchPlan, prompt: &str) -> String {
    plan.queries()
        .iter()
        .find(|q| q.prompt == prompt)
        .unwrap_or_else(|| panic!("no model query {prompt:?}"))
        .id
        .0
        .clone()
}

/// A stage like the band clip: three guitarists, a singer between them.
fn stage() -> Vec<(Rect, f32)> {
    vec![
        (r(0.19, 0.60, 0.26, 0.94), 0.20), // guitarist L
        (r(0.40, 0.67, 0.45, 0.92), 0.24), // singer
        (r(0.57, 0.62, 0.62, 0.88), 0.22), // guitarist M
        (r(0.71, 0.61, 0.77, 0.91), 0.18), // guitarist R
    ]
}

#[test]
fn prompts_parse_into_the_object_they_name() {
    assert_eq!(
        selector("a person playing guitar"),
        Selector::Object("a guitar".into())
    );
    assert_eq!(
        selector("A person speaking at a  Lectern"),
        Selector::Object("a lectern".into())
    );
    assert_eq!(
        selector("a man in a red shirt"),
        Selector::Object("a red shirt".into())
    );
    assert_eq!(
        selector("a woman holding a painting"),
        Selector::Object("a painting".into()),
        "an -ing noun after an article is the object"
    );
    assert_eq!(
        selector("someone standing in front of the drums"),
        Selector::Object("a drums".into())
    );
}

#[test]
fn prompts_without_an_object_are_their_own_object_or_everyone() {
    assert_eq!(selector("a person"), Selector::Everyone);
    assert_eq!(selector("People"), Selector::Everyone);
    assert_eq!(selector("a woman"), Selector::Described("a woman".into()));
    assert_eq!(
        selector("a guitarist"),
        Selector::Object("a guitarist".into())
    );
    assert_eq!(
        selector("a person speaking"),
        Selector::Described("a person speaking".into())
    );
    assert_eq!(
        selector("a tall person"),
        Selector::Described("a tall person".into())
    );
    assert_eq!(selector(""), Selector::Object(String::new()));
}

#[test]
fn another_person_is_never_the_object() {
    assert_eq!(
        selector("a man standing next to a woman"),
        Selector::Described("a man standing next to a woman".into())
    );
    assert_eq!(
        selector("a person with a child"),
        Selector::Described("a person with a child".into())
    );
    assert_eq!(
        selector("a person with a microphone"),
        Selector::Object("a microphone".into()),
        "an ordinary object is still an object"
    );
}

#[test]
fn typographic_apostrophes_are_apostrophes() {
    assert_eq!(
        selector("a person\u{2019}s face"),
        selector("a person's face")
    );
    assert_eq!(
        selector("the drummer\u{2019}s sticks"),
        Selector::Object("the drummer's sticks".into())
    );
}

#[test]
fn the_plan_asks_for_people_first_and_each_object_once() {
    let p = plan(&[
        "a person playing guitar",
        "a guitarist",
        "a man with a guitar",
        "a person",
    ]);
    let prompts: Vec<&str> = p.queries().iter().map(|q| q.prompt.as_str()).collect();
    assert_eq!(prompts, ["a person", "a guitar", "a guitarist"]);
    assert_eq!(p.thresholds(), [0.15, 0.3, 0.3]);
}

#[test]
fn a_shared_object_decodes_at_the_lowest_threshold() {
    let queries = [
        PromptQuery::new("a", "a person playing guitar"),
        PromptQuery::new("b", "a man with a guitar"),
    ];
    let p = MatchPlan::new(
        &queries,
        |id| Some(if id.0 == "a" { 0.4 } else { 0.25 }),
        MatchParams::default(),
    );
    assert_eq!(p.thresholds(), [0.15, 0.25]);
}

#[test]
fn guitars_pick_their_players_whole_bodies() {
    let p = plan(&["a person playing guitar"]);
    let (person, guitar) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a guitar"));
    let mut dets: Vec<Detection> = stage()
        .into_iter()
        .map(|(b, s)| det(&person, b, s))
        .collect();
    // Guitars stick out sideways: about half of the middle one is inside.
    dets.push(det(&guitar, r(0.20, 0.63, 0.28, 0.75), 0.63));
    dets.push(det(&guitar, r(0.58, 0.68, 0.66, 0.76), 0.71));
    dets.push(det(&guitar, r(0.71, 0.65, 0.80, 0.77), 0.66));
    let out = p.assign(&dets);
    assert_eq!(out.len(), 3);
    for d in &out {
        assert_eq!(d.query, QueryId("q0".into()));
        assert!(
            stage().iter().any(|(b, _)| *b == d.bbox),
            "the box is a person's"
        );
        assert_ne!(d.bbox, stage()[1].0, "the singer has no guitar");
    }
    assert_eq!(out[0].score, 0.71, "highest object score first");
}

#[test]
fn objects_away_from_people_pick_no_one() {
    let p = plan(&["a person playing guitar"]);
    let (person, guitar) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a guitar"));
    let dets = vec![
        det(&person, r(0.40, 0.60, 0.48, 0.95), 0.22),
        det(&guitar, r(0.60, 0.70, 0.64, 0.85), 0.70), // on a stand
        det(&guitar, r(0.10, 0.10, 0.20, 0.30), 0.40), // in a photo
    ];
    assert!(p.assign(&dets).is_empty());
}

#[test]
fn weak_objects_and_weak_people_are_ignored() {
    let p = plan(&["a person playing guitar"]);
    let (person, guitar) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a guitar"));
    let body = r(0.40, 0.60, 0.48, 0.95);
    let weak_object = vec![
        det(&person, body, 0.22),
        det(&guitar, r(0.41, 0.7, 0.47, 0.8), 0.29),
    ];
    assert!(p.assign(&weak_object).is_empty());
    let weak_person = vec![
        det(&person, body, 0.14),
        det(&guitar, r(0.41, 0.7, 0.47, 0.8), 0.6),
    ];
    assert!(p.assign(&weak_person).is_empty());
}

#[test]
fn an_object_goes_to_the_person_holding_most_of_it() {
    let p = plan(&["a person playing guitar"]);
    let (person, guitar) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a guitar"));
    let left = r(0.30, 0.50, 0.40, 0.95);
    let right = r(0.38, 0.50, 0.48, 0.95);
    let dets = vec![
        det(&person, left, 0.25),
        det(&person, right, 0.20),
        det(&guitar, r(0.385, 0.6, 0.45, 0.7), 0.6), // mostly inside `right`
    ];
    let out = p.assign(&dets);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].bbox, right);
}

#[test]
fn everyone_prompts_return_every_person_above_their_threshold() {
    let p = plan(&["a person"]);
    assert_eq!(p.queries().len(), 1, "no object query");
    let person = model_id(&p, PERSON_PROMPT);
    let mut dets: Vec<Detection> = stage()
        .into_iter()
        .map(|(b, s)| det(&person, b, s))
        .collect();
    dets.push(det(&person, r(0.0, 0.0, 0.1, 0.1), 0.16)); // below the prompt's 0.3
    let p = MatchPlan::new(
        &[PromptQuery::new("q0", "a person")],
        |_| Some(0.19),
        MatchParams::default(),
    );
    let out = p.assign(&dets);
    assert_eq!(
        out.len(),
        3,
        "0.20, 0.24 and 0.22 pass 0.19; 0.18 and 0.16 do not"
    );
    assert!(out.windows(2).all(|w| w[0].score >= w[1].score));
}

#[test]
fn people_prompts_use_the_person_threshold_by_default() {
    let p = plan(&["a person", "a woman", "a person speaking"]);
    let prompts: Vec<&str> = p.queries().iter().map(|q| q.prompt.as_str()).collect();
    assert_eq!(prompts, ["a person", "a woman", "a person speaking"]);
    assert_eq!(p.thresholds(), [0.15, 0.15, 0.15]);
    let (person, woman) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a woman"));
    let mut dets: Vec<Detection> = stage()
        .into_iter()
        .map(|(b, s)| det(&person, b, s))
        .collect();
    // OWLv2 scores a fitting description of a person about this low.
    dets.push(det(&woman, r(0.575, 0.62, 0.62, 0.88), 0.22));
    let out = p.assign(&dets);
    let everyone = out.iter().filter(|d| d.query.0 == "q0").count();
    assert_eq!(everyone, 4, "all four people score at least 0.15");
    let women: Vec<_> = out.iter().filter(|d| d.query.0 == "q1").collect();
    assert_eq!(women.len(), 1);
    assert_eq!(women[0].bbox, stage()[2].0);
    assert!(
        out.iter().all(|d| d.query.0 != "q2"),
        "nothing scored as speaking"
    );
}

/// What OWLv2 does with an action (see the module's limits): "a person
/// speaking" scores every person alike, about 0.18–0.20, so the described
/// query picks every person, as "a person" does, ranked by its own score.
#[test]
fn a_description_on_every_person_picks_every_person_ranked_by_its_score() {
    let p = plan(&["a person speaking", "a person"]);
    let (person, speaking) = (
        model_id(&p, PERSON_PROMPT),
        model_id(&p, "a person speaking"),
    );
    let people = [
        r(0.10, 0.30, 0.20, 0.90),
        r(0.30, 0.30, 0.40, 0.90),
        r(0.50, 0.30, 0.60, 0.90),
        r(0.70, 0.30, 0.80, 0.90),
        r(0.85, 0.05, 0.90, 0.20),
    ];
    // In another order than the person scores.
    let speaking_scores = [0.19, 0.18, 0.20, 0.185, 0.195];
    let mut dets = Vec::new();
    for (i, (bbox, s)) in people.iter().zip(speaking_scores).enumerate() {
        dets.push(det(&person, *bbox, 0.25 + 0.01 * i as f32));
        // OWLv2's boxes do not depend on the query: the same box.
        dets.push(det(&speaking, *bbox, s));
    }
    let out = p.assign(&dets);
    let speakers: Vec<&Detection> = out.iter().filter(|d| d.query.0 == "q0").collect();
    assert_eq!(speakers.len(), 5, "everyone is speaking: {out:?}");
    assert_eq!(
        out.iter().filter(|d| d.query.0 == "q1").count(),
        5,
        "as many as a person"
    );
    let order: Vec<Rect> = speakers.iter().map(|d| d.bbox).collect();
    assert_eq!(
        order,
        [people[2], people[4], people[0], people[3], people[1]],
        "ranked by the speaking score"
    );
    let scores: Vec<f32> = speakers.iter().map(|d| d.score).collect();
    assert_eq!(scores, [0.20, 0.195, 0.19, 0.185, 0.18]);
}

#[test]
fn a_person_inside_an_object_larger_than_them_holds_it() {
    let p = plan(&["a person behind a drum kit"]);
    let (person, kit) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a drum kit"));
    let drummer = r(0.45, 0.40, 0.55, 0.80);
    let dets = vec![
        det(&person, drummer, 0.21),
        det(&person, r(0.75, 0.40, 0.85, 0.90), 0.23), // away from the kit
        // A kit twice the drummer's size: under a third of it overlaps them.
        det(&kit, r(0.38, 0.55, 0.62, 0.90), 0.64),
    ];
    let out = p.assign(&dets);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].bbox, drummer);
    assert_eq!(out[0].score, 0.64);
}

#[test]
fn contained_fraction_is_measured_against_the_smaller_box() {
    let big = r(0.0, 0.0, 1.0, 1.0);
    let small = r(0.2, 0.2, 0.4, 0.4);
    assert_eq!(contained_fraction(&small, &big), 1.0);
    assert_eq!(contained_fraction(&big, &small), 1.0);
    let half = r(0.3, 0.2, 0.5, 0.4);
    assert!((contained_fraction(&half, &small) - 0.5).abs() < 1e-9);
    let zero = r(0.5, 0.5, 0.5, 0.5);
    assert_eq!(contained_fraction(&zero, &big), 0.0);
}

/// A "person speaking" box equal to a whole person also contains a head box
/// nested in them, small enough to survive NMS at 0.3. Both hold it fully;
/// rounding makes the head's share the larger, but the tie goes to the
/// higher-scored whole person.
#[test]
fn a_containment_tie_goes_to_the_higher_scored_person_not_to_rounding() {
    let p = plan(&["a person speaking"]);
    let (person, speaking) = (
        model_id(&p, PERSON_PROMPT),
        model_id(&p, "a person speaking"),
    );
    let full = Rect::new(0.2, 0.15, 0.13, 0.62);
    let head = Rect::new(0.221, 0.15, 0.07, 0.15);
    assert!(head.iou(&full) < 0.3, "the head box survives NMS");
    assert!(
        contained_fraction(&full, &head) > contained_fraction(&full, &full),
        "rounding favours the head"
    );
    let dets = vec![
        det(&person, full, 0.5),
        det(&person, head, 0.3),
        det(&speaking, full, 0.25),
    ];
    let out = p.assign(&dets);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].bbox, full);
}

/// Over a grid of head boxes nested in a whole person, a box containing
/// both always goes to whichever of the two scored higher as a person.
#[test]
fn exact_containment_ties_always_go_to_the_higher_scored_person() {
    let p = plan(&["a person speaking"]);
    let (person, speaking) = (
        model_id(&p, PERSON_PROMPT),
        model_id(&p, "a person speaking"),
    );
    let mut rounded = 0;
    for fx in 0..10 {
        for hx in 0..10 {
            for hy in 0..10 {
                let full = Rect::new(0.20 + 0.013 * f64::from(fx), 0.15, 0.13, 0.62);
                let head = Rect::new(
                    full.x + 0.003 * f64::from(hx),
                    full.y + 0.004 * f64::from(hy),
                    0.07,
                    0.15,
                );
                if contained_fraction(&full, &head) != contained_fraction(&full, &full) {
                    rounded += 1;
                }
                for (full_score, head_score) in [(0.5, 0.3), (0.3, 0.5)] {
                    let dets = vec![
                        det(&person, full, full_score),
                        det(&person, head, head_score),
                        det(&speaking, full, 0.25),
                    ];
                    let out = p.assign(&dets);
                    let want = if full_score > head_score { full } else { head };
                    assert_eq!(out.len(), 1, "{out:?}");
                    assert_eq!(
                        out[0].bbox, want,
                        "full {full:?} at {full_score}, head {head:?} at {head_score}"
                    );
                }
            }
        }
    }
    assert!(rounded > 0, "the grid has ties that rounding breaks");
}

/// The tie margin is rounding-sized: a person holding even slightly more of
/// an object still gets it over a higher-scored one; equal shares go to the
/// higher score, and shares under `min_overlap` to no one.
#[test]
fn only_rounding_sized_differences_are_ties() {
    let object = r(0.2, 0.0, 0.6, 1.0);
    // 0.5 of the object against 0.500025.
    let people = [
        (r(0.0, 0.0, 0.4, 1.0), 0.5),
        (r(0.39999, 0.0, 0.8, 1.0), 0.3),
    ];
    assert_eq!(holder(&people, &object, 0.45), Some(1));
    // Equal halves, but rounding gives the lower-scored person the larger
    // one (0.5000000000000001 against 0.49999999999999994): only the margin
    // gives the object to the higher score.
    let people = [(r(0.4, 0.0, 0.8, 1.0), 0.5), (r(0.0, 0.0, 0.4, 1.0), 0.3)];
    assert!(contained_fraction(&object, &people[0].0) < contained_fraction(&object, &people[1].0));
    assert_eq!(holder(&people, &object, 0.45), Some(0));
    assert_eq!(holder(&people, &object, 0.6), None);
    assert_eq!(holder(&[], &object, 0.45), None);
}

#[test]
fn role_prompts_attach_their_own_boxes() {
    let p = plan(&["a guitarist"]);
    let (person, role) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a guitarist"));
    let mut dets: Vec<Detection> = stage()
        .into_iter()
        .map(|(b, s)| det(&person, b, s))
        .collect();
    dets.push(det(&role, r(0.58, 0.68, 0.66, 0.76), 0.67));
    let out = p.assign(&dets);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].bbox, stage()[2].0);
}

#[test]
fn each_query_is_capped_and_queries_stay_separate() {
    let queries = [
        PromptQuery::new("all", "people"),
        PromptQuery::new("gtr", "a person playing guitar"),
    ];
    let params = MatchParams {
        max_per_query: 2,
        ..MatchParams::default()
    };
    let p = MatchPlan::new(&queries, |_| Some(0.15), params);
    let (person, guitar) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a guitar"));
    let mut dets: Vec<Detection> = stage()
        .into_iter()
        .map(|(b, s)| det(&person, b, s))
        .collect();
    dets.push(det(&guitar, r(0.20, 0.63, 0.25, 0.75), 0.63));
    let out = p.assign(&dets);
    let all: Vec<_> = out.iter().filter(|d| d.query.0 == "all").collect();
    let gtr: Vec<_> = out.iter().filter(|d| d.query.0 == "gtr").collect();
    assert_eq!(all.len(), 2, "capped");
    assert_eq!(gtr.len(), 1);
    assert_eq!(gtr[0].bbox, stage()[0].0);
}

#[test]
fn the_person_prompt_is_configurable() {
    let params = MatchParams {
        person_prompt: "Person".into(),
        ..MatchParams::default()
    };
    let p = MatchPlan::new(
        &[
            PromptQuery::new("q0", "person"),
            PromptQuery::new("q1", "a person"),
        ],
        |_| Some(0.3),
        params,
    );
    let prompts: Vec<&str> = p.queries().iter().map(|q| q.prompt.as_str()).collect();
    assert_eq!(prompts, ["person"], "both pick everyone, no object query");
}

#[test]
fn a_box_around_several_people_is_a_group_not_a_person() {
    let queries = [
        PromptQuery::new("q0", "a person playing guitar"),
        PromptQuery::new("q1", "people"),
    ];
    let p = MatchPlan::new(&queries, |_| Some(0.15), MatchParams::default());
    let (person, guitar) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a guitar"));
    let screen = r(0.10, 0.17, 0.76, 0.83);
    let dets = vec![
        det(&person, screen, 0.16),
        det(&person, r(0.15, 0.20, 0.20, 0.30), 0.16), // photos on the screen
        det(&person, r(0.40, 0.25, 0.45, 0.35), 0.15),
        det(&person, r(0.80, 0.60, 0.86, 0.95), 0.22), // a real person, outside
        det(&guitar, r(0.30, 0.65, 0.34, 0.80), 0.70), // on a stand, under the screen box
    ];
    let out = p.assign(&dets);
    assert!(out.iter().all(|d| d.bbox != screen), "{out:?}");
    assert!(
        out.iter().all(|d| d.query.0 != "q0"),
        "the stand's guitar is on no one"
    );
    assert_eq!(out.iter().filter(|d| d.query.0 == "q1").count(), 3);
}

#[test]
fn a_box_around_one_person_is_still_a_person() {
    let p = plan(&["a person playing guitar"]);
    let (person, guitar) = (model_id(&p, PERSON_PROMPT), model_id(&p, "a guitar"));
    let body = r(0.40, 0.50, 0.50, 0.95);
    let dets = vec![
        det(&person, body, 0.22),
        det(&person, r(0.42, 0.50, 0.48, 0.60), 0.16), // the same person's head
        det(&guitar, r(0.41, 0.65, 0.49, 0.75), 0.70),
    ];
    let out = p.assign(&dets);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].bbox, body);
}

#[test]
fn degenerate_boxes_overlap_nothing() {
    let zero = r(0.5, 0.5, 0.5, 0.5);
    assert_eq!(overlap_fraction(&zero, &r(0.0, 0.0, 1.0, 1.0)), 0.0);
    assert_eq!(
        overlap_fraction(&r(0.0, 0.0, 0.2, 0.2), &r(0.5, 0.5, 0.6, 0.6)),
        0.0
    );
    assert!((overlap_fraction(&r(0.0, 0.0, 0.2, 0.2), &r(0.1, 0.0, 1.0, 1.0)) - 0.5).abs() < 1e-9);
}

#[test]
fn no_queries_and_no_detections_give_nothing() {
    let p = plan(&[]);
    assert_eq!(p.queries().len(), 1);
    assert!(p.assign(&[]).is_empty());
    let p = plan(&["a person playing guitar"]);
    assert!(p.assign(&[]).is_empty());
}
