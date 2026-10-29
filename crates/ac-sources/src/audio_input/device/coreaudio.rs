//! [`super::SystemDevices`] on macOS: CoreAudio through cpal's CoreAudio
//! host, the only part of the device input that calls CoreAudio.
//!
//! # Theory
//!
//! cpal gives what the input needs, through a safe API (the workspace
//! forbids `unsafe`):
//!
//! * **identity**: `Device::id()` is the device's
//!   `kAudioDevicePropertyDeviceUID`, the UID a config names;
//! * **format**: the input channels of all the device's input streams
//!   (`supported_input_configs`, from its stream configuration) and its
//!   current nominal rate (`default_input_config`, from its stream
//!   format). The stream is opened at exactly that rate, so cpal leaves
//!   the device's rate as it is, as `f32`, which the AUHAL converts to;
//! * **time**: every input callback's `capture` instant, the buffer's host
//!   time (`mHostTime`) less the device's latency, in nanoseconds of the
//!   host clock, and `Stream::now()` on the same time base;
//! * **trouble**: its error callback reports an overload
//!   (`kAudioDeviceProcessorOverload`) as `Xrun`, from the real-time
//!   thread, and the device going away (`kAudioDevicePropertyDeviceIsAlive`)
//!   or its rate changing as `DeviceNotAvailable` and `StreamInvalidated`,
//!   from a thread of cpal's.
//!
//! The data callback only hands its slice and capture time to the
//! [`CaptureSink`]; the error callback only bumps the overload count or
//! records the loss ([`StreamEvents`]). Everything else is the input's.

use std::time::Duration;

use ac_media::MediaError;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, HostId, InputCallbackInfo, StreamConfig, StreamInstant};

use super::{AudioDevice, CaptureSink, DeviceStream, StreamEvents};

/// A CoreAudio failure as an open error.
fn open_error(what: &str, e: cpal::Error) -> MediaError {
    MediaError::Open(format!("CoreAudio: {what}: {e}"))
}

/// `instant` as a duration of the host clock.
fn host_time(instant: StreamInstant) -> Duration {
    Duration::from_nanos(u64::try_from(instant.as_nanos()).unwrap_or(u64::MAX))
}

/// The CoreAudio host.
fn coreaudio() -> Result<cpal::Host, MediaError> {
    cpal::host_from_id(HostId::CoreAudio).map_err(|e| open_error("no host", e))
}

/// Every CoreAudio device, as [`super::DeviceBackend::devices`] lists them;
/// a device that goes away while it is being looked at is left out.
pub(super) fn devices() -> Result<Vec<AudioDevice>, MediaError> {
    let host = coreaudio()?;
    let default_uid = host
        .default_input_device()
        .and_then(|d| d.id().ok())
        .map(|id| id.id().to_owned());
    let mut out = Vec::new();
    for device in host
        .devices()
        .map_err(|e| open_error("cannot list devices", e))?
    {
        let Ok(id) = device.id() else {
            continue;
        };
        let uid = id.id().to_owned();
        let name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| uid.clone());
        let input_channels = device
            .supported_input_configs()
            .ok()
            .and_then(|mut configs| configs.next())
            .map_or(0, |c| c.channels());
        let sample_rate = device
            .default_input_config()
            .or_else(|_| device.default_output_config())
            .map_or(0, |c| c.sample_rate());
        out.push(AudioDevice {
            default_input: default_uid.as_deref() == Some(uid.as_str()),
            uid,
            name,
            input_channels,
            sample_rate,
        });
    }
    Ok(out)
}

/// A running cpal input stream.
struct Stream(cpal::Stream);

impl DeviceStream for Stream {
    fn host_now(&self) -> Duration {
        host_time(self.0.now())
    }
}

/// Starts capturing `device` into `sink`; see [`super::DeviceBackend::start`].
pub(super) fn start(
    device: &AudioDevice,
    mut sink: CaptureSink,
    events: StreamEvents,
) -> Result<Box<dyn DeviceStream>, MediaError> {
    let host = coreaudio()?;
    let wanted = cpal::DeviceId::new(HostId::CoreAudio, &device.uid);
    let found = host
        .device_by_id(&wanted)
        .ok_or_else(|| MediaError::Open(format!("audio device {device} is gone")))?;
    let config = StreamConfig {
        channels: device.input_channels,
        sample_rate: device.sample_rate,
        buffer_size: BufferSize::Default,
    };
    let data = move |samples: &[f32], info: &InputCallbackInfo| {
        sink.deliver(host_time(info.timestamp().capture), samples);
    };
    let trouble = move |e: cpal::Error| match e.kind() {
        ErrorKind::Xrun => events.overload(),
        _ => events.lost(e.to_string()),
    };
    let stream = found
        .build_input_stream::<f32, _, _>(config, data, trouble, None)
        .map_err(|e| open_error("cannot open the input stream", e))?;
    stream
        .play()
        .map_err(|e| open_error("cannot start the input stream", e))?;
    Ok(Box::new(Stream(stream)))
}
