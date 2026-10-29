//! The program output session, driven from the engine thread.
//!
//! [`OutputController`] owns at most one [`ProgramEncoder`]. Pushing frames
//! and audio never blocks (the encoder queues are bounded and drop), but
//! starting an encoder (it probes `ffmpeg -encoders`) and finishing one (it
//! waits for ffmpeg to flush, up to a timeout) can take seconds, which would
//! stall the master clock. Both therefore run on short-lived helper threads
//! that hand the result back over a channel; [`OutputController::poll`]
//! collects it on the next tick. The state machine is:
//!
//! ```text
//! Idle/Finished/Failed --start--> Starting --ok--> Running --stop--> Stopping --> Finished
//!                                    |  \--error--> Failed     \--ffmpeg died--> Failed
//!                                    \--stop--> (stops as soon as it has started)
//! ```
//!
//! Every helper thread is joined by [`OutputController::shutdown`], which
//! also finishes a running encoder synchronously, so no ffmpeg process or
//! thread outlives the engine.
//!
//! Each start opens a new *session*, numbered, and every helper result
//! carries the number of the session it belongs to. A result for an older
//! session (the finish of an ffmpeg that died, arriving after the operator
//! already started again) only updates the "last session" statistics; it
//! never changes the state of the current session, so a stale `Finished`
//! cannot make a starting or running session look finished.
//!
//! The encoder gets exactly one frame per master-clock tick. Ticks the
//! engine skipped after falling far behind are owed to it as repeated
//! frames ([`ProgramEncoder::repeat_frames`]) and counted in the frame
//! count, so the encoded timeline stays on the master clock. Program audio,
//! when configured, is paced against that frame count by an [`AudioPacer`].
//!
//! While a session is live (from its start until it is finished or failed)
//! the controller also holds the machine awake (macOS `caffeinate`), when
//! the engine asked it to ([`OutputController::with_keep_awake`]); the hold
//! is released after the encoder has finished, and it never affects the
//! output.
//!
//! The controller's methods are split by their part in a session: the state
//! machine above (start, stop, poll) here, pushing the program and reading
//! the view in `tick`, finishing encoders (in the background, at shutdown
//! and on drop) in `finish`, and the keep-awake hold in `awake`.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_output::{AudioFormat, AwakeMode, Caffeinate, EncoderStats, OutputError, ProgramEncoder};

use crate::audio::AudioPacer;
use crate::command::{CommandError, CommandReply};
use crate::config::OutputSettings;
use crate::snapshot::OutputState;

mod awake;
mod finish;
mod tick;

use awake::SessionAwake;

/// Audio may lag video by this much before silence is inserted.
pub(crate) const AUDIO_SLACK: Duration = Duration::from_millis(100);
/// Audio may lead video by this much before the oldest blocks are dropped.
pub(crate) const AUDIO_MAX_LEAD: Duration = Duration::from_millis(100);

/// A result handed back by a helper thread.
enum Event {
    /// `ProgramEncoder::start` returned.
    Started {
        /// The session it was started for.
        session: u64,
        /// The start result.
        result: Result<ProgramEncoder, OutputError>,
    },
    /// `ProgramEncoder::finish` returned; an earlier failure of the session
    /// is kept.
    Finished {
        /// The session it belonged to.
        session: u64,
        /// The finish result.
        result: Result<EncoderStats, OutputError>,
        /// Stats read just before finishing (boxed: the enum stays small).
        before: Box<EncoderStats>,
    },
}

/// The program output session; see the module docs.
pub struct OutputController {
    /// Encoder and default destinations.
    settings: OutputSettings,
    /// Lifecycle of the current session.
    state: OutputState,
    /// Number of the current session (0 before the first start).
    session: u64,
    /// The running encoder.
    encoder: Option<ProgramEncoder>,
    /// Program audio pacing for the running session.
    pacer: Option<AudioPacer>,
    /// Program loudness and peaks of the session, fed what the pacer
    /// pushes (kept after the session ends, so its readings stay final).
    meter: Option<ac_audio::LoudnessMeter>,
    /// Frames handed to the encoder this session.
    frames_pushed: u64,
    /// Destinations of the current session.
    rtmp_url: Option<String>,
    /// Recording path of the current session.
    record_path: Option<PathBuf>,
    /// Stats of the newest finished (or failed) session.
    last_stats: Option<EncoderStats>,
    /// Stop as soon as the starting encoder is up.
    stop_after_start: bool,
    /// Helper results.
    events: Receiver<Event>,
    /// Sender cloned into helpers.
    sender: Sender<Event>,
    /// Helper threads to join.
    helpers: Vec<JoinHandle<()>>,
    /// The keep-awake hold of the live session.
    awake: SessionAwake,
}

impl std::fmt::Debug for OutputController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputController")
            .field("state", &self.state)
            .field("frames_pushed", &self.frames_pushed)
            .finish_non_exhaustive()
    }
}

