//! Device inputs against a real CoreAudio input device (feature
//! `coreaudio-hw`, macOS):
//!
//! ```sh
//! cargo test -p ac-sources --features coreaudio-hw --test coreaudio_hw -- --nocapture
//! ```
//!
//! Each test skips (passes with a note) when the devices cannot be listed
//! (not macOS, or a build without CoreAudio) or no input device is
//! connected. `AC_AUDIO_UID` picks the device to open (default: the first
//! input device listed). macOS asks once whether the terminal may use the
//! microphone; a refused terminal gets silence, which still has to arrive
//! with rising PTS.

#![cfg(feature = "coreaudio-hw")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::{AudioBlock, AudioInput, SourceState};
use ac_sources::audio_input::device::inputs;
use ac_sources::{
    AudioDevice, DeviceBackend, DeviceInput, DeviceOptions, SystemDevices, WallClock,
};

/// The device to open, or `None` (and a note) when there is none or the
/// devices cannot be listed.
fn device() -> Option<AudioDevice> {
    let devices = match SystemDevices.devices() {
        Ok(devices) => inputs(devices),
        Err(e) => {
            eprintln!("skipping: {e}");
            return None;
        }
    };
    for d in &devices {
        eprintln!(
            "input device {d}{}",
            if d.default_input { " (default)" } else { "" }
        );
    }
    let wanted = std::env::var("AC_AUDIO_UID").ok();
    let found = match &wanted {
        Some(uid) => devices.into_iter().find(|d| &d.uid == uid),
        None => devices.into_iter().next(),
    };
    if found.is_none() {
        eprintln!(
            "skipping: no input device {}",
            wanted.map_or("is connected".into(), |uid| format!("has UID {uid:?}"))
        );
    }
    found
}

#[test]
fn every_input_device_has_a_uid_channels_and_a_rate() {
    let Some(_) = device() else {
        return;
    };
    for d in inputs(SystemDevices.devices().unwrap()) {
        assert!(!d.uid.is_empty(), "{d}");
        assert!(d.input_channels > 0 && d.sample_rate > 0, "{d}");
    }
}

#[test]
fn a_device_delivers_a_second_of_blocks_with_rising_pts() {
    let Some(device) = device() else {
        return;
    };
    let mut input = DeviceInput::open(
        SourceId("hw".into()),
        &device.uid,
        &SystemDevices,
        DeviceOptions::default(),
        Arc::new(WallClock::new()),
    )
    .unwrap();
    assert_eq!(
        input.info().format,
        Some((device.sample_rate, device.input_channels))
    );
    let started = Instant::now();
    let mut blocks: Vec<AudioBlock> = Vec::new();
    while started.elapsed() < Duration::from_secs(1) {
        input.wait(Duration::from_millis(50));
        blocks.extend(input.drain());
    }
    blocks.extend(input.drain());
    assert_eq!(input.state(), SourceState::Running, "{:?}", input.error());
    let frames: usize = blocks.iter().map(AudioBlock::frames).sum();
    eprintln!(
        "{} blocks, {frames} frames in 1 s; {} discontinuities, {} overloads; error {:?}",
        blocks.len(),
        input.discontinuities(),
        input.overloads(),
        input.error()
    );
    assert!(blocks.len() > 10, "{} blocks", blocks.len());
    // Most of a second of audio (the first buffer takes a moment).
    assert!(
        frames as f64 > 0.5 * f64::from(device.sample_rate),
        "{frames}"
    );
    assert!(blocks.windows(2).all(|w| w[1].seq > w[0].seq));
    assert!(blocks.windows(2).all(|w| w[1].pts > w[0].pts), "PTS rise");
    let last = blocks.last().unwrap();
    assert!(
        last.pts < Duration::from_secs(2),
        "PTS on the clock since the open: {:?}",
        last.pts
    );
    input.stop();
    assert_eq!(input.state(), SourceState::Stopped);
}
