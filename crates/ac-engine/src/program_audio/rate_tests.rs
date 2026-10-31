//! The program input at another rate than the output was started with: a
//! board offline when the output starts (so the session takes the 48 kHz
//! stand-in) and plugged in at 96 kHz, and a board whose rate changed
//! under the stream, so that it comes back at 44.1 kHz. Its blocks reach
//! the output's pacer, which drops them as another format than the
//! encoder's; the board's view says why it is not heard, and the alarm is
//! raised with the same words, in a session that only streams. The same
//! boards at the session's rate are heard and raise nothing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ac_media::AudioInputInfo;
use ac_sources::SourceConfig;

use super::*;
use crate::alarms::{AlarmCause, AlarmChange, Alarms};
use crate::audio::AudioPacer;
use crate::config::StudioConfig;
use crate::core::EngineCore;
use crate::output::{AUDIO_MAX_LEAD, AUDIO_SLACK};
use crate::snapshot::{EngineSnapshot, OutputState, OutputView, TimingView};

/// The board's connection, as the test plugs it.
#[derive(Debug, Default)]
struct Wire {
    /// The rate it runs at while connected.
    rate: Option<u32>,
    /// The rate of its newest connection, once it had one.
    newest: Option<u32>,
    /// Why it is not connected, while it is not.
    why: String,
}

/// A stereo board whose connection the test plugs and unplugs; each drain
/// delivers a 30 fps tick's worth of its audio while it is connected.
struct Board {
    /// What it was configured as.
    info: AudioInputInfo,
    /// Its connection.
    wire: Arc<Mutex<Wire>>,
    /// Blocks delivered so far.
    seq: Mutex<u64>,
}

impl AudioInput for Board {
    fn info(&self) -> &AudioInputInfo {
        &self.info
    }

    fn current_info(&self) -> AudioInputInfo {
        AudioInputInfo {
            id: self.info.id.clone(),
            format: self.wire.lock().unwrap().newest.map(|rate| (rate, 2)),
        }
    }

    fn state(&self) -> SourceState {
        match self.wire.lock().unwrap().rate {
            Some(_) => SourceState::Running,
            None => SourceState::Offline,
        }
    }

    fn drain(&self) -> Vec<AudioBlock> {
        let Some(rate) = self.wire.lock().unwrap().rate else {
            return Vec::new();
        };
        let mut seq = self.seq.lock().unwrap();
        *seq += 1;
        let frames = (rate / 30) as usize;
        vec![AudioBlock {
            source: self.info.id.clone(),
            seq: *seq,
            pts: Duration::from_secs_f64(*seq as f64 / 30.0),
            sample_rate: rate,
            channels: 2,
            samples: vec![0.25; frames * 2].into(),
        }]
    }

    fn wait(&self, _timeout: Duration) -> bool {
        false
    }

    fn dropped(&self) -> u64 {
        0
    }

    fn error(&self) -> Option<String> {
        let wire = self.wire.lock().unwrap();
        wire.rate.is_none().then(|| wire.why.clone())
    }

    fn stop(&mut self) {}
}

/// The studio of one camera and the board as program audio, with an
/// output session the test starts and stops, observed tick by tick as the
/// engine thread would publish it.
struct Show {
    /// Program audio: the board.
    audio: ProgramAudio,
    /// The board's connection.
    wire: Arc<Mutex<Wire>>,
    /// The session's pacer and format, while one is live.
    session: Option<(AudioPacer, AudioFormat)>,
    /// Frames pushed in the session.
    frames: u64,
    /// The snapshot published next.
    snap: EngineSnapshot,
    /// The window's alarm model.
    alarms: Alarms,
}

impl Show {
    /// The show with the board not connected (`why`), nothing on air.
    fn new(why: &str) -> Self {
        let wire = Arc::new(Mutex::new(Wire {
            why: why.into(),
            ..Wire::default()
        }));
        let board = Board {
            info: AudioInputInfo {
                id: SourceId("board".into()),
                format: None,
            },
            wire: wire.clone(),
            seq: Mutex::new(0),
        };
        let slot = InputSlot::new("Soundboard", None, Box::new(board));
        let config = StudioConfig::with_source(
            "stage",
            SourceConfig::Synthetic {
                width: 64,
                height: 36,
                fps: 30.0,
                subjects: 0,
            },
        );
        let info = ac_media::SourceInfo {
            id: SourceId("stage".into()),
            width: 64,
            height: 36,
            fps: 30.0,
            audio: None,
        };
        let core = EngineCore::new(&config, vec![info], &[false]).unwrap();
        Self {
            audio: ProgramAudio::with_inputs(Some(Origin::Input(0)), vec![slot]),
            wire,
            session: None,
            frames: 0,
            snap: core.snapshot(OutputView::default(), TimingView::default(), true),
            alarms: Alarms::new(),
        }
    }

    /// Plugs the board in at `rate`, or unplugs it (`None`).
    fn plug(&mut self, rate: Option<u32>) {
        let mut wire = self.wire.lock().unwrap();
        wire.rate = rate;
        wire.newest = rate.or(wire.newest);
    }

    /// Starts a session that only streams, in the format program audio
    /// has now, as `StartOutput` does; returns its rate.
    fn start(&mut self) -> u32 {
        let format = self.audio.format(&[]).unwrap();
        let pacer = AudioPacer::new(format.sample_rate, 2, 30, AUDIO_SLACK, AUDIO_MAX_LEAD);
        self.session = Some((pacer, format));
        self.frames = 0;
        format.sample_rate
    }

