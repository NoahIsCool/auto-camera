//! The engine thread: the master-clock loop around the
//! [`EngineCore`](crate::EngineCore).
//!
//! [`Runtime::run`] is the whole life of the thread. It polls the
//! [`TickSchedule`] and sleeps until the next tick is due, at most
//! [`WAIT_SLICE`] at a time. A tick applies the pending commands, feeds the
//! core the newest frame of every source and the discovery results that
//! arrived, hands the core's requests to the workers, pushes the program
//! frame and audio to the output, and publishes a snapshot. The frame the
//! tick read from the phase reference (the program audio source, else the
//! first source) goes to the [`PhaseLock`], which delays the schedule when
//! ticks collide with that source's frames (see [`crate::schedule`]).
//!
//! Only the phase reference is fed its newest frame as it is, though: every
//! other source's newest frame is offered to its
//! [`PresentationBuffer`](crate::presentation::PresentationBuffer) whenever
//! the thread wakes, between ticks too, and the core is fed the frame that
//! buffer presents for the tick's due time (see [`crate::presentation`]).
//!
//! Commands are applied in order at tick start, and only on this thread:
//! `StartOutput` and `StopOutput` go to the
//! [`OutputController`](crate::output::OutputController) and every other
//! command but `Shutdown` to the core, so no other thread touches either.
//! `Shutdown`, or every sender gone, ends the loop, and
//! [`Runtime::shutdown`] then stops the parts in the order the
//! [runtime docs](super) give. A thread that panics instead publishes its
//! last snapshot through [`publish_fatal`], called from the spawn closure
//! in [`Engine::start_with`](super::Engine::start_with).

use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use ac_output::AudioFormat;

use super::{EngineReport, Runtime};
use crate::command::{CommandReply, EngineCommand};
use crate::core::SourceFeed;
use crate::discovery::{DiscoveryResult, DiscoveryWorker};
use crate::mailbox::Latest;
use crate::schedule::{PhaseLock, TickDecision, TickSchedule};
use crate::snapshot::{EngineSnapshot, OutputState, TimingView};

/// Longest single wait of the engine thread between ticks.
const WAIT_SLICE: Duration = Duration::from_millis(10);

/// The text of a panic payload.
pub(super) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Publishes the last snapshot of an engine thread that panicked: the newest
/// one, marked stopped, with the reason and the output no longer running.
pub(super) fn publish_fatal(snapshots: &Latest<Arc<EngineSnapshot>>, message: &str) {
    let Some(last) = snapshots.get() else {
        return;
    };
    let mut snap = (*last).clone();
    snap.running = false;
    snap.fatal = Some(format!("the engine thread panicked: {message}"));
    if matches!(
        snap.output.state,
        OutputState::Starting | OutputState::Running | OutputState::Stopping
    ) {
        snap.output.state = OutputState::Failed("the engine stopped".into());
    }
    snapshots.publish(Arc::new(snap));
}

impl Runtime {
    /// The engine thread's main loop.
    pub(super) fn run(mut self) -> EngineReport {
        let mut schedule = TickSchedule::new(self.clock.now(), self.period, self.max_lag);
        let mut phase = PhaseLock::new(self.period);
        loop {
            let (media_time, due, late, skipped) = match schedule.poll(self.clock.now()) {
                TickDecision::Wait(d) => {
                    self.collect_frames();
                    self.clock.sleep(d.min(WAIT_SLICE));
                    continue;
                }
                TickDecision::Tick {
                    media_time,
                    due,
                    late,
                    skipped,
                    ..
                } => (media_time, due, late, skipped),
            };
            let started = Instant::now();
            if self.apply_commands() {
                break;
            }
            let reference = self.tick(media_time, due, skipped);
            if let Some(by) = phase.observe(reference) {
                schedule.delay(by);
            }
            self.timer.record(started.elapsed());
            self.late += u64::from(late);
            self.skipped += skipped;
            self.publish(true);
        }
        self.shutdown()
    }

    /// Applies pending commands; true when the engine must stop.
    fn apply_commands(&mut self) -> bool {
        loop {
            let envelope = match self.commands.try_recv() {
                Ok(e) => e,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
            };
            let stop = envelope.command == EngineCommand::Shutdown;
            let result = match &envelope.command {
                EngineCommand::Shutdown => Ok(CommandReply::Done),
                EngineCommand::StartOutput {
                    rtmp_url,
                    record_path,
                } => {
                    let audio = self.program_audio_format();
                    self.output
                        .start(rtmp_url.clone(), record_path.clone(), audio)
                }
                EngineCommand::StopOutput => self.output.stop(),
                other => self.core.apply(other),
            };
            if let Some(reply) = envelope.reply {
                let _ = reply.send(result);
            }
            if stop {
                return true;
            }
        }
    }

