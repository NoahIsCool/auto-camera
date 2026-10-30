//! Cross-PR tests: a CoreAudio board's audio as the device input stamps it
//! (its hand-off, [`CaptureLatency`] and [`Stamper`], from `ac-sources`),
//! through program audio's drift correction ([`ProgramAudio`] and its
//! input's `InputDrift`) to the output's pacer: the way a board on the
//! Mac's USB reaches the encoder.
//!
//! # Theory
//!
//! Each half deals with timestamps that are wrong in its own way. The
//! device input holds the capture latency of a connection's first buffer,
//! so a change in the callbacks' length cannot move its PTS, and undoes a
//! host-time excursion that comes back within its window, so a glitch
//! leaves the PTS on the old timeline; a jump that lasts stays in the PTS.
//! The drift correction behind it holds a block whose PTS stepped, takes
//! the blocks in again when the ones after them come back, and makes up a
//! step (silence for audio lost, a cut for PTS that went back) only once
//! the blocks after it confirm it. Each half is tested on its own against
//! made-up timestamps; these tests check that what one hands the other
//! adds up: that no glitch the device input passes on is made up as a
//! loss, and that a real loss is made up once.
//!
//! The device is a model of what cpal's CoreAudio host delivers: a sample
//! clock some ppm off the host clock; IO buffers of a size that may change;
//! for each, `mHostTime`, the host time of its first frame give or take
//! [`JITTER`]; cpal's capture time, that less the IO buffer (this
//! callback's own length when the device does not report its size) and the
//! device's latency and safety offset; and the callback, at the buffer's
//! end plus up to [`CALLBACK_DELAY`]. Its events are buffers whose host
//! time is off (a glitch of one buffer, an excursion of several), IO
//! cycles it skips (their audio lost, as in an overload) and a change of
//! buffer size. The engine ticks at [`FPS`] and drains what was delivered
//! by then.
//!
//! What reaches the encoder is read from the pacer's lead (audio pushed,
//! silence included, less the video's share), which only audio added or
//! taken away moves. A run is compared with its *twin*, the same device
//! without the event: every buffer jitters by its own frame number, so the
//! twin's blocks come at the same ticks with the same frames, and the
//! difference of the two leads is what was put in or cut for the event.
//!
//! # Invariants checked
//!
//! * A one-buffer glitch of ±1.5, 3, 8 or 15 ms at 128, 512 or 1024
//!   frames: the lead is the twin's within 1 ms at every tick, nothing is
//!   filled or cut (no step made up, no silence or drop in the pacer), and
//!   the device input reports one glitch when the glitch was beyond its
//!   tolerance, else nothing.
//! * Four buffers off as much: no lasting offset and no break. (The drift
//!   correction may take such an excursion for a step once its fourth
//!   block confirms it, and take it back once the blocks after it confirm
//!   the way back: the silence and the cut cancel.)
//! * One IO cycle lost: made up once, by the frames lost.
//! * A board 37 ppm fast for three hours, a cycle lost every ten minutes:
//!   the lead within 20 ms of where it settled throughout, each loss made
//!   up once, the drift corrected.
//! * Buffers switching between 512 and 128 frames every 30 s on a device
//!   that does not report its buffer size: the same PTS and lead as a
//!   device that does, with nothing reported or made up.

use std::time::Duration;

use ac_core::SourceId;
use ac_media::AudioBlock;
use ac_sources::audio_input::device::{CaptureLatency, Discontinuity, Stamper, handoff};

use crate::audio::{AudioPacer, AudioPacerStats};
use crate::output::{AUDIO_MAX_LEAD, AUDIO_SLACK};
use crate::program_audio::tests::scripted;
use crate::program_audio::{AudioInputView, Origin, ProgramAudio};

/// Host time of the device's first frame, in seconds.
const HOST0: f64 = 100.0;

/// Host time at which the connection opened, in seconds: the PTS origin
/// and the start of the video's timeline.
const OPENED: f64 = HOST0 - 0.003;

