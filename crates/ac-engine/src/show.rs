//! Shows: what the operator built on a studio, as a document that survives
//! a restart (spec M13).
//!
//! # Theory
//!
//! A studio file ([`crate::config`]) is written by hand and says what the
//! studio is: its sources, detector, tuning and encoder. While a show runs
//! the operator changes part of that: adds and removes queries, sets the
//! director's mode and profile, picks a transition. A [`Show`] is that part
//! as it stands, a layer over the studio file it names (`studio`), which
//! stays the base:
//!
//! * The engine core exports it as a pure value
//!   ([`EngineCore::export_show`](crate::EngineCore::export_show), the
//!   `ExportShow` command): the sources, the queries with their presets and
//!   priorities, the director's mode and profile, the transition and the
//!   output's default destinations. Nothing is exported unasked: saving is
//!   the operator's act (the window's Save show, `--save-show`), never a
//!   side effect of a change.
//! * The app names the studio file and writes the show to a show file, off
//!   the engine thread, replacing the previous one atomically.
//! * Opening a show file (`--config`, `--check`, any loader of
//!   [`StudioConfig`]) reads the studio file it names and lays the show
//!   over it ([`Show::apply_to`]): a list the show gives (`sources`,
//!   `queries`) replaces the studio's, a key it gives in `[director]`,
//!   `[switcher]` or `[output]` replaces that one value, and whatever the
//!   show leaves out, every other table included, is the studio's. The
//!   result is checked like any studio config.
//!
//! ```toml
//! show = 1
//! studio = "/Users/me/studios/sunday.toml"
//!
//! [[queries]]
//! name = "Pastor"
//! prompt = "speaker upper body"
//! presets = ["medium", "medium_close"]
//!
//! [director]
//! mode = "auto"
//! ```
//!
//! # Versions
//!
//! A show file has its own layout number, the top-level `show` key, which
//! also says that the file is a show ([`Show::is_show`]): a studio file
//! never has that key, since its keys are checked. It is read through the
//! studio file's machinery (`config::schema`) with its own [`Layout`]:
//! [`SHOW_VERSION`] reads straight from the text, an older version is
//! upgraded one step at a time by [`MIGRATIONS`] (with a warning that the
//! file itself is unchanged until it is saved again), and a newer or
//! unknown one is an error. A file is a show because of its key, so a
//! show file without one cannot exist; [`Show::parse_with_warnings`]
//! refuses text without it. Later fields (slots, zones, names) come with
//! a version and a step each, tested as the step of a pretend version is
//! in this module's tests.
//!
//! # Stream keys
//!
//! The default RTMP URL carries a stream key. A show holds the one its
//! studio config held (the destinations are the config's defaults; a URL
//! typed at Start Output is not part of the show), so a show file says no
//! more than its studio file did, and the app writes it readable by its
//! owner only.
//!
//! # Invariants
//!
//! * A show the core exports, laid over the studio file it was started
//!   from, builds a core whose export is that show again: queries (ids,
//!   prompts, presets, literal flags, priorities), director mode and
//!   profile, transition and destinations survive a restart.
//! * Parsing, laying over and writing out are pure; reading the files is
//!   the loader's (`config::load`).
//! * [`Show::to_text`] writes [`SHOW_VERSION`] first, so the file says
//!   what it is before anything else, and parses back to the same value.

use std::path::{Path, PathBuf};

use ac_director::{ContentProfile, DirectorMode};
use ac_sources::SourceConfig;
use ac_switcher::TransitionKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::schema::{Layout, Migration, read_layout};
use crate::config::{ConfigError, LoadedConfig, QuerySettings, SourceEntry, StudioConfig};

/// The layout of show files this build reads and writes.
pub const SHOW_VERSION: u32 = 1;

/// The steps from each older show layout to the next: `MIGRATIONS[0]`
/// takes show schema 1 to 2, and so on. Empty while 1 is the only one.
const MIGRATIONS: [Migration; SHOW_VERSION as usize - 1] = [];

/// The show file's layout: keyed `show`, which it requires.
const LAYOUT: Layout<'static> = Layout {
    key: "show",
    label: "show schema",
    noun: "show",
    current: SHOW_VERSION,
    steps: &MIGRATIONS,
    unversioned: None,
};

/// The comment a written TOML show starts with, before its `show` key.
const HEADER: &str = "\
# An Auto Camera show, written by Save show: the sources, queries, director,
# transition and output destinations as they stood, laid over the studio
# file `studio`. A key left out is the studio file's. Open it with
# `auto-camera --config <this file>`.

";