impl OutputController {
    /// An idle controller that leaves keeping the machine awake to its
    /// caller (see [`OutputController::with_keep_awake`]).
    pub fn new(settings: OutputSettings) -> Self {
        let (sender, events) = channel();
        Self {
            settings,
            state: OutputState::Idle,
            session: 0,
            encoder: None,
            pacer: None,
            meter: None,
            frames_pushed: 0,
            rtmp_url: None,
            record_path: None,
            last_stats: None,
            stop_after_start: false,
            events,
            sender,
            helpers: Vec::new(),
            awake: SessionAwake::new(Caffeinate::Off, AwakeMode::System),
        }
    }

    /// The lifecycle state.
    pub fn state(&self) -> &OutputState {
        &self.state
    }

    /// Starts a session to the given destinations (defaulting to the
    /// config's), with program audio in `audio` format if any.
    pub fn start(
        &mut self,
        rtmp_url: Option<String>,
        record_path: Option<PathBuf>,
        audio: Option<AudioFormat>,
    ) -> Result<CommandReply, CommandError> {
        match self.state {
            OutputState::Starting => return Err(CommandError::OutputBusy("starting")),
            OutputState::Running => return Err(CommandError::OutputBusy("running")),
            OutputState::Stopping => return Err(CommandError::OutputBusy("stopping")),
            _ => {}
        }
        let rtmp_url = rtmp_url
            .or_else(|| self.settings.rtmp_url.clone())
            .filter(|u| !u.trim().is_empty());
        let record_path = record_path
            .or_else(|| self.settings.record_path.clone())
            .filter(|p| !p.as_os_str().is_empty());
        if rtmp_url.is_none() && record_path.is_none() {
            return Err(CommandError::Output(
                "no destination: give an RTMP URL or a recording path".into(),
            ));
        }
        if let Some(p) = &record_path
            && p.exists()
        {
            return Err(CommandError::Output(format!(
                "{} already exists; recordings are never overwritten",
                p.display()
            )));
        }
        let config = self
            .settings
            .to_output_config(rtmp_url.clone(), record_path.clone(), audio);
        config
            .validate()
            .map_err(|e| CommandError::Output(e.to_string()))?;
        self.pacer = audio.map(|a| {
            AudioPacer::new(
                a.sample_rate,
                a.channels,
                self.settings.fps,
                AUDIO_SLACK,
                AUDIO_MAX_LEAD,
            )
        });
        // The format passed `validate` (8 to 192 kHz, 1 to 8 channels),
        // which is within what the meter takes.
        self.meter =
            audio.and_then(|a| ac_audio::LoudnessMeter::new(a.sample_rate, a.channels).ok());
        let tx = self.sender.clone();
        let session = self.session + 1;
        let helper = std::thread::Builder::new()
            .name("ac-output-start".into())
            .spawn(move || {
                let result = ProgramEncoder::start(config);
                let _ = tx.send(Event::Started { session, result });
            })
            .map_err(|e| CommandError::Output(e.to_string()))?;
        self.helpers.push(helper);
        self.session = session;
        self.state = OutputState::Starting;
        self.rtmp_url = rtmp_url;
        self.record_path = record_path;
        self.frames_pushed = 0;
        self.stop_after_start = false;
        self.awake.hold();
        Ok(CommandReply::OutputStarting)
    }

    /// Finishes the session.
    pub fn stop(&mut self) -> Result<CommandReply, CommandError> {
        match self.state {
            OutputState::Running => {
                self.finish_in_background();
                self.state = OutputState::Stopping;
                Ok(CommandReply::OutputStopping)
            }
            OutputState::Starting => {
                self.stop_after_start = true;
                self.state = OutputState::Stopping;
                Ok(CommandReply::OutputStopping)
            }
            OutputState::Stopping => Err(CommandError::OutputBusy("stopping")),
            _ => Err(CommandError::OutputIdle),
        }
    }