/// Video frames per second.
const FPS: u64 = 30;

/// The device's latency and safety offset, in frames, which cpal takes off
/// the host time with the IO buffer.
const EXTRA: u64 = 40;

/// Most a buffer's host time is off its first frame's, either way, in
/// seconds.
const JITTER: f64 = 30e-6;

/// Most a callback runs after its buffer ends, in seconds.
const CALLBACK_DELAY: f64 = 150e-6;

/// The hand-off's largest block, in frames.
const MAX_FRAMES: usize = 1024;

/// The rate the short runs play at, in Hz.
const RATE: u32 = 48_000;

/// How long the short runs play, in seconds.
const SECONDS: u64 = 30;

/// When their event comes, in seconds of the device's clock.
const EVENT_AT: f64 = 15.0;

/// A number in `-1.0..1.0` that depends on `key` alone (SplitMix64), so
/// that a buffer jitters alike in a run and its twin.
fn spread(key: u64) -> f64 {
    let mut z = key.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 52) as f64 - 1.0
}

/// `s` seconds as a duration.
fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

/// What happens to the device, from a time of its own clock on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Event {
    /// Its IO buffers have this many frames.
    Buffer(u64),
    /// The host times of this many buffers are this many seconds off.
    Off(u64, f64),
    /// It skips this many IO cycles: their audio is lost.
    Lose(u64),
}

/// A CoreAudio device as cpal delivers it, and the device input's capture
/// latency; see the module docs.
struct Device {
    /// Nominal sample rate.
    rate: u32,
    /// How fast its sample clock runs, in ppm of the host clock.
    ppm: f64,
    /// Frames per IO buffer now.
    buffer: u64,
    /// The IO buffer size cpal read when the stream was built, if it could.
    reported: Option<u64>,
    /// What happens to it, at seconds of its own clock, in order.
    events: Vec<(f64, Event)>,
    /// Buffers whose host time is still to be off, and by how much.
    off: (u64, f64),
    /// Its next frame.
    frame: u64,
    /// The latency the device input holds for the connection, or `None` to
    /// hand on cpal's capture time as it is.
    latency: Option<CaptureLatency>,
    /// When its latest callback ran, in host seconds.
    called: f64,
}

impl Device {
    /// A device at `rate` with IO buffers of `buffer` frames, whose size
    /// cpal read, 0.5 ppm fast, to which nothing happens.
    fn new(rate: u32, buffer: u64) -> Self {
        Self {
            rate,
            ppm: 0.5,
            buffer,
            reported: Some(buffer),
            events: Vec::new(),
            off: (0, 0.0),
            frame: 0,
            latency: Some(CaptureLatency::default()),
            called: 0.0,
        }
    }

    /// The device with `events`.
    fn with(mut self, events: &[(f64, Event)]) -> Self {
        self.events = events.to_vec();
        self
    }

    /// Host time of its frame `frame`, in seconds.
    fn host_of(&self, frame: u64) -> f64 {
        HOST0 + frame as f64 / (f64::from(self.rate) * (1.0 + self.ppm * 1e-6))
    }

    /// Its next callback: when it runs, in host seconds, the capture time
    /// handed to the sink, and its frames.
    fn callback(&mut self) -> (f64, Duration, u64) {
        let rate = f64::from(self.rate);
        while let Some(&(at, event)) = self.events.first()
            && at <= self.frame as f64 / rate
        {
            self.events.remove(0);
            match event {
                Event::Buffer(frames) => self.buffer = frames,
                Event::Off(buffers, by) => self.off = (buffers, by),
                Event::Lose(cycles) => self.frame += cycles * self.buffer,
            }
        }
        let frames = self.buffer;
        let mut host = self.host_of(self.frame) + spread(2 * self.frame) * JITTER;
        if self.off.0 > 0 {
            host += self.off.1;
            self.off.0 -= 1;
        }
        let latency = (self.reported.unwrap_or(frames) + EXTRA) as f64 / rate;
        let (stamp, capture) = (secs(host), secs(host - latency));
        let at = match &mut self.latency {
            Some(held) => held.capture(stamp, capture),
            None => capture,
        };
        self.frame += frames;
        let delay = (spread(2 * self.frame + 1) + 1.0) / 2.0 * CALLBACK_DELAY;
        self.called = (self.host_of(self.frame) + delay).max(self.called);
        (self.called, at, frames)
    }
}