/// What the operator built on a studio; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Show {
    /// The layout the show is written in: [`SHOW_VERSION`] once read.
    /// First, so a written file starts with it.
    pub show: u32,
    /// The studio file the show lies over. Required in a show file, where
    /// a relative path is taken from the show file's folder; `None` only
    /// in a show just exported, before the app names its studio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio: Option<PathBuf>,
    /// The sources, in place of the studio's when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<SourceEntry>>,
    /// The queries, in place of the studio's when given (an empty list
    /// leaves none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<QuerySettings>>,
    /// The director's mode and profile.
    #[serde(default, skip_serializing_if = "ShowDirector::is_empty")]
    pub director: ShowDirector,
    /// The transition.
    #[serde(default, skip_serializing_if = "ShowSwitcher::is_empty")]
    pub switcher: ShowSwitcher,
    /// The output's default destinations.
    #[serde(default, skip_serializing_if = "ShowOutput::is_empty")]
    pub output: ShowOutput,
}

/// The `[director]` keys a show holds, each replacing the studio's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShowDirector {
    /// Off, suggest or auto.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<DirectorMode>,
    /// Talk or music.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ContentProfile>,
}

impl ShowDirector {
    /// True when the show gives neither key.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The `[switcher]` keys a show holds, each replacing the studio's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShowSwitcher {
    /// Cut, mix or dip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition: Option<TransitionKind>,
    /// Seconds, for mix and dip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

impl ShowSwitcher {
    /// True when the show gives neither key.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The `[output]` keys a show holds, each replacing the studio's; an empty
/// value means none, as in a studio file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShowOutput {
    /// Default RTMP URL, with its stream key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtmp_url: Option<String>,
    /// Default recording path, taken from the working folder as in a
    /// studio file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_path: Option<PathBuf>,
}

impl ShowOutput {
    /// True when the show gives neither key.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Show {
    /// True when `text` (JSON when `json`, else TOML) is a show: a
    /// document with a top-level `show` key. Text that does not parse is
    /// not a show; reading it as a studio file says why it does not parse.
    pub fn is_show(text: &str, json: bool) -> bool {
        let doc: Result<Value, String> = if json {
            serde_json::from_str(text).map_err(|e| e.to_string())
        } else {
            toml::from_str(text).map_err(|e| e.to_string())
        };
        doc.is_ok_and(|d| d.get(LAYOUT.key).is_some())
    }

    /// Parses `text` (JSON when `json`, else TOML) written at any show
    /// schema this build reads, without checking its values, with the
    /// warnings for the operator (an upgraded older layout); see the module
    /// docs.
    pub fn parse_with_warnings(text: &str, json: bool) -> Result<(Self, Vec<String>), String> {
        read_layout(text, json, &LAYOUT)
    }

    /// The show as a file's text: JSON when `json`, else TOML after a
    /// comment saying what the file is. Fails only on a path that is not
    /// UTF-8, which TOML and JSON cannot hold.
    pub fn to_text(&self, json: bool) -> Result<String, String> {
        if json {
            serde_json::to_string_pretty(self)
                .map(|t| t + "\n")
                .map_err(|e| e.to_string())
        } else {
            toml::to_string(self)
                .map(|t| format!("{HEADER}{t}"))
                .map_err(|e| e.to_string())
        }
    }

    /// Makes a relative `studio` and relative source file paths relative
    /// to `dir`, the show file's folder, as a studio file's are to its.
    pub fn resolve_paths(&mut self, dir: &Path) {
        if let Some(studio) = &mut self.studio
            && studio.is_relative()
        {
            *studio = dir.join(&*studio);
        }
        for s in self.sources.iter_mut().flatten() {
            if let SourceConfig::File { path, .. } = &mut s.input
                && path.is_relative()
            {
                *path = dir.join(&*path);
            }
        }
    }

    /// What opening the show as the show file `path` would give, its paths
    /// already resolved (an exported show's are): the studio file it names,
    /// read, with the show laid over it, not validated. It reads the studio
    /// file, so the app asks it before it writes a show, off the engine
    /// thread.
    pub fn open(&self, path: &Path) -> Result<LoadedConfig, ConfigError> {
        crate::config::open_show(self, path)
    }

    /// Lays the show over `studio`: each list and key the show gives
    /// replaces the studio's, and the rest is left as it is (see the module
    /// docs). The result is not checked; the loader validates it as it
    /// validates any studio config.
    pub fn apply_to(&self, studio: &mut StudioConfig) {
        if let Some(sources) = &self.sources {
            studio.sources.clone_from(sources);
        }
        if let Some(queries) = &self.queries {
            studio.queries.clone_from(queries);
        }
        let ShowDirector { mode, profile } = self.director;
        studio.director.mode = mode.unwrap_or(studio.director.mode);
        studio.director.profile = profile.unwrap_or(studio.director.profile);
        let ShowSwitcher {
            transition,
            duration,
        } = self.switcher;
        studio.switcher.transition = transition.unwrap_or(studio.switcher.transition);
        studio.switcher.duration = duration.unwrap_or(studio.switcher.duration);
        if let Some(url) = &self.output.rtmp_url {
            studio.output.rtmp_url = Some(url.clone());
        }
        if let Some(path) = &self.output.record_path {
            studio.output.record_path = Some(path.clone());
        }
    }
}

#[cfg(test)]
mod tests;
