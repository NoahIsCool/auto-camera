//! Which ffmpeg avfoundation device a unique ID is now: parsing the two
//! listings and matching them.
//!
//! # Theory
//!
//! ffmpeg opens an avfoundation camera by its index in ffmpeg's own device
//! list (`ffmpeg -f avfoundation -list_devices true -i ""`), and that index
//! is not stable: it is the camera's position among the devices connected
//! now, so unplugging one camera, or plugging it into another port, can
//! move every camera after it. macOS's stable name for a camera is its
//! unique ID (`AVCaptureDevice.uniqueID`, which `system_profiler
//! SPCameraDataType -json` shows as `spcamera_unique-id`), but ffmpeg's list
//! does not show it. So a source is configured by unique ID and the index is
//! worked out again before every open ([`resolve`]):
//!
//! 1. system_profiler's list gives each camera's name and unique ID. The
//!    unique ID is looked up there; a camera that is not listed is an error,
//!    with no fallback to another camera (not to one of the same name, nor
//!    to the old index), because a wrong picture on air is worse than a
//!    source that says it is offline.
//! 2. ffmpeg's list gives each device's index and name. The camera is the
//!    ffmpeg device of the same name.
//! 3. Two cameras of one model have the same name. The assumption is that
//!    both lists give such twins in the same order (both come from the same
//!    AVFoundation enumeration), so the `k`-th twin in system_profiler's list
//!    is the `k`-th in ffmpeg's. That holds only while both lists have the
//!    same number of devices of that name; when they do not (a virtual
//!    camera of the same name that only one tool lists), the camera cannot
//!    be told apart and resolving is an error, again with no fallback. A
//!    run on a Mac with two identical webcams confirms the order assumption.
//!
//! # Invariants
//!
//! * Pure: the listings come in as text ([`parse_device_list`],
//!   [`parse_camera_report`]), so every rule here is tested on canned
//!   output without a Mac. Running the tools is
//!   [`DeviceInventory`](super::DeviceInventory)'s job.
//! * Names match exactly (after trimming); unique IDs match ignoring ASCII
//!   case, since a hex ID typed in a config may be upper case.
//! * A resolved device always names an index ffmpeg listed.
//! * Every error lists the connected cameras with their unique IDs, so the
//!   operator can see which ID to configure.

use std::fmt;

use ac_media::MediaError;
use serde::Deserialize;

use crate::logs::strip_context;

/// The heading ffmpeg prints before its avfoundation video devices.
const VIDEO_HEADING: &str = "AVFoundation video devices:";

/// The heading ffmpeg prints before its avfoundation audio devices.
const AUDIO_HEADING: &str = "AVFoundation audio devices:";

/// A video device as ffmpeg's avfoundation input lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvDevice {
    /// Its index, which `-i "<index>:none"` opens.
    pub index: u32,
    /// Its name (AVFoundation's localized name).
    pub name: String,
}

/// A camera as `system_profiler SPCameraDataType -json` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemCamera {
    /// Its name, the same as ffmpeg shows.
    pub name: String,
    /// Its unique ID, when system_profiler gives one.
    pub unique_id: Option<String>,
    /// Its model ID (for a USB camera, its vendor and product), if given.
    pub model_id: Option<String>,
}

/// A unique ID matched to the ffmpeg device it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDevice {
    /// The unique ID, as system_profiler gives it.
    pub unique_id: String,
    /// The camera's name.
    pub name: String,
    /// The index ffmpeg opens it by, valid until the cameras change.
    pub index: u32,
    /// How many connected cameras have this name (1 unless twins are
    /// connected, which were told apart by order).
    pub same_name: usize,
}

impl fmt::Display for ResolvedDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} (unique ID {}, ffmpeg device {})",
            self.name, self.unique_id, self.index
        )
    }
}

/// Parses the video devices from ffmpeg's `-list_devices true` output
/// (stderr), in index order. An error when the output has no avfoundation
/// device list at all (an ffmpeg built without avfoundation, or not on a
/// Mac), quoting its first line; an empty list when it lists no video
/// device.
pub fn parse_device_list(text: &str) -> Result<Vec<AvDevice>, MediaError> {
    let mut in_video = false;
    let mut seen = false;
    let mut devices: Vec<AvDevice> = Vec::new();
    for line in text.lines().map(|l| strip_context(l.trim())) {
        if line.ends_with(VIDEO_HEADING) {
            (in_video, seen) = (true, true);
        } else if line.ends_with(AUDIO_HEADING) {
            in_video = false;
        } else if in_video
            && let Some(device) = numbered(line)
            && devices.iter().all(|d| d.index != device.index)
        {
            devices.push(device);
        }
    }
    if !seen {
        // ffmpeg says why first ("Unknown input format"), then that it
        // could not open the input.
        let first = text.lines().map(str::trim).find(|l| !l.is_empty());
        return Err(MediaError::Open(format!(
            "ffmpeg lists no avfoundation devices (it needs macOS and an ffmpeg \
             built with avfoundation): {}",
            first.map_or("it printed nothing", strip_context)
        )));
    }
    devices.sort_by_key(|d| d.index);
    Ok(devices)
}

