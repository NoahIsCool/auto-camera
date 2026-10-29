//! Audio inputs from the Mac's audio devices: the soundboard's USB audio
//! interface, or anything else CoreAudio offers as a device (a Dante
//! Virtual Soundcard, an AVB or MADI interface with its CoreAudio driver),
//! named by the device's UID (spec M12, "board feed").
//!
//! # Identity
//!
//! An input is configured by the device's CoreAudio UID
//! (`kAudioDevicePropertyDeviceUID`): a string the driver builds from the
//! device itself, which stays the same when the device is unplugged and
//! plugged back in, while the numeric device id CoreAudio hands out does
//! not. Every open lists the devices ([`DeviceBackend::devices`]) and picks
//! the one with exactly that UID ([`select`]). A device that is not there
//! is an error naming the UIDs that are, and nothing else is ever opened in
//! its place: not the default microphone, not a device of the same name.
//! `auto-camera --list-audio-devices` prints every input device with its
//! UID, to copy into the config.
//!
//! # A connection
//!
//! A connection captures every input channel of the device at its current
//! nominal rate (the engine picks the program's two channels; see
//! [`ac_media::ChannelPick`]) and is built from three parts, each in a
//! module of its own and all but the first pure:
//!
//! * the **backend** ([`DeviceBackend`]) lists the devices and starts a
//!   stream whose callbacks feed the other two. [`SystemDevices`] is
//!   CoreAudio, through cpal, in a macOS build with the `coreaudio`
//!   feature; tests use a fake. Everything that is not a CoreAudio call
//!   lives outside it;
//! * the **hand-off** ([`mod@handoff`]) carries the callback's buffers to the
//!   input's worker thread through a fixed pool, never blocking or
//!   allocating in the callback;
//! * the **stamper** ([`timing`]) gives each block its sequence number and
//!   its PTS, the time it was captured at mapped to the clock the input is
//!   paced by, and finds the stream's discontinuities.
//!
//! The worker ([`input`]) polls the hand-off, stamps the blocks and queues
//! them in the input's [`AudioRing`](ac_media::AudioRing); the first block
//! makes the input `Running`.
//!
//! # Trouble
//!
//! * A **discontinuity** (audio the hand-off dropped, a host-time jump
//!   that lasted or a timestamp glitch that came back, see [`timing`]) or
//!   an **overload** the device reported does not stop the input: it is
//!   counted, the two on counts of their own (an overload that lost audio
//!   is measured as a discontinuity too; see [`input`]), logged (at most a
//!   line per [`input::LOG_QUIET`], counting the rest) and shown as the
//!   input's error while it keeps running, with both counts. Blocks the
//!   hand-off dropped count in
//!   [`AudioInput::dropped`](ac_media::AudioInput::dropped) too.
//! * The device **going away** (unplugged; its sample rate changed under
//!   the stream) stops the input with that error, and so does a device
//!   that sends nothing for [`DeviceOptions::stall_timeout`]
//!   ([`DeviceOptions::open_timeout`] after opening), in real time. A
//!   supervisor ([`super::supervised`]) then opens it again, listing the
//!   devices anew, so the board comes back when it is plugged back in.
//!
//! # Invariants
//!
//! * A connection opens the device with exactly the configured UID, or
//!   fails.
//! * The device's callbacks never block, allocate or take a lock
//!   ([`mod@handoff`]); the input's memory is bounded by its pool and its ring.
//! * Sequence numbers and PTS follow [`timing`]'s rules: one number per
//!   block (dropped blocks keep theirs), PTS never decreasing.
//! * After [`AudioInput::stop`](ac_media::AudioInput::stop) (or a drop)
//!   the stream is stopped and the worker joined.

use std::fmt;

use ac_media::MediaError;

#[cfg(all(feature = "coreaudio", target_os = "macos"))]
mod coreaudio;
pub mod handoff;
pub mod input;
pub mod timing;

pub use handoff::{CaptureSink, Captured, HandoffReader, StreamEvents, handoff};
pub use input::{DeviceInput, DeviceOptions};
pub use timing::{CaptureLatency, Discontinuity, Stamper};

/// True in a build that can open CoreAudio devices: macOS, with the
/// `coreaudio` feature.
pub const SUPPORTED: bool = cfg!(all(feature = "coreaudio", target_os = "macos"));

/// Why this build cannot open CoreAudio devices, and how to build one that
/// can; see [`SUPPORTED`].
pub const UNSUPPORTED: &str = if cfg!(target_os = "macos") {
    "this build has no CoreAudio device support; build auto-camera with \
     `--features coreaudio` (cargo build --release -p auto-camera --features coreaudio)"
} else {
    "audio devices (kind = \"device\") are CoreAudio devices, which only macOS has; \
     on macOS, build auto-camera with `--features coreaudio`"
};