    /// Collects helper results, notices a dead ffmpeg and releases the
    /// keep-awake hold of a session that is over.
    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Started { session, result } if session != self.session => {
                    // Cannot happen (a start is refused until the previous
                    // one resolved), but an encoder nobody owns is never
                    // adopted: dropping it kills and reaps its ffmpeg.
                    drop(result);
                }
                Event::Started {
                    result: Ok(encoder),
                    ..
                } => {
                    self.encoder = Some(encoder);
                    if self.stop_after_start {
                        self.finish_in_background();
                        self.state = OutputState::Stopping;
                    } else {
                        self.state = OutputState::Running;
                    }
                }
                Event::Started { result: Err(e), .. } => {
                    self.state = OutputState::Failed(e.to_string());
                }
                Event::Finished {
                    session,
                    result,
                    before,
                } => {
                    let current = session == self.session;
                    let failed = matches!(self.state, OutputState::Failed(_));
                    let (stats, error) = match result {
                        Ok(stats) => (stats, None),
                        Err(e) => (*before, Some(e.to_string())),
                    };
                    self.last_stats = Some(stats);
                    if current && !failed {
                        self.state = match error {
                            None => OutputState::Finished,
                            Some(e) => OutputState::Failed(e),
                        };
                    }
                }
            }
        }
        if self.state == OutputState::Running
            && let Some(enc) = &self.encoder
        {
            let stats = enc.stats();
            if !stats.ffmpeg_alive {
                let why = stats
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "ffmpeg exited".into());
                self.state = OutputState::Failed(why);
                self.finish_in_background();
            }
        }
        self.helpers.retain(|h| !h.is_finished());
        self.awake.check(self.live());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings with an ffmpeg that does not exist.
    fn settings() -> OutputSettings {
        OutputSettings {
            width: 64,
            height: 36,
            ffmpeg: "/nonexistent/ffmpeg-for-ac-engine-tests".into(),
            ..OutputSettings::default()
        }
    }

    /// Polls `c` until `done` holds for its state, for at most 20 s.
    fn wait_for(c: &mut OutputController, done: impl Fn(&OutputState) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !done(c.state()) && std::time::Instant::now() < deadline {
            c.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn start_needs_a_destination() {
        let mut c = OutputController::new(settings());
        assert!(matches!(
            c.start(None, None, None),
            Err(CommandError::Output(_))
        ));
        assert!(matches!(
            c.start(Some("  ".into()), None, None),
            Err(CommandError::Output(_))
        ));
        assert_eq!(c.state(), &OutputState::Idle);
        assert_eq!(c.stop(), Err(CommandError::OutputIdle));
    }

    #[test]
    fn existing_recordings_are_never_overwritten() {
        let mut c = OutputController::new(settings());
        let existing = std::env::current_exe().unwrap();
        assert!(matches!(
            c.start(None, Some(existing), None),
            Err(CommandError::Output(m)) if m.contains("never overwritten")
        ));
    }

    #[test]
    fn invalid_settings_are_reported() {
        let mut s = settings();
        s.width = 63;
        let mut c = OutputController::new(s);
        let path = std::env::temp_dir().join("ac-engine-never-written.mp4");
        assert!(matches!(
            c.start(None, Some(path), None),
            Err(CommandError::Output(_))
        ));
    }

    #[test]
    fn a_missing_ffmpeg_fails_the_session_without_blocking() {
        let mut c = OutputController::new(settings());
        let path = std::env::temp_dir().join(format!("ac-engine-out-{}.mp4", std::process::id()));
        assert_eq!(
            c.start(None, Some(path.clone()), None),
            Ok(CommandReply::OutputStarting)
        );
        assert_eq!(
            c.start(None, Some(path.clone()), None),
            Err(CommandError::OutputBusy("starting"))
        );
        wait_for(&mut c, |s| matches!(s, OutputState::Failed(_)));
        assert!(matches!(c.state(), OutputState::Failed(_)));
        let view = c.view();
        assert_eq!(view.record_path.as_deref(), Some(path.as_path()));
        assert!(!path.exists());
        assert!(c.shutdown() <= 1);
    }

    /// Final stats of a session that wrote `frames` frames.
    pub(super) fn stats(frames: u64) -> EncoderStats {
        EncoderStats {
            encoder: ac_output::EncoderChoice::Libx264,
            frames_in: frames,
            frames_dropped: 0,
            frames_repeated: 0,
            frames_written: frames,
            audio_samples_in: 0,
            audio_samples_dropped: 0,
            audio_silence_samples: 0,
            audio_samples_written: 0,
            audio_connected: false,
            ffmpeg_alive: false,
            exit_code: Some(0),
            last_error: None,
            progress: Default::default(),
            stream: None,
        }
    }

    #[test]
    fn a_late_finish_of_an_old_session_leaves_the_new_one_alone() {
        let mut c = OutputController::new(settings());
        // Session 1 died and its finish is still pending while the operator
        // already started session 2.
        c.session = 2;
        c.state = OutputState::Starting;
        let send = |c: &OutputController, session, result| {
            c.sender
                .send(Event::Finished {
                    session,
                    result,
                    before: Box::new(stats(7)),
                })
                .unwrap();
        };
        send(&c, 1, Ok(stats(42)));
        c.poll();
        assert_eq!(c.state(), &OutputState::Starting);
        assert_eq!(c.view().stats.map(|s| s.frames_written), Some(42));
        // The new session can still be cancelled.
        assert_eq!(c.stop(), Ok(CommandReply::OutputStopping));
        send(&c, 1, Err(OutputError::Stopped));
        c.poll();
        assert_eq!(c.state(), &OutputState::Stopping);
        // Its own finish does count.
        send(&c, 2, Ok(stats(9)));
        c.poll();
        assert_eq!(c.state(), &OutputState::Finished);
        assert_eq!(c.view().stats.map(|s| s.frames_written), Some(9));
    }

    #[test]
    fn stopping_while_starting_is_accepted() {
        let mut c = OutputController::new(settings());
        let path = std::env::temp_dir().join(format!("ac-engine-out2-{}.mp4", std::process::id()));
        c.start(
            Some("rtmp://example.invalid/live/secret".into()),
            Some(path),
            None,
        )
        .unwrap();
        assert_eq!(c.stop(), Ok(CommandReply::OutputStopping));
        assert_eq!(c.stop(), Err(CommandError::OutputBusy("stopping")));
        let view = c.view();
        assert!(!view.rtmp_url.unwrap().contains("secret"));
        wait_for(&mut c, |s| matches!(s, OutputState::Failed(_)));
        c.shutdown();
    }
}
