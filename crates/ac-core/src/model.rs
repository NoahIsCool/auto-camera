//! Sources, queries, subjects, presets and shots.

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;

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

/// A named text prompt and the framings applied to every subject it matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// Stable identifier.
    pub id: QueryId,
    /// Operator-facing name, such as "Pastor".
    pub name: String,
    /// The open-vocabulary prompt given to the detector, such as "person's upper body".
    pub prompt: String,
    /// Framing recipes; each matched subject gets one shot per preset.
    pub presets: Vec<Preset>,
}

/// Conventional broadcast shot sizes.
///
/// Each size states how much of the output frame height the subject's box
/// should fill. For a person detected as an upper body this moves from a loose
/// medium shot to a tight close-up; for objects it scales the object in frame.
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
    /// Fraction of the output frame height the subject box should occupy.
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
    /// Gap between the top of the subject and the top of the frame, as a
    /// fraction of the output frame height.
    pub headroom: f64,
    /// Largest allowed magnification relative to the full source width.
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

    #[test]
    fn tighter_presets_use_less_headroom() {
        let close = Preset::new("c", "Close", ShotSize::Close);
        let medium = Preset::new("m", "Medium", ShotSize::Medium);
        assert!(close.headroom < medium.headroom);
    }
}