/// What the device input and the engine made of a device's audio.
struct Run {
    /// The pacer's lead after each tick, in ms.
    lead: Vec<f64>,
    /// What the device input reported.
    found: Vec<Discontinuity>,
    /// Frames the drift correction passed on less those it was given:
    /// silence it put in, less audio it cut, plus what resampling changed.
    edited: i64,
    /// The pacer's counters at the end.
    pacer: AudioPacerStats,
    /// The board as the operator sees it at the end.
    view: AudioInputView,
}

impl Run {
    /// The largest difference of its lead from `twin`'s, in ms, over the
    /// ticks from `from` seconds on.
    fn apart(&self, twin: &Run, from: f64) -> f64 {
        let first = (from * FPS as f64) as usize;
        self.lead[first..]
            .iter()
            .zip(&twin.lead[first..])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max)
    }
}

/// Sample frames in `block`.
fn frames(block: &AudioBlock) -> i64 {
    (block.samples.len() / usize::from(block.channels)) as i64
}

/// Plays `device` for `seconds` through the device input's hand-off and
/// stamper, program audio and the output's pacer; see the module docs.
fn play(mut device: Device, seconds: u64) -> Run {
    let rate = device.rate;
    let (mut sink, reader) = handoff(2, rate, MAX_FRAMES, 64);
    let mut stamper = Stamper::new(SourceId("board".into()), rate, 2, secs(OPENED));
    let (slot, handles) = scripted("board", Some((rate, 2)), None);
    let mut audio = ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]);
    let mut pacer = AudioPacer::new(rate, 2, FPS as u32, AUDIO_SLACK, AUDIO_MAX_LEAD);
    let silence = [0.0; 2 * MAX_FRAMES];
    let (mut lead, mut found, mut edited) = (Vec::new(), Vec::new(), 0);
    let mut next = device.callback();
    for tick in 1..=seconds * FPS {
        let now = OPENED + tick as f64 / FPS as f64;
        let mut blocks = Vec::new();
        while next.0 <= now {
            let (_, at, length) = next;
            sink.deliver(at, &silence[..2 * length as usize]);
            while let Some(captured) = reader.take() {
                let (block, notes) = stamper.stamp(&captured);
                reader.recycle(captured);
                found.extend(notes);
                edited -= frames(&block);
                blocks.push(block);
            }
            next = device.callback();
        }
        handles.queue.push(blocks);
        let taken = audio.take(&[]);
        edited += taken.iter().map(frames).sum::<i64>();
        pacer.plan(tick, taken);
        let stats = pacer.stats();
        let ahead = (stats.pushed + stats.silence) as f64 - pacer.target(tick) as f64;
        lead.push(ahead * 1000.0 / f64::from(rate));
    }
    Run {
        lead,
        found,
        edited,
        pacer: pacer.stats(),
        view: audio.views(None).remove(0),
    }
}

/// The device input's tolerance after a block of `frames` at [`RATE`], in
/// seconds: half the block, at least 1 ms.
fn tolerance(frames: u64) -> f64 {
    (frames as f64 / 2.0 / f64::from(RATE)).max(0.001)
}

/// Checks that `run` made up nothing and left the pacer nothing to do,
/// saying `what` if not.
fn untouched(run: &Run, what: &str) {
    let (view, pacer) = (&run.view, &run.pacer);
    assert_eq!((view.drift_steps, view.drift_resets), (0, 0), "{what}");
    assert_eq!((pacer.silence, pacer.dropped), (0, 0), "{what}");
}