/// A `[3] Name` line as a device; `None` for anything else.
fn numbered(line: &str) -> Option<AvDevice> {
    let (index, name) = line.strip_prefix('[')?.split_once("] ")?;
    let name = name.trim();
    if name.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(AvDevice {
        index: index.parse().ok()?,
        name: name.to_owned(),
    })
}

/// Top level of `system_profiler SPCameraDataType -json`.
#[derive(Debug, Deserialize)]
struct RawReport {
    /// The cameras; absent when there are none.
    #[serde(rename = "SPCameraDataType", default)]
    cameras: Vec<RawCamera>,
}

/// One camera's entry; every field is optional in practice.
#[derive(Debug, Deserialize)]
struct RawCamera {
    /// The camera's name.
    #[serde(rename = "_name")]
    name: Option<String>,
    /// Its unique ID.
    #[serde(rename = "spcamera_unique-id")]
    unique_id: Option<String>,
    /// Its model ID.
    #[serde(rename = "spcamera_model-id")]
    model_id: Option<String>,
}

/// Parses `system_profiler SPCameraDataType -json`, in its order (which
/// tells twins apart; see the module docs). Entries without a name are
/// left out, since no ffmpeg device can match them; an empty or blank
/// unique ID counts as none.
pub fn parse_camera_report(json: &str) -> Result<Vec<SystemCamera>, MediaError> {
    let raw: RawReport = serde_json::from_str(json)
        .map_err(|e| MediaError::Open(format!("unreadable system_profiler output: {e}")))?;
    let text = |s: Option<String>| s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    Ok(raw
        .cameras
        .into_iter()
        .filter_map(|c| {
            Some(SystemCamera {
                name: text(c.name)?,
                unique_id: text(c.unique_id),
                model_id: text(c.model_id),
            })
        })
        .collect())
}

/// The ffmpeg device that the camera with `unique_id` is now, from
/// system_profiler's `cameras` and ffmpeg's `devices`; see the module docs
/// for the rules and why no error falls back to another device. Every
/// error ends with the connected cameras.
pub fn resolve(
    unique_id: &str,
    cameras: &[SystemCamera],
    devices: &[AvDevice],
) -> Result<ResolvedDevice, MediaError> {
    let wanted = unique_id.trim();
    let is_it = |c: &SystemCamera| {
        c.unique_id
            .as_deref()
            .is_some_and(|u| u.eq_ignore_ascii_case(wanted))
    };
    let Some(at) = cameras.iter().position(is_it) else {
        return Err(MediaError::Open(format!(
            "no camera with unique ID {wanted:?} is connected ({})",
            connected(cameras)
        )));
    };
    let camera = &cameras[at];
    let twins = cameras.iter().filter(|c| c.name == camera.name).count();
    let rank = cameras[..at]
        .iter()
        .filter(|c| c.name == camera.name)
        .count();
    let listed: Vec<&AvDevice> = devices.iter().filter(|d| d.name == camera.name).collect();
    if listed.len() != twins {
        let why = match listed.len() {
            0 => format!(
                "camera {:?} (unique ID {wanted}) is connected, but ffmpeg's avfoundation \
                 input does not list it",
                camera.name
            ),
            n => format!(
                "system_profiler lists {twins} camera(s) named {:?} and ffmpeg {n} \
                 device(s), so the one with unique ID {wanted} cannot be told apart",
                camera.name
            ),
        };
        return Err(MediaError::Open(format!("{why} ({})", connected(cameras))));
    }
    Ok(ResolvedDevice {
        unique_id: camera
            .unique_id
            .clone()
            .unwrap_or_else(|| wanted.to_owned()),
        name: camera.name.clone(),
        index: listed[rank].index,
        same_name: twins,
    })
}

/// The connected cameras for an error message: each name with its unique
/// ID, or that there are none.
fn connected(cameras: &[SystemCamera]) -> String {
    if cameras.is_empty() {
        return "no camera is connected".into();
    }
    let each: Vec<String> = cameras
        .iter()
        .map(|c| match &c.unique_id {
            Some(u) => format!("{:?} {u}", c.name),
            None => format!("{:?} without a unique ID", c.name),
        })
        .collect();
    format!("connected: {}", each.join(", "))
}

#[cfg(test)]
mod tests;
