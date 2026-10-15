//! People-first prompt matching: every subject is a whole person, and a
//! prompt only chooses which people.
//!
//! OWLv2 boxes whatever a prompt names. Asked for "a person playing guitar"
//! it boxes guitars (and guitars on stands, and guitars in photos on the
//! projection screen); asked for "a person speaking at a lectern" it boxes
//! lecterns and microphone stands. Framing those boxes frames objects, not
//! people. Scoring the prompt on the person boxes themselves does not help
//! either: on whole-person boxes "a person playing guitar" scores the
//! singer, the drummer and the keyboard player as high as the guitarists.
//! What the model does well is find the people ("a person") and find the
//! prompt's object ("a guitar", 0.6–0.75 on real guitars against at most 0.26
//! for anything else). So a detector pass asks for both, and a prompt picks a
//! person when one of its object boxes lies mostly inside that person's box.
//!
//! On 40 hand-checked frames of a band and a sermon, that rule found all 60
//! guitarist-frames with 2 false subjects, against 45 found with 22 false
//! when the guitar had to lie 60% inside, and 58 found with 65 false for
//! same-box scoring (see crates/ac-vision/README.md).
//!
//! # Plans
//!
//! [`MatchPlan::new`] turns the operator's queries into the model's queries:
//!
//! * the person query ([`PERSON_PROMPT`] unless configured), always first;
//! * per user prompt, its *object*: [`object_phrase`] parses "a person
//!   playing guitar" into "a guitar" and "a person speaking at a lectern" into
//!   "a lectern". A prompt with no person word ("a guitarist", "the drummer")
//!   or no object ("a woman", "a person speaking") is its own object, so its
//!   own boxes are attached instead. A prompt that only says "person" picks
//!   everyone.
//!
//! Identical objects are asked for once. [`MatchPlan::assign`] maps the
//! decoded model detections back to the operator's queries: the output box is
//! always a person box, and its score is the object's score (the person's,
//! for an everyone prompt).
//!
//! A "person" box that contains two or more other people is a group (OWLv2
//! sometimes boxes a whole projection screen of photos, or a whole band, as
//! "a person"); it is dropped before objects are attached, so objects under
//! it do not make a false subject.
//!
//! # Limits
//!
//! The model sees objects, not actions: a speaker who is not at a lectern is
//! not "a person speaking at a lectern", and "holding a microphone" cannot be
//! told from standing at a microphone stand. Someone standing right in front
//! of an instrument on a stand holds it, as far as boxes can tell. Prompts
//! work best when they
//! name something visible on the person: an instrument, a garment, a prop.

use std::collections::HashMap;

use ac_core::{QueryId, Rect};

use crate::api::{Detection, PromptQuery};
use crate::owl::normalize_prompt;

/// The default person query, asked on every pass.
pub const PERSON_PROMPT: &str = "a person";

/// Words that name a person; text after the first one describes them.
const PERSON_WORDS: &[&str] = &[
    "person",
    "persons",
    "people",
    "man",
    "men",
    "woman",
    "women",
    "someone",
    "somebody",
    "guy",
    "guys",
    "girl",
    "girls",
    "boy",
    "boys",
    "kid",
    "kids",
    "child",
    "children",
    "human",
    "humans",
    "lady",
    "individual",
];

/// Words that only say "person" when a prompt is nothing else.
const EVERYONE: &[&str] = &[
    "person", "persons", "people", "someone", "somebody", "human",
];

/// Articles and possessives: after one, an "-ing" word is a noun
/// ("a painting"), not a verb.
const ARTICLES: &[&str] = &["a", "an", "the", "his", "her", "their", "its", "some"];

/// Words skipped between the person and the object besides articles:
/// relative pronouns, auxiliaries and prepositions.
const LINK_WORDS: &[&str] = &[
    "who", "that", "which", "is", "are", "at", "on", "in", "front", "with", "behind", "near", "by",
    "beside", "next", "to", "from", "into", "onto", "over", "under", "of", "and",
];

/// What a user prompt asks the model for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Every person ("a person", "people").
    Everyone,
    /// People with this object inside their box.
    Object(String),
}

/// The selector for one prompt; see the module docs.
pub fn selector(prompt: &str) -> Selector {
    let text = normalize_prompt(prompt);
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    let Some(k) = words.iter().position(|w| PERSON_WORDS.contains(w)) else {
        return Selector::Object(text);
    };
    let before = &words[..k];
    let after = &words[k + 1..];
    let only_articles = before
        .iter()
        .all(|w| matches!(*w, "a" | "an" | "the" | "any"));
    if after.is_empty() {
        return if only_articles && EVERYONE.contains(&words[k]) {
            Selector::Everyone
        } else {
            Selector::Object(text)
        };
    }
    match object_phrase(after) {
        Some(object) => Selector::Object(object),
        None => Selector::Object(text),
    }
}

/// The object named by the words after the person word: articles, link
/// words and "-ing" verbs are dropped from the front, and the rest is
/// prefixed with "a" ("playing guitar" → "a guitar", "holding a painting" →
/// "a painting"). An "-ing" word counts as a verb only before the first
/// article. `None` when nothing is left.
pub fn object_phrase(after: &[&str]) -> Option<String> {
    let mut article_seen = false;
    for (i, w) in after.iter().enumerate() {
        if ARTICLES.contains(w) {
            article_seen = true;
            continue;
        }
        if LINK_WORDS.contains(w) || (!article_seen && is_verb_ing(w)) {
            continue;
        }
        return Some(format!("a {}", after[i..].join(" ")));
    }
    None
}

/// A word that looks like an "-ing" verb ("playing", not "ring").
fn is_verb_ing(word: &str) -> bool {
    word.len() > 4 && word.ends_with("ing")
}