/// One audio device as the system lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Its CoreAudio UID: the identity a config names it by.
    pub uid: String,
    /// Its name, as macOS shows it (not unique: two interfaces of one
    /// model share it).
    pub name: String,
    /// Input channels, all streams together; 0 for an output-only device.
    pub input_channels: u16,
    /// Its nominal sample rate now, in Hz.
    pub sample_rate: u32,
    /// It is the system's default input. Shown in listings only: nothing
    /// is ever opened for being the default.
    pub default_input: bool,
}

impl AudioDevice {
    /// True when it has input channels.
    pub fn is_input(&self) -> bool {
        self.input_channels > 0
    }
}

impl fmt::Display for AudioDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = if self.input_channels == 1 { "" } else { "s" };
        write!(
            f,
            "{:?} ({}, {} input channel{s} at {} Hz)",
            self.uid, self.name, self.input_channels, self.sample_rate
        )
    }
}

/// The machine's audio devices, behind a trait so that everything but the
/// CoreAudio calls is tested with a fake; see the module docs.
pub trait DeviceBackend: Send + Sync {
    /// Every audio device there is now, inputs or not, in the system's
    /// order; never cached, since the answer holds only until a device is
    /// plugged or unplugged.
    fn devices(&self) -> Result<Vec<AudioDevice>, MediaError>;

    /// Starts capturing every input channel of `device` (as
    /// [`DeviceBackend::devices`] listed it) at its listed rate: each input
    /// buffer goes to `sink` with its capture time, an overload or the loss
    /// of the stream to `events`. Capture runs until the stream returned is
    /// dropped.
    fn start(
        &self,
        device: &AudioDevice,
        sink: CaptureSink,
        events: StreamEvents,
    ) -> Result<Box<dyn DeviceStream>, MediaError>;
}

/// A running capture; dropping it stops the device's callbacks.
pub trait DeviceStream: Send {
    /// The host clock now, on the time base of the capture times the
    /// stream hands to its sink.
    fn host_now(&self) -> std::time::Duration;
}

/// The device with exactly the UID `uid` among `devices`; see the module
/// docs. Fails, naming the input devices there are, when no device has it;
/// fails too when it has no inputs, or when several devices claim it. No
/// other device is ever chosen in its place.
pub fn select<'a>(devices: &'a [AudioDevice], uid: &str) -> Result<&'a AudioDevice, MediaError> {
    let fail = |why: String| Err(MediaError::Open(why));
    if uid.trim().is_empty() {
        return fail("an audio device's UID cannot be empty".into());
    }
    let mut matching = devices.iter().filter(|d| d.uid == uid);
    match (matching.next(), matching.next()) {
        (Some(device), None) if device.is_input() => {
            if device.sample_rate == 0 {
                return fail(format!("audio device {device} reports no sample rate"));
            }
            Ok(device)
        }
        (Some(device), None) => fail(format!(
            "audio device {:?} ({}) has no input channels",
            device.uid, device.name
        )),
        (Some(_), Some(_)) => fail(format!(
            "{} audio devices report the UID {uid:?}; unplug all but one",
            devices.iter().filter(|d| d.uid == uid).count()
        )),
        (None, _) => {
            let inputs: Vec<String> = devices
                .iter()
                .filter(|d| d.is_input())
                .map(AudioDevice::to_string)
                .collect();
            let there = if inputs.is_empty() {
                "no input device is connected".into()
            } else {
                format!("the input devices connected: {}", inputs.join(", "))
            };
            fail(format!(
                "no audio device with UID {uid:?} is connected (no other device is opened \
                 in its place); {there}"
            ))
        }
    }
}

/// The input devices among `devices`, in their order.
pub fn inputs(devices: Vec<AudioDevice>) -> Vec<AudioDevice> {
    devices.into_iter().filter(AudioDevice::is_input).collect()
}

/// The machine's CoreAudio devices: CoreAudio itself, through cpal, in a
/// build where [`SUPPORTED`]; in any other build every call fails with
/// [`UNSUPPORTED`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemDevices;

#[cfg(all(feature = "coreaudio", target_os = "macos"))]
impl DeviceBackend for SystemDevices {
    fn devices(&self) -> Result<Vec<AudioDevice>, MediaError> {
        coreaudio::devices()
    }

    fn start(
        &self,
        device: &AudioDevice,
        sink: CaptureSink,
        events: StreamEvents,
    ) -> Result<Box<dyn DeviceStream>, MediaError> {
        coreaudio::start(device, sink, events)
    }
}

#[cfg(not(all(feature = "coreaudio", target_os = "macos")))]
impl DeviceBackend for SystemDevices {
    fn devices(&self) -> Result<Vec<AudioDevice>, MediaError> {
        Err(MediaError::Open(UNSUPPORTED.into()))
    }

    fn start(
        &self,
        _device: &AudioDevice,
        _sink: CaptureSink,
        _events: StreamEvents,
    ) -> Result<Box<dyn DeviceStream>, MediaError> {
        Err(MediaError::Open(UNSUPPORTED.into()))
    }
}

#[cfg(test)]
mod tests;
