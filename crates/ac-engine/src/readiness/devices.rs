//! Audio inputs that are devices (`kind = "device"`): whether this build
//! can open them.
//!
//! A device input opens only in a macOS build with the `coreaudio` feature
//! ([`ac_sources::audio_input::device::SUPPORTED`]). In any other build it
//! would stay offline for the whole show, its supervisor retrying an open
//! that cannot work, so the check makes that a problem and says how to
//! build. Whether the device is connected is not asked, as for cameras: it
//! can be plugged in later, and an input whose device is missing stays
//! offline with the UIDs of the devices that are, and comes on air once it
//! is plugged in (`auto-camera --list-audio-devices` lists them).
//!
//! An empty UID is a config problem already, so it gets no finding here.

use ac_sources::audio_input::device::UNSUPPORTED;

use super::{Finding, Level};
use crate::config::StudioConfig;

/// A finding for each device input of `config`, in a build that can
/// (`supported`) or cannot open devices; see the module docs.
pub(super) fn findings(config: &StudioConfig, supported: bool) -> Vec<Finding> {
    config
        .audio_inputs
        .iter()
        .filter_map(|a| Some((a, a.input.device_uid()?)))
        .filter(|(_, uid)| !uid.trim().is_empty())
        .map(|(a, uid)| {
            let at = format!("audio input {:?}: device {uid:?}", a.id);
            if supported {
                Finding {
                    level: Level::Ok,
                    message: format!(
                        "{at} is opened by its UID (whether it is connected is not checked; \
                         `auto-camera --list-audio-devices` lists the devices)"
                    ),
                }
            } else {
                Finding {
                    level: Level::Problem,
                    message: format!("{at} cannot be opened: {UNSUPPORTED}"),
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readiness::Readiness;

    /// A studio with a synthetic source and the audio inputs `entries`
    /// (TOML lines after `[[audio_inputs]]` headers).
    fn studio(entries: &[&str]) -> StudioConfig {
        let mut text = String::from(
            "[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 64\nheight = 36\nfps = 30\n",
        );
        for e in entries {
            text.push_str("\n[[audio_inputs]]\n");
            text.push_str(e);
        }
        StudioConfig::parse(&text, false).unwrap()
    }

    #[test]
    fn a_build_without_coreaudio_cannot_open_a_device_and_says_how_to_build() {
        let c = studio(&[
            "id = \"board\"\nkind = \"device\"\nuid = \"AppleUSBAudioEngine:SQ:1\"\n",
            "id = \"rehearsal\"\nkind = \"file\"\npath = \"/media/board.wav\"\n",
        ]);
        let found = Readiness {
            findings: findings(&c, false),
        };
        let problems: Vec<&str> = found.problems().collect();
        assert_eq!(problems.len(), 1, "the file input is not a device");
        assert!(
            problems[0].starts_with(
                "audio input \"board\": device \"AppleUSBAudioEngine:SQ:1\" cannot be opened: "
            ),
            "{problems:?}"
        );
        assert!(problems[0].contains("--features coreaudio"), "{problems:?}");
    }

    #[test]
    fn a_build_with_coreaudio_notes_the_device_without_asking_for_it() {
        let c = studio(&["id = \"board\"\nkind = \"device\"\nuid = \"BuiltInMicrophoneDevice\"\n"]);
        let found = Readiness {
            findings: findings(&c, true),
        };
        assert!(found.is_ready());
        assert_eq!(
            found.findings[0].message,
            "audio input \"board\": device \"BuiltInMicrophoneDevice\" is opened by its UID \
             (whether it is connected is not checked; `auto-camera --list-audio-devices` lists \
             the devices)"
        );
        // An empty UID is the config check's to report.
        let blank = studio(&["id = \"board\"\nkind = \"device\"\nuid = \"\"\n"]);
        assert!(findings(&blank, false).is_empty());
    }
}