#[test]
fn a_one_buffer_host_time_glitch_is_neither_filled_nor_cut() {
    for frames in [128, 512, 1024] {
        let twin = play(Device::new(RATE, frames), SECONDS);
        untouched(&twin, "twin");
        assert!(
            twin.found.is_empty() && twin.edited == 0,
            "{:?}",
            twin.found
        );
        for ms in [1.5, 3.0, 8.0, 15.0] {
            for by in [ms, -ms] {
                let off = [(EVENT_AT, Event::Off(1, by / 1000.0))];
                let run = play(Device::new(RATE, frames).with(&off), SECONDS);
                let what = format!("{by} ms at {frames} frames: {:?}", run.found);
                untouched(&run, &what);
                assert_eq!(run.edited, 0, "{what}");
                assert!(run.apart(&twin, 0.0) <= 1.0, "{what}");
                if ms / 1000.0 > tolerance(frames) {
                    assert!(
                        matches!(run.found[..], [Discontinuity::Glitch { blocks: 1, .. }]),
                        "{what}"
                    );
                } else {
                    assert!(run.found.is_empty(), "{what}");
                }
            }
        }
    }
}

#[test]
fn an_excursion_of_four_buffers_leaves_no_lasting_offset() {
    // Four buffers off as much: the device input says it was a glitch
    // when it was beyond its tolerance, and the board's audio ends up
    // where it was. (The drift correction may take such an excursion for
    // a step once its fourth block confirms it, and take it back as the
    // blocks after it confirm the way back: the silence and the cut then
    // cancel.)
    for frames in [128, 512, 1024] {
        let twin = play(Device::new(RATE, frames), SECONDS);
        for ms in [1.5, 3.0, 8.0, 15.0] {
            for by in [ms, -ms] {
                let off = [(EVENT_AT, Event::Off(4, by / 1000.0))];
                let run = play(Device::new(RATE, frames).with(&off), SECONDS);
                let what = format!(
                    "{by} ms at {frames} frames: {:?}, {:?}, {:?}",
                    run.found, run.view, run.pacer
                );
                assert_eq!(run.view.drift_resets, 0, "{what}");
                assert_eq!((run.pacer.silence, run.pacer.dropped), (0, 0), "{what}");
                assert!(run.apart(&twin, EVENT_AT + 2.0) <= 1.0, "{what}");
                assert!(run.edited.abs() <= 3, "{what}");
                if ms / 1000.0 > tolerance(frames) {
                    assert!(
                        matches!(run.found[..], [Discontinuity::Glitch { blocks: 4, .. }]),
                        "{what}"
                    );
                } else {
                    assert!(run.found.is_empty(), "{what}");
                }
            }
        }
    }
}

#[test]
fn a_buffer_the_device_lost_is_made_up_once_at_its_size() {
    for frames in [128, 512, 1024] {
        let twin = play(Device::new(RATE, frames), SECONDS);
        let lose = [(EVENT_AT, Event::Lose(1))];
        let run = play(Device::new(RATE, frames).with(&lose), SECONDS);
        let what = format!(
            "{frames} frames: {:?}, {:?}, {:?}",
            run.found, run.view, run.pacer
        );
        // The device input: one gap, a buffer long.
        let length = frames as f64 / f64::from(RATE);
        assert!(
            matches!(run.found[..], [Discontinuity::Gap { by, .. }]
                if (by.as_secs_f64() - length).abs() <= 2.0 * JITTER),
            "{what}"
        );
        // The drift correction: one step, made up with the frames lost.
        assert_eq!(
            (run.view.drift_steps, run.view.drift_resets),
            (1, 0),
            "{what}"
        );
        assert!((run.edited - frames as i64).abs() <= 3, "{what}");
        // The pacer: nothing to do; once made up, the twin's lead again.
        assert_eq!((run.pacer.silence, run.pacer.dropped), (0, 0), "{what}");
        assert!(run.apart(&twin, EVENT_AT + 1.0) <= 1.0, "{what}");
    }
}