    /// Ends the session.
    fn stop(&mut self) {
        self.session = None;
    }

    /// Runs `n` ticks; returns what the alarms did.
    fn ticks(&mut self, n: u64) -> Vec<AlarmChange> {
        let mut changes = Vec::new();
        for _ in 0..n {
            let blocks = self.audio.take(&[]);
            let mut output = OutputView::default();
            if let Some((pacer, format)) = &mut self.session {
                self.frames += 1;
                pacer.plan(self.frames, blocks, Duration::ZERO);
                output = OutputView {
                    state: OutputState::Running,
                    rtmp_url: Some("rtmp://example.com/app/<redacted>".into()),
                    audio: Some(pacer.stats()),
                    audio_format: Some(*format),
                    ..OutputView::default()
                };
            }
            let snap = &mut self.snap;
            snap.tick += 1;
            snap.media_time = Duration::from_secs_f64(snap.tick as f64 / 30.0);
            snap.audio_inputs = self.audio.views(output.audio_format);
            snap.output = output;
            changes.extend(self.alarms.observe(snap));
        }
        changes
    }

    /// The board's view in the newest snapshot.
    fn board(&self) -> &AudioInputView {
        &self.snap.audio_inputs[0]
    }

    /// The program audio pushed in the session, as the newest snapshot has
    /// it.
    fn pushed(&self) -> u64 {
        self.snap.output.audio.map_or(0, |a| a.pushed)
    }
}

#[test]
fn a_board_plugged_in_at_another_rate_than_the_stand_in_says_it_is_not_heard() {
    let mut show = Show::new("no audio device with UID \"SQ-1\" is connected");
    // The output starts before the board is there: the stand-in rate.
    assert_eq!(show.start(), STAND_IN_RATE);
    assert_eq!(show.ticks(30), [], "an offline board raises no alarm here");
    assert!(!show.board().unheard);
    assert_eq!(show.board().state, SourceState::Offline);
    // Plugged in at 96 kHz: on the first tick it delivers, the alarm.
    show.plug(Some(96_000));
    let text = "Soundboard runs at 96000 Hz; the output was started at 48000 Hz: not heard \
                until the output is restarted";
    let raised = show.ticks(1);
    assert!(
        matches!(&raised[..], [AlarmChange::Raised(a)]
            if a.cause == AlarmCause::ProgramAudioUnheard && a.message == text),
        "{raised:?}"
    );
    let board = show.board();
    assert_eq!(board.state, SourceState::Running);
    assert!(board.unheard);
    assert_eq!(board.error.as_deref(), Some(text), "the view says the same");
    // It stays raised, and nothing of the board reaches the encoder.
    assert_eq!(show.ticks(90), []);
    assert_eq!(show.pushed(), 0);
    assert_eq!(show.snap.output.audio.unwrap().mismatched_blocks, 91);
    assert!(show.alarms.get(&AlarmCause::ProgramAudioUnheard).is_some());
    // Restarted, the output takes the board's rate: heard, and quiet.
    show.stop();
    let cleared = show.ticks(1);
    assert!(
        matches!(&cleared[..], [AlarmChange::Cleared { .. }]),
        "{cleared:?}"
    );
    assert_eq!(show.board().error, None);
    assert_eq!(show.start(), 96_000);
    assert_eq!(show.ticks(60), []);
    assert!(!show.board().unheard);
    assert!(show.pushed() > 0);
    // The same show with the board at the stand-in rate is heard at once.
    let mut heard = Show::new("no audio device with UID \"SQ-1\" is connected");
    heard.start();
    heard.ticks(30);
    heard.plug(Some(48_000));
    assert_eq!(heard.ticks(60), []);
    assert!(heard.pushed() > 0 && !heard.board().unheard);
}

#[test]
fn a_board_that_comes_back_at_another_rate_says_it_is_not_heard() {
    let mut show = Show::new("not connected yet");
    show.plug(Some(48_000));
    assert_eq!(show.start(), 48_000);
    assert_eq!(show.ticks(60), []);
    let heard = show.pushed();
    assert!(heard > 0);
    // Its rate changed under the stream: the connection ends, and the
    // supervisor opens it again at the new rate.
    let gone = "audio device \"SQ\" went away: Device sample rate changed";
    show.wire.lock().unwrap().why = gone.into();
    show.plug(None);
    assert_eq!(show.ticks(10), []);
    assert_eq!(
        show.board().error.as_deref(),
        Some(gone),
        "offline says why"
    );
    assert!(!show.board().unheard);
    show.plug(Some(44_100));
    let raised = show.ticks(1);
    let text = "Soundboard runs at 44100 Hz; the output was started at 48000 Hz: not heard \
                until the output is restarted";
    assert!(
        matches!(&raised[..], [AlarmChange::Raised(a)] if a.message == text),
        "{raised:?}"
    );
    assert_eq!(show.board().error.as_deref(), Some(text));
    assert_eq!(show.ticks(60), []);
    assert_eq!(show.pushed(), heard, "nothing more reaches the encoder");
    // Unplugged again, the board delivers nothing to drop: the alarm
    // clears after a second, and the view says it is offline.
    show.plug(None);
    let cleared = show.ticks(31);
    assert!(
        matches!(&cleared[..], [AlarmChange::Cleared { .. }]),
        "{cleared:?}"
    );
    assert_eq!(show.board().error.as_deref(), Some(gone));
}