/// Matching tuning.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchParams {
    /// The prompt that finds people.
    pub person_prompt: String,
    /// Lowest person score kept.
    pub person_threshold: f32,
    /// Fraction of an object's box that must lie inside a person's box.
    pub min_overlap: f64,
    /// Largest number of people returned per query.
    pub max_per_query: usize,
}

impl Default for MatchParams {
    fn default() -> Self {
        Self {
            person_prompt: PERSON_PROMPT.into(),
            person_threshold: 0.15,
            min_overlap: 0.45,
            max_per_query: 16,
        }
    }
}

/// The model queries for a set of user queries, and how to map the model's
/// detections back; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchPlan {
    /// Model queries, the person query first.
    internal: Vec<PromptQuery>,
    /// Score threshold per model query, in the same order.
    thresholds: Vec<f32>,
    /// Per user query: its id, threshold and model query index (`None` for
    /// everyone).
    routes: Vec<(QueryId, f32, Option<usize>)>,
    /// Tuning.
    params: MatchParams,
}

impl MatchPlan {
    /// Plans a pass for `queries`; `threshold` gives each user query's object
    /// score threshold.
    pub fn new(
        queries: &[PromptQuery],
        threshold: impl Fn(&QueryId) -> f32,
        params: MatchParams,
    ) -> Self {
        let person = normalize_prompt(&params.person_prompt);
        let mut internal = vec![PromptQuery::new("#person", &person)];
        let mut thresholds = vec![params.person_threshold];
        let mut by_text: HashMap<String, usize> = HashMap::new();
        let mut routes = Vec::with_capacity(queries.len());
        for q in queries {
            let t = threshold(&q.id);
            let route = match selector(&q.prompt) {
                Selector::Everyone => None,
                Selector::Object(object) if object == person => None,
                Selector::Object(object) => {
                    let i = *by_text.entry(object.clone()).or_insert_with(|| {
                        internal.push(PromptQuery::new(
                            &format!("#object{}", internal.len()),
                            &object,
                        ));
                        thresholds.push(t);
                        internal.len() - 1
                    });
                    // Shared objects decode at the lowest threshold asking.
                    thresholds[i] = thresholds[i].min(t);
                    Some(i)
                }
            };
            routes.push((q.id.clone(), t, route));
        }
        Self {
            internal,
            thresholds,
            routes,
            params,
        }
    }

    /// The model queries, the person query first.
    pub fn queries(&self) -> &[PromptQuery] {
        &self.internal
    }

    /// The decode threshold of each model query.
    pub fn thresholds(&self) -> &[f32] {
        &self.thresholds
    }

    /// Maps decoded model detections (already per-query NMS'd) to person
    /// detections for the user queries.
    pub fn assign(&self, detections: &[Detection]) -> Vec<Detection> {
        let mut people: Vec<(Rect, f32)> = detections
            .iter()
            .filter(|d| d.query == self.internal[0].id && d.score >= self.params.person_threshold)
            .map(|d| (d.bbox, d.score))
            .collect();
        people.sort_by(|a, b| b.1.total_cmp(&a.1));
        drop_groups(&mut people);
        let mut out = Vec::new();
        for (id, threshold, route) in &self.routes {
            let mut best: Vec<Option<f32>> = vec![None; people.len()];
            match route {
                None => {
                    for (slot, (_, score)) in best.iter_mut().zip(&people) {
                        if *score >= *threshold {
                            *slot = Some(*score);
                        }
                    }
                }
                Some(i) => {
                    let object = &self.internal[*i].id;
                    for d in detections
                        .iter()
                        .filter(|d| &d.query == object && d.score >= *threshold)
                    {
                        if let Some(p) = holder(&people, &d.bbox, self.params.min_overlap) {
                            best[p] = Some(best[p].map_or(d.score, |s: f32| s.max(d.score)));
                        }
                    }
                }
            }
            let mut picked: Vec<Detection> = best
                .iter()
                .zip(&people)
                .filter_map(|(s, (bbox, _))| {
                    s.map(|score| Detection {
                        query: id.clone(),
                        bbox: *bbox,
                        score,
                    })
                })
                .collect();
            picked.sort_by(|a, b| b.score.total_cmp(&a.score));
            picked.truncate(self.params.max_per_query);
            out.extend(picked);
        }
        out
    }
}

/// Fraction of another person's box that must lie inside a box for that box
/// to count it.
const GROUP_CONTAINS: f64 = 0.8;

/// Removes boxes that contain two or more other people; see the module docs.
fn drop_groups(people: &mut Vec<(Rect, f32)>) {
    let groups: Vec<bool> = people
        .iter()
        .map(|(outer, _)| {
            people
                .iter()
                .filter(|(inner, _)| {
                    inner != outer && overlap_fraction(inner, outer) >= GROUP_CONTAINS
                })
                .count()
                >= 2
        })
        .collect();
    let mut flags = groups.into_iter();
    people.retain(|_| !flags.next().unwrap_or(false));
}

/// Fraction of `inner`'s area that lies inside `outer`.
pub fn overlap_fraction(inner: &Rect, outer: &Rect) -> f64 {
    let area = inner.area();
    if area <= 0.0 {
        return 0.0;
    }
    inner.intersection(outer).map_or(0.0, |i| i.area() / area)
}

/// The person (index into `people`) holding `object`: the one containing the
/// largest fraction of it, if that is at least `min_overlap`. Ties go to the
/// higher-scored person, which comes first.
fn holder(people: &[(Rect, f32)], object: &Rect, min_overlap: f64) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, (person, _)) in people.iter().enumerate() {
        let f = overlap_fraction(object, person);
        if f >= min_overlap && best.is_none_or(|(_, b)| f > b) {
            best = Some((i, f));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests;