#[test]
fn a_board_37_ppm_fast_overloading_every_ten_minutes_stays_in_sync_for_three_hours() {
    // At 8 kHz, so that three hours simulate in a few seconds, in 10 ms
    // buffers; an overload skips one IO cycle, whose audio is lost.
    let overloads: Vec<_> = (1..18)
        .map(|k| (k as f64 * 600.0, Event::Lose(1)))
        .collect();
    let mut device = Device::new(8_000, 80).with(&overloads);
    device.ppm = 37.0;
    let run = play(device, 3 * 3_600);
    let what = format!("{:?}, {:?}", run.view, run.pacer);
    // Each overload is one gap, made up once; nothing for the pacer.
    assert_eq!(run.found.len(), 17, "{:?}", run.found);
    assert!(
        run.found
            .iter()
            .all(|d| matches!(d, Discontinuity::Gap { .. })),
        "{:?}",
        run.found
    );
    assert_eq!(
        (run.view.drift_steps, run.view.drift_resets),
        (17, 0),
        "{what}"
    );
    assert_eq!((run.pacer.silence, run.pacer.dropped), (0, 0), "{what}");
    // The drift corrected, and the lead within 20 ms of where it settled
    // throughout (block delivery and the losses before they are made up
    // included).
    let cancels = -37.0 / (1.0 + 37e-6);
    assert!((run.view.correction_ppm - cancels).abs() < 1.0, "{what}");
    let settled = &run.lead[5 * FPS as usize..65 * FPS as usize];
    let settled = settled.iter().sum::<f64>() / settled.len() as f64;
    let worst = run.lead[5 * FPS as usize..]
        .iter()
        .map(|l| (l - settled).abs())
        .fold(0.0, f64::max);
    assert!(worst <= 20.0, "{worst} ms from {settled} ms: {what}");
}

#[test]
fn callbacks_switching_between_512_and_128_frames_move_nothing() {
    // cpal takes this callback's length for the IO buffer when it could
    // not read the buffer size; the device input holds the first buffer's
    // latency, so the board's PTS, and all that follows from them, are
    // those of a device that reports its size.
    let switches = [
        (30.0, Event::Buffer(128)),
        (60.0, Event::Buffer(512)),
        (90.0, Event::Buffer(128)),
        (120.0, Event::Buffer(512)),
    ];
    let device = |reported, latency| Device {
        reported,
        latency,
        ..Device::new(RATE, 512).with(&switches)
    };
    let held = Some(CaptureLatency::default());
    let reported = play(device(Some(512), held), 150);
    untouched(&reported, "reported");
    assert!(reported.found.is_empty() && reported.edited == 0);
    let fallback = play(device(None, held), 150);
    untouched(&fallback, "fallback");
    assert!(fallback.found.is_empty(), "{:?}", fallback.found);
    assert_eq!((fallback.edited, &fallback.lead), (0, &reported.lead));
    // Control: cpal's capture time as it is. Each switch to 128 frames
    // reads as 8 ms of audio missing, made up with silence; each switch
    // back as a rewind the stamper absorbs; so the audio ends up 16 ms
    // late after two rounds.
    let unheld = play(device(None, None), 150);
    let gaps = unheld
        .found
        .iter()
        .filter(|d| {
            matches!(d, Discontinuity::Gap { by, .. } if (by.as_secs_f64() - 0.008).abs() < 1e-4)
        })
        .count();
    assert_eq!(
        (gaps, unheld.view.drift_steps),
        (2, 2),
        "{:?}",
        unheld.found
    );
    let late = unheld.lead.last().unwrap() - reported.lead.last().unwrap();
    assert!((late - 16.0).abs() < 1.0, "{late} ms");
}
