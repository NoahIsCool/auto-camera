//! `--list-audio-devices`: the Mac's audio input devices and their UIDs,
//! printed for the operator to copy into the config.
//!
//! An audio input of `kind = "device"` names its device by its CoreAudio
//! UID, which stays the same when the device is replugged but which macOS
//! shows nowhere (Audio MIDI Setup shows names only). This lists every
//! device that has input channels ([`ac_sources::SystemDevices`]), in the
//! system's order: its name (marked when it is the default input), its
//! input channels and nominal rate, and its UID as a TOML line ready to
//! paste into an `[[audio_inputs]]` entry. It opens no device and starts
//! nothing else.
//!
//! The exit status is 0 when the devices were listed (none connected
//! included) and 1 when they cannot be listed: a build that is not for
//! macOS or lacks the `coreaudio` feature, which the message says how to
//! fix, or CoreAudio failing.

use std::fmt::Write as _;
use std::process::ExitCode;

use ac_media::MediaError;
use ac_sources::audio_input::device::inputs;
use ac_sources::{AudioDevice, DeviceBackend, SystemDevices};

/// Lists this machine's audio input devices on stdout and returns the exit
/// status; see the module docs.
pub fn run() -> ExitCode {
    match SystemDevices.devices() {
        Ok(devices) => {
            print!("{}", report(&inputs(devices)));
            ExitCode::SUCCESS
        }
        Err(e) => {
            let why = match e {
                MediaError::Open(why) => why,
                other => other.to_string(),
            };
            eprintln!("auto-camera: cannot list the audio devices: {why}");
            ExitCode::FAILURE
        }
    }
}

/// The printed list of `devices` (input devices only).
fn report(devices: &[AudioDevice]) -> String {
    if devices.is_empty() {
        return "No audio input device is connected.\n".into();
    }
    let mut out = match devices.len() {
        1 => "1 audio input device:\n".to_string(),
        n => format!("{n} audio input devices:\n"),
    };
    for d in devices {
        let default = if d.default_input {
            " (the default input)"
        } else {
            ""
        };
        let s = if d.input_channels == 1 { "" } else { "s" };
        let _ = writeln!(
            out,
            "  {}{default}: {} input channel{s} at {} Hz",
            d.name, d.input_channels, d.sample_rate
        );
        // A JSON string is a TOML basic string, escapes and all.
        let uid = serde_json::to_string(&d.uid).unwrap_or_default();
        let _ = writeln!(out, "    uid = {uid}");
    }
    out.push_str(
        "Copy a device's uid line into its [[audio_inputs]] entry, with kind = \"device\".\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device listing.
    fn device(uid: &str, name: &str, channels: u16, default_input: bool) -> AudioDevice {
        AudioDevice {
            uid: uid.into(),
            name: name.into(),
            input_channels: channels,
            sample_rate: 48_000,
            default_input,
        }
    }

    #[test]
    fn each_device_gets_a_line_and_a_uid_line_to_paste() {
        let devices = [
            device("BuiltInMicrophoneDevice", "MacBook Pro Microphone", 1, true),
            device("AppleUSBAudioEngine:Allen&Heath:SQ:1A2B:1", "SQ", 32, false),
        ];
        assert_eq!(
            report(&devices),
            "2 audio input devices:\n\
             \x20 MacBook Pro Microphone (the default input): 1 input channel at 48000 Hz\n\
             \x20   uid = \"BuiltInMicrophoneDevice\"\n\
             \x20 SQ: 32 input channels at 48000 Hz\n\
             \x20   uid = \"AppleUSBAudioEngine:Allen&Heath:SQ:1A2B:1\"\n\
             Copy a device's uid line into its [[audio_inputs]] entry, with kind = \"device\".\n"
        );
    }

    #[test]
    fn a_uid_line_is_valid_toml_whatever_the_uid_holds() {
        let odd = device("Odd \"quoted\" \\ uid\t1", "Odd", 2, false);
        let text = report(std::slice::from_ref(&odd));
        assert!(text.starts_with("1 audio input device:\n"), "{text}");
        let line = text.lines().find(|l| l.contains("uid = ")).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(line.trim().trim_start_matches("uid = ")).unwrap();
        assert_eq!(parsed, serde_json::Value::String(odd.uid));
    }

    #[test]
    fn no_device_says_so() {
        assert_eq!(report(&[]), "No audio input device is connected.\n");
    }
}
