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
//! thread outlives the engine. Program audio, when configured, is paced
//! against the frame count by an [`AudioPacer`].

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_media::{AudioBlock, VideoFrame};
use ac_output::{AudioFormat, EncoderStats, OutputError, ProgramEncoder};

use crate::audio::{AudioPacer, PacedAudio};
use crate::command::{CommandError, CommandReply};
use crate::config::OutputSettings;
use crate::snapshot::{OutputState, OutputView};

/// Audio may lag video by this much before silence is inserted.
const AUDIO_SLACK: Duration = Duration::from_millis(100);
/// Audio may lead video by this much before blocks are dropped.
const AUDIO_MAX_LEAD: Duration = Duration::from_millis(500);

/// A result handed back by a helper thread.
enum Event {
    /// `ProgramEncoder::start` returned.
    Started(Result<ProgramEncoder, OutputError>),
    /// `ProgramEncoder::finish` returned; `failed` keeps an earlier failure.
    Finished {
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
    /// Lifecycle.
    state: OutputState,
    /// The running encoder.
    encoder: Option<ProgramEncoder>,
    /// Program audio pacing for the running session.
    pacer: Option<AudioPacer>,
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
    /// An idle controller.
    pub fn new(settings: OutputSettings) -> Self {
        let (sender, events) = channel();
        Self {
            settings,
            state: OutputState::Idle,
            encoder: None,
            pacer: None,
            frames_pushed: 0,
            rtmp_url: None,
            record_path: None,
            last_stats: None,
            stop_after_start: false,
            events,
            sender,
            helpers: Vec::new(),
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
        let tx = self.sender.clone();
        let helper = std::thread::Builder::new()
            .name("ac-output-start".into())
            .spawn(move || {
                let _ = tx.send(Event::Started(ProgramEncoder::start(config)));
            })
            .map_err(|e| CommandError::Output(e.to_string()))?;
        self.helpers.push(helper);
        self.state = OutputState::Starting;
        self.rtmp_url = rtmp_url;
        self.record_path = record_path;
        self.frames_pushed = 0;
        self.stop_after_start = false;
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

    /// Moves the encoder to a helper thread that finishes it.
    fn finish_in_background(&mut self) {
        let Some(encoder) = self.encoder.take() else {
            return;
        };
        let tx = self.sender.clone();
        let spawned = std::thread::Builder::new()
            .name("ac-output-finish".into())
            .spawn(move || {
                let before = Box::new(encoder.stats());
                let result = encoder.finish();
                let _ = tx.send(Event::Finished { result, before });
            });
        match spawned {
            Ok(h) => self.helpers.push(h),
            // Could not spawn: the encoder was moved into the failed closure
            // and dropped with it, which kills and reaps ffmpeg.
            Err(e) => self.state = OutputState::Failed(format!("cannot finish output: {e}")),
        }
    }

    /// Collects helper results and notices a dead ffmpeg.
    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Started(Ok(encoder)) => {
                    self.encoder = Some(encoder);
                    if self.stop_after_start {
                        self.finish_in_background();
                        self.state = OutputState::Stopping;
                    } else {
                        self.state = OutputState::Running;
                    }
                }
                Event::Started(Err(e)) => self.state = OutputState::Failed(e.to_string()),
                Event::Finished { result, before } => {
                    let failed = matches!(self.state, OutputState::Failed(_));
                    match result {
                        Ok(stats) => {
                            self.last_stats = Some(stats);
                            if !failed {
                                self.state = OutputState::Finished;
                            }
                        }
                        Err(e) => {
                            self.last_stats = Some(*before);
                            if !failed {
                                self.state = OutputState::Failed(e.to_string());
                            }
                        }
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
    }

    /// Pushes one program frame and the program audio drained this tick.
    pub fn push(&mut self, frame: &VideoFrame, audio: Vec<AudioBlock>) {
        let Some(encoder) = &self.encoder else {
            return;
        };
        if encoder.push_frame(frame).is_ok() {
            self.frames_pushed += 1;
        }
        let Some(pacer) = &mut self.pacer else {
            return;
        };
        for item in pacer.plan(self.frames_pushed, audio) {
            let _ = match item {
                PacedAudio::Block(block) => encoder.push_audio_block(&block),
                PacedAudio::Silence(frames) => {
                    let channels = encoder.config().audio.map_or(2, |a| a.channels);
                    let n = usize::try_from(frames).unwrap_or(0) * usize::from(channels);
                    encoder.push_audio(&vec![0.0; n])
                }
            };
        }
    }

    /// The output as published in snapshots.
    pub fn view(&self) -> OutputView {
        OutputView {
            state: self.state.clone(),
            rtmp_url: self.rtmp_url.as_deref().map(ac_output::config::redact_url),
            record_path: self.record_path.clone(),
            stats: self
                .encoder
                .as_ref()
                .map(ProgramEncoder::stats)
                .or_else(|| self.last_stats.clone()),
            audio: self.pacer.as_ref().map(AudioPacer::stats),
            frames_pushed: self.frames_pushed,
        }
    }

    /// Finishes a running encoder synchronously and joins every helper.
    /// Returns the number of helper threads joined.
    pub fn shutdown(&mut self) -> usize {
        if let Some(encoder) = self.encoder.take() {
            let before = Box::new(encoder.stats());
            let result = encoder.finish();
            let _ = self.sender.send(Event::Finished { result, before });
            if self.state == OutputState::Running {
                self.state = OutputState::Stopping;
            }
        }
        let mut joined = 0;
        for h in self.helpers.drain(..) {
            let _ = h.join();
            joined += 1;
        }
        // A start that completed during shutdown leaves an encoder to finish.
        self.poll();
        if let Some(encoder) = self.encoder.take() {
            let before = Box::new(encoder.stats());
            let result = encoder.finish();
            let _ = self.sender.send(Event::Finished { result, before });
            self.poll();
        }
        for h in self.helpers.drain(..) {
            let _ = h.join();
            joined += 1;
        }
        joined
    }
}

impl Drop for OutputController {
    fn drop(&mut self) {
        self.shutdown();
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
