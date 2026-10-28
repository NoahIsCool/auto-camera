//! The `[director]` table: the auto-director's mode and content profile.
//!
//! Both keys are `ac_director` enums with their own defaults (suggest, talk),
//! and the director's parameters are derived from them and checked when the
//! studio builds the director
//! ([`StudioConfig::director_config`](super::StudioConfig::director_config)).

use ac_director::{ContentProfile, DirectorMode};
use serde::{Deserialize, Serialize};

/// Auto-director mode and content profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DirectorSettings {
    /// Off, suggest (default) or auto.
    pub mode: DirectorMode,
    /// Talk (default) or music.
    pub profile: ContentProfile,
}