    /// The format of program audio for an output starting now: the audio
    /// source's, as its newest connection describes it, so a source that
    /// was missing when the engine started gives the program its audio
    /// once it has been on air. `None` without an audio source, or while
    /// it has given no description with audio.
    fn program_audio_format(&self) -> Option<AudioFormat> {
        let source = self.sources.get(self.audio_source?)?;
        let (sample_rate, channels) = source.current_info().audio?;
        Some(AudioFormat {
            sample_rate,
            channels,
        })
    }

    /// Offers every source's newest frame to its presentation buffer, if
    /// it has one, stamped with the clock reading now.
    fn collect_frames(&mut self) {
        let now = self.clock.now();
        for (s, buffer) in self.sources.iter().zip(&mut self.presenters) {
            if let Some(buffer) = buffer
                && let Some(frame) = s.latest()
            {
                buffer.offer(frame, now);
            }
        }
    }

    /// One tick, due at clock reading `due`: feed the core, dispatch
    /// discovery, push the program (owing the encoder a repeat for each of
    /// the `skipped` ticks before). Returns the sequence number and PTS of
    /// the frame the tick read from the phase reference (the program audio
    /// source, else the first source), for the [`PhaseLock`].
    fn tick(
        &mut self,
        media_time: Duration,
        due: Duration,
        skipped: u64,
    ) -> Option<(u64, Duration)> {
        self.output.poll();
        self.collect_frames();
        let results: Vec<DiscoveryResult> = self.results.try_iter().collect();
        let feeds: Vec<SourceFeed> = self
            .sources
            .iter()
            .zip(&mut self.workers)
            .zip(&mut self.presenters)
            .map(|((s, w), p)| SourceFeed {
                state: s.state(),
                frame: match p {
                    Some(buffer) => buffer.present(due).into_frame(),
                    None => s.latest(),
                },
                detector_loading: w.as_mut().is_some_and(DiscoveryWorker::is_loading),
                error: s.error(),
                // After `frame` (fields are evaluated in order), so it
                // describes that frame's connection or a newer one.
                info: Some(s.current_info()),
            })
            .collect();
        let reference = feeds
            .get(self.audio_source.unwrap_or(0))
            .and_then(|feed| feed.frame.as_ref())
            .map(|frame| (frame.seq, frame.pts));
        let out = self.core.tick(media_time, &feeds, results);
        for (i, request) in out.requests {
            if let Some(Some(worker)) = self.workers.get(i) {
                worker.submit(request);
            }
        }
        let mut program_audio = Vec::new();
        for (i, s) in self.sources.iter().enumerate() {
            let blocks = s.drain_audio();
            if Some(i) == self.audio_source {
                program_audio = blocks;
            }
        }
        self.output.push(&out.program, program_audio, skipped);
        reference
    }

    /// Publishes the current snapshot and returns it.
    fn publish(&self, running: bool) -> Arc<EngineSnapshot> {
        let timing = TimingView {
            tick: self.timer,
            budget_ms: self.period.as_secs_f64() * 1e3,
            late_ticks: self.late,
            skipped_ticks: self.skipped,
        };
        let snapshot = Arc::new(self.core.snapshot(self.output.view(), timing, running));
        self.snapshots.publish(snapshot.clone());
        snapshot
    }

    /// Stops the sidecar, the workers, the output and the sources, and
    /// publishes the final snapshot. The sidecar goes first: dropping its
    /// service interrupts a pass or a load in progress, so the workers can
    /// be joined at once instead of after a request deadline.
    fn shutdown(mut self) -> EngineReport {
        let mut joined = 0;
        if let Some(service) = self.sidecar.take() {
            // Ends the process (stdin closed, 2 s grace, kill, reap) and
            // joins the service thread.
            drop(service);
            joined += 1;
        }
        for w in self.workers.iter_mut().flatten() {
            joined += usize::from(w.stop());
        }
        joined += self.output.shutdown();
        for s in &mut self.sources {
            s.stop();
        }
        EngineReport {
            ticks: self.core.ticks(),
            threads_joined: joined,
            final_snapshot: self.publish(false),
        }
    }
}
