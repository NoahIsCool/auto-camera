//! Capture sources against real cameras (feature `capture-hw`, macOS with
//! an ffmpeg built with avfoundation):
//!
//! ```sh
//! cargo test -p ac-sources --features capture-hw --test capture_hw -- --nocapture
//! ```
//!
//! Each test skips (passes with a note) when the cameras cannot be listed
//! (not macOS) or none is connected. `AC_CAPTURE_UID` picks the camera to
//! open (default: the first one listed) and `AC_CAPTURE_MODE` its mode as
//! `WIDTHxHEIGHT@FPS` (default `1280x720@30`, which most webcams and
//! dongles offer).

#![cfg(feature = "capture-hw")]

use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{SourceState, VideoSource};
use ac_sources::capture::{parse_camera_report, parse_device_list, resolve_device};
use ac_sources::{
    CaptureFormat, CaptureOptions, CaptureSource, DeviceInventory, FfmpegTools, SystemInventory,
};

/// The connected cameras' unique IDs, or `None` (and a note) when there
/// are none or they cannot be listed.
fn connected(inventory: &SystemInventory) -> Option<Vec<String>> {
    let report = match inventory.camera_report() {
        Ok(report) => report,
        Err(e) => {
            eprintln!("skipping: {e}");
            return None;
        }
    };
    let ids: Vec<String> = parse_camera_report(&report)
        .unwrap()
        .into_iter()
        .filter_map(|c| c.unique_id)
        .collect();
    if ids.is_empty() {
        eprintln!("skipping: no camera is connected");
        return None;
    }
    Some(ids)
}

/// The mode to ask for: `AC_CAPTURE_MODE`, else 1280x720 at 30 fps.
fn mode() -> CaptureFormat {
    let text = std::env::var("AC_CAPTURE_MODE").unwrap_or_else(|_| "1280x720@30".into());
    let parsed = text.split_once('@').and_then(|(size, fps)| {
        let (w, h) = size.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?, fps.parse().ok()?))
    });
    let (w, h, fps) = parsed.unwrap_or_else(|| panic!("AC_CAPTURE_MODE {text:?} is not WxH@FPS"));
    CaptureFormat::new(w, h, fps, None).unwrap()
}

#[test]
fn every_connected_camera_resolves_to_an_ffmpeg_device() {
    let inventory = SystemInventory::new(&FfmpegTools::from_env());
    let Some(ids) = connected(&inventory) else {
        return;
    };
    let devices = parse_device_list(&inventory.ffmpeg_devices().unwrap()).unwrap();
    for id in ids {
        let device = resolve_device(&inventory, &id).unwrap();
        eprintln!("{device}");
        assert!(
            devices
                .iter()
                .any(|d| d.index == device.index && d.name == device.name)
        );
    }
}

#[test]
fn a_connected_camera_delivers_the_requested_mode() {
    let tools = FfmpegTools::from_env();
    let inventory = SystemInventory::new(&tools);
    let Some(ids) = connected(&inventory) else {
        return;
    };
    let uid = std::env::var("AC_CAPTURE_UID").unwrap_or_else(|_| ids[0].clone());
    let format = mode();
    let options = CaptureOptions {
        tools,
        ..CaptureOptions::default()
    };
    let mut src =
        CaptureSource::open(SourceId("hw".into()), &uid, &format, &inventory, options).unwrap();
    eprintln!("capturing {} at {:?}", src.device(), format);
    let pids = src.child_pids();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut frames = 0;
    let mut last = None;
    while frames < 30 && Instant::now() < deadline && src.state() != SourceState::Stopped {
        if let Some(f) = src.wait_newer(last, Duration::from_millis(100)) {
            assert_eq!((f.width, f.height), format.size());
            last = Some(f.seq);
            frames += 1;
        }
    }
    assert_eq!(src.error(), None, "the device refused the mode");
    assert_eq!(frames, 30, "30 frames within 20 s");
    src.stop();
    for pid in pids {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(!alive, "ffmpeg {pid} outlived its source");
    }
}
