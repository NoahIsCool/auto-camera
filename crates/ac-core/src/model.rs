//! Sources, queries, subjects, presets and shots.
//!
//! # Queries
//!
//! A [`Query`] keeps two prompts apart. [`Query::prompt`] is what the
//! operator typed ("speaker upper body") and is only ever shown;
//! [`Query::parse`] is that text read once, when the query was created
//! ([`crate::prompt`]), and says what the detector is asked for
//! ([`Query::detector_phrase`], "a person speaking") and whether the subject
//! is a person or an object ([`Basis`]). The parse is never recomputed, so a
//! query means the same thing for its whole life, whatever happens to the
//! vocabulary it was read with.
//!
//! # Basis and extent
//!
//! Two facts decide how a subject's shots are composed. The query's basis is
//! what the operator asked to frame; a subject's [`Extent`] is what the
//! detector's box covers. Shots are cut at body lines only when both say
//! person ([`Query::framing_extent`]). An object prompt keeps the fill rule
//! even when people-first matching answers it with the whole person holding
//! the object ("guitar close-up" finds the guitarist): framing that person's
//! head would frame the wrong thing, and framing the guitar itself needs its
//! box attached to the subject, which is later work.
//!
//! # Invariants
//!
//! * A query's presets never include [`ShotSize::Wide`]; every source has
//!   its own wide shot.
//! * `Query::prompt` is never sent to a detector; `Query::detector_phrase`
//!   is.

use serde::{Deserialize, Serialize};

use crate::geometry::{Extent, Rect};
use crate::prompt::{Basis, ParsedPrompt};

/// Stable identifier of a video source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceId(pub String);

/// Stable identifier of a query (text prompt).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct QueryId(pub String);

/// Identifier of a tracked subject, unique within one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SubjectId(pub u32);

/// Stable identifier of a shot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ShotId(pub String);

/// A video input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// Stable identifier.
    pub id: SourceId,
    /// Operator-facing name.
    pub name: String,
    /// Location of the input, such as a file path or a device URI.
    pub uri: String,
    /// Width divided by height of the source picture.
    pub aspect: f64,
}

/// A named text prompt and the framings applied to every subject it
/// matches; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// Stable identifier.
    pub id: QueryId,
    /// Operator-facing name, such as "Pastor".
    pub name: String,
    /// The prompt as the operator typed it, such as "speaker upper body".
    /// Shown, never sent to a detector.
    pub prompt: String,
    /// The prompt read once, when the query was created: what the detector
    /// is asked for, the shot sizes its shot words name and the basis.
    pub parse: ParsedPrompt,
    /// Framing recipes; each matched subject gets one shot per preset.
    pub presets: Vec<Preset>,
}

impl Query {
    /// A query whose `prompt` was read as `parse`, framed by `presets`.
    pub fn new(
        id: QueryId,
        name: &str,
        prompt: &str,
        parse: ParsedPrompt,
        presets: Vec<Preset>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            prompt: prompt.into(),
            parse,
            presets,
        }
    }

    /// What the detector is asked for: the parsed model phrase ("a person
    /// speaking" for "speaker upper body"), not the operator's prompt.
    pub fn detector_phrase(&self) -> &str {
        &self.parse.model_phrase
    }

    /// Whether the operator asked to frame a person or an object.
    pub fn basis(&self) -> Basis {
        self.parse.basis
    }

    /// What a box the detector marked `detected` is framed as for this
    /// query: a whole person only when the query asks for a person, else
    /// an object; see the module docs.
    pub fn framing_extent(&self, detected: Extent) -> Extent {
        match self.basis() {
            Basis::Person => detected,
            Basis::Object => Extent::Object,
        }
    }
}

/// Conventional broadcast shot sizes.
///
/// What a size means depends on the subject's [`Extent`]. For a whole-person
/// box it is a lower cut line on the body (tops of the shoulders for
/// [`ShotSize::Close`], the waist for [`ShotSize::Medium`]), measured in
/// head heights from the crown by `ac_framing`'s person basis. For any other
/// box, an object or a part such as an upper body, it is [`ShotSize::fill`]:
/// how much of the output frame height the box fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotSize {
    /// The whole source picture.
    Wide,
    /// Subject small in frame with generous surroundings.
    Full,
    /// Subject fills a little over half the frame height.
    Medium,
    /// Subject fills most of the frame height.
    MediumClose,
    /// Subject fills the frame height.
    Close,
}

impl ShotSize {
    /// Fraction of the output frame height the subject box should occupy,
    /// for boxes framed on the object basis ([`Extent::Object`], and person
    /// boxes too truncated to measure).
    ///
    /// The fractions were tuned on upper-body boxes. Applied to a whole
    /// person they give shots far too wide: a person half the frame high
    /// needs a frame 0.54 high for `Close`, which is a full-length shot.
    pub fn fill(self) -> f64 {
        match self {
            ShotSize::Wide => 0.0,
            ShotSize::Full => 0.4,
            ShotSize::Medium => 0.6,
            ShotSize::MediumClose => 0.78,
            ShotSize::Close => 0.92,
        }
    }
}

