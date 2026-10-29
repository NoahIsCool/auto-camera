//! The `[[audio_inputs]]` table: program audio with no picture, such as a
//! soundboard's feed (spec M12, "one stereo input").
//!
//! An entry is an [`AudioInputConfig`] (`kind = "file"`, or `"device"`
//! for a CoreAudio device such as the board's USB interface, by `uid`; its
//! keys flattened into the entry) plus an id, an optional operator-facing
//! name and which of the input's channels are program audio:
//!
//! ```toml
//! [[audio_inputs]]
//! id = "board"
//! kind = "file"
//! path = "board-rehearsal.wav"
//! loop_playback = true
//! channels = [3, 4]     # left and right; [5] puts channel 5 on both sides
//! channel_count = 16    # what the input has, checked before it is used
//!
//! [output]
//! audio_source = "board"
//! ```
//!
//! On the Mac the board itself is a device, named by the UID that
//! `auto-camera --list-audio-devices` prints (a build with the `coreaudio`
//! feature opens it; the readiness check says so of any other build):
//!
//! ```toml
//! [[audio_inputs]]
//! id = "board"
//! kind = "device"
//! uid = "AppleUSBAudioEngine:Allen&Heath:SQ:1A2B3C:1"
//! channels = [1, 2]
//! ```
//!
//! # Channels
//!
//! A board sends many channels and the program carries two, so the entry
//! picks them ([`ChannelPick`]): `channels` lists two numbers (left, then
//! right) or one (on both sides), counted from 1 as a board labels them.
//! Without it the program takes channels 1 and 2, or a mono input's one on
//! both sides. The picked samples are copied as they are: nothing is mixed
//! (an in-app mix is out of scope), so a pick is exact or refused.
//!
//! `channel_count`, when given, is how many channels the input has. It lets
//! the config check catch a pick that cannot exist ("channels 17 and 18" of
//! a 16-channel board) before anything opens, and makes the engine refuse
//! a connection with another count (the wrong device or recording), which
//! then stays offline with the reason and is retried. Without it, each
//! connection is still refused when it lacks the picked channels.
//!
//! # Ids
//!
//! An audio input's id is shown and named by `output.audio_source`, which
//! takes a video source's id too (its embedded audio), so the two kinds
//! share one namespace: an audio input's id must be a valid id
//! ([`valid_id`]), unique among the audio inputs, and not
//! a source's id. `output.audio_source` may not name a capture source: a
//! camera's audio is not captured, so the program would be silent with
//! nothing saying why.
//!
//! The check here is pure, like every config check: whether the file is
//! there is the readiness check's question ([`crate::readiness`]), and its
//! real channel count is learned when it opens.

use std::collections::HashSet;
use std::path::Path;

use ac_core::SourceId;
use ac_media::{AudioInputInfo, ChannelPick};
use ac_sources::{AudioInputConfig, SourceConfig};
use serde::{Deserialize, Serialize};

use super::{StudioConfig, valid_id};

/// One audio-only input; see the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioInputEntry {
    /// Stable identifier; letters, digits, `-` and `_`, not a source's id.
    pub id: String,
    /// Operator-facing name (defaults to the id).
    #[serde(default)]
    pub name: Option<String>,
    /// The channels that are program audio, counted from 1: left and
    /// right, or one on both sides. Default: 1 and 2 (a mono input's one
    /// on both sides).
    #[serde(default)]
    pub channels: Option<Vec<u16>>,
    /// How many channels the input has, when known; a connection with
    /// another count is refused.
    #[serde(default)]
    pub channel_count: Option<u16>,
    /// How to open it (`kind = "file"`).
    #[serde(flatten)]
    pub input: AudioInputConfig,
}

impl AudioInputEntry {
    /// The input's id.
    pub fn input_id(&self) -> SourceId {
        SourceId(self.id.clone())
    }

    /// The operator-facing name.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// The configured pick, `None` for the default (see the module docs),
    /// or why `channels` is not one.
    pub fn pick(&self) -> Result<Option<ChannelPick>, String> {
        self.channels
            .as_deref()
            .map(ChannelPick::from_numbers)
            .transpose()
    }

    /// How the input is described while it has not opened: its id, and no
    /// format, which only an opened input knows.
    pub fn placeholder_info(&self) -> AudioInputInfo {
        AudioInputInfo {
            id: self.input_id(),
            format: None,
        }
    }

    /// The check the engine makes of every connection before using it: it
    /// has `channel_count` channels when that is given, and the picked
    /// ones. A pick the config check refuses is not checked here (the
    /// engine does not start with one).
    pub fn check_opened(&self) -> impl Fn(&AudioInputInfo) -> Result<(), String> + Send + 'static {
        let (pick, expected) = (self.pick().ok().flatten(), self.channel_count);
        move |info: &AudioInputInfo| {
            let Some((_, channels)) = info.format else {
                return Ok(());
            };
            if channels == 0 {
                return Err("the input has no channels".into());
            }
            if let Some(n) = expected
                && n != channels
            {
                return Err(format!(
                    "the input has {channels} channels, not the {n} of channel_count"
                ));
            }
            pick.map_or(Ok(()), |p| p.check(channels))
        }
    }
}

/// Makes the relative file paths of `entries` relative to `dir`.
pub(super) fn resolve_paths(entries: &mut [AudioInputEntry], dir: &Path) {
    for path in entries.iter_mut().filter_map(|a| a.input.path_mut()) {
        if path.is_relative() {
            *path = dir.join(&*path);
        }
    }
}

/// Every problem of `config`'s audio inputs and of `output.audio_source`,
/// which must name one of `source_ids` or an audio input, and not a
/// capture source (whose audio is not captured); see the module docs.
pub(super) fn problems(config: &StudioConfig, source_ids: &HashSet<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut ids = HashSet::new();
    for a in &config.audio_inputs {
        let at = format!("audio input {:?}", a.id);
        if !valid_id(&a.id) {
            out.push(format!(
                "{at}: its id must be non-empty letters, digits, '-' or '_'"
            ));
        }
        if !ids.insert(a.id.as_str()) {
            out.push(format!("duplicate audio input id {:?}", a.id));
        }
        if source_ids.contains(a.id.as_str()) {
            out.push(format!(
                "{at}: a source has the same id, so output.audio_source could mean either"
            ));
        }
        if let Some(problem) = a.input.problem() {
            out.push(format!("{at}: {problem}"));
        }
        let pick = match a.pick() {
            Ok(pick) => pick,
            Err(e) => {
                out.push(format!("{at}: channels: {e}"));
                None
            }
        };
        match a.channel_count {
            Some(0) => out.push(format!("{at}: channel_count must be at least 1")),
            Some(n) => {
                if let Some(Err(e)) = pick.map(|p| p.check(n)) {
                    out.push(format!("{at}: {e} (channel_count)"));
                }
            }
            None => {}
        }
    }
    if let Some(audio) = &config.output.audio_source {
        if !source_ids.contains(audio.as_str()) && !ids.contains(audio.as_str()) {
            out.push(format!(
                "output.audio_source {audio:?} is not a source or audio input id"
            ));
        }
        // A capture source opens the video only, so naming one would give
        // the program no audio and nothing would say why.
        if config
            .sources
            .iter()
            .any(|s| s.id == *audio && matches!(s.input, SourceConfig::Capture { .. }))
        {
            out.push(format!(
                "output.audio_source {audio:?} is a camera, whose audio is not captured; \
                 name an audio input ([[audio_inputs]]) for program audio"
            ));
        }
    }
    out
}

#[cfg(test)]
mod device_tests;

#[cfg(test)]
mod tests;