/// A framing recipe applied to a subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// Identifier, unique within its query.
    pub id: String,
    /// Operator-facing name, such as "Medium".
    pub name: String,
    /// Target shot size.
    pub size: ShotSize,
    /// Gap between the top of the subject (the crown, for a person) and the
    /// top of the frame, as a fraction of the output frame height.
    pub headroom: f64,
    /// Largest allowed magnification relative to the full source width. A
    /// shot that would need more is drawn at this limit, looser than its
    /// size, and reported as limited by zoom.
    pub max_zoom: f64,
}

impl Preset {
    /// A preset with broadcast-typical headroom and a 4x zoom ceiling.
    pub fn new(id: &str, name: &str, size: ShotSize) -> Self {
        let headroom = match size {
            ShotSize::Close => 0.03,
            ShotSize::MediumClose => 0.06,
            ShotSize::Medium => 0.08,
            ShotSize::Full | ShotSize::Wide => 0.1,
        };
        Self {
            id: id.into(),
            name: name.into(),
            size,
            headroom,
            max_zoom: 4.0,
        }
    }
}

/// Tracking state of a subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackState {
    /// Observed recently with good confidence.
    Tracking,
    /// Not observed recently; framing holds its last position.
    Lost,
}

/// One entity matched by a query on one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subject {
    /// Identifier, unique within the source.
    pub id: SubjectId,
    /// Source on which the subject was found.
    pub source: SourceId,
    /// Query that matched it.
    pub query: QueryId,
    /// Latest bounding box.
    pub bbox: Rect,
    /// What `bbox` covers: that of the newest detection applied to the
    /// subject. Absent in older data, where it reads as [`Extent::Object`].
    #[serde(default)]
    pub extent: Extent,
    /// Detector confidence in `0.0..=1.0`.
    pub confidence: f64,
    /// Tracking state.
    pub state: TrackState,
    /// Operator pinned the subject: it must never be retired automatically.
    pub pinned: bool,
}

/// What a shot frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotTarget {
    /// The whole picture of one source.
    Wide {
        /// The framed source.
        source: SourceId,
    },
    /// A subject framed by one of its query's presets.
    Subject {
        /// The framed source.
        source: SourceId,
        /// The framed subject.
        subject: SubjectId,
        /// Identifier of the preset within the subject's query.
        preset: String,
    },
}

/// A virtual camera the operator can put on preview or program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shot {
    /// Stable identifier.
    pub id: ShotId,
    /// Operator-facing name.
    pub name: String,
    /// What the shot frames.
    pub target: ShotTarget,
}

impl Shot {
    /// The source this shot crops from.
    pub fn source(&self) -> &SourceId {
        match &self.target {
            ShotTarget::Wide { source } | ShotTarget::Subject { source, .. } => source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tighter_sizes_fill_more_of_the_frame() {
        let sizes = [
            ShotSize::Full,
            ShotSize::Medium,
            ShotSize::MediumClose,
            ShotSize::Close,
        ];
        assert!(sizes.windows(2).all(|w| w[0].fill() < w[1].fill()));
    }

    /// A query for `prompt` read with the built-in lexicon.
    fn query(prompt: &str) -> Query {
        let parse = crate::parse_prompt(prompt, &crate::Lexicon::builtin());
        Query::new(QueryId("q".into()), "Q", prompt, parse, Vec::new())
    }

    #[test]
    fn the_detector_gets_the_parsed_phrase_not_the_prompt() {
        let q = query("speaker upper body");
        assert_eq!(q.prompt, "speaker upper body");
        assert_eq!(q.detector_phrase(), "a person speaking");
        assert_eq!(q.basis(), Basis::Person);
        let plain = query("a person playing guitar");
        assert_eq!(plain.detector_phrase(), plain.prompt);
    }

    #[test]
    fn shots_are_cut_at_body_lines_only_for_person_queries_on_person_boxes() {
        let person = query("guitarist close-up");
        assert_eq!(person.framing_extent(Extent::Person), Extent::Person);
        assert_eq!(person.framing_extent(Extent::Object), Extent::Object);
        // People-first matching answers "guitar" with the player holding it;
        // the shot keeps the fill rule rather than framing the player's head.
        let object = query("guitar close-up");
        assert_eq!(object.framing_extent(Extent::Person), Extent::Object);
        assert_eq!(object.framing_extent(Extent::Object), Extent::Object);
        // A failed parse (shot words only) is an object, so nothing is cut.
        assert_eq!(
            query("close-up").framing_extent(Extent::Person),
            Extent::Object
        );
    }

    #[test]
    fn tighter_presets_use_less_headroom() {
        let close = Preset::new("c", "Close", ShotSize::Close);
        let medium = Preset::new("m", "Medium", ShotSize::Medium);
        assert!(close.headroom < medium.headroom);
    }
}
