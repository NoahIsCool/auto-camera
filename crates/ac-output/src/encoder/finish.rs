//! Finishing and teardown: every way a [`ProgramEncoder`] ends reaps ffmpeg
//! and joins its threads.
//!
//! [`ProgramEncoder::finish_with_timeout`] closes both inputs (the writers
//! drain their queues and settle their debts first), polls for ffmpeg's
//! exit every `EXIT_POLL` until the deadline and kills it after that.
//! Dropping an encoder does not wait: it closes the queues and kills ffmpeg
//! at once. Either way the stop flag is raised before anything is killed,
//! so the broken pipes of the teardown are not recorded as errors; the
//! child is reaped and every thread joined before the call returns, and a
//! thread that panicked is reported in `last_error`.

use std::process::ExitStatus;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use super::{DEFAULT_FINISH_TIMEOUT, EXIT_POLL, EncoderStats, ProgramEncoder};
use crate::OutputError;

impl ProgramEncoder {
    /// Finishes with [`DEFAULT_FINISH_TIMEOUT`].
    pub fn finish(self) -> Result<EncoderStats, OutputError> {
        self.finish_with_timeout(DEFAULT_FINISH_TIMEOUT)
    }

    /// Closes the inputs, waits up to `timeout` for ffmpeg to flush both
    /// outputs and exit, then kills it. Returns the final stats on a clean
    /// exit, [`OutputError::Failed`] on a non-zero exit and
    /// [`OutputError::Timeout`] if ffmpeg had to be killed.
    ///
    /// A timeout does not by itself mean the recording is damaged: the MP4
    /// is finalised before the RTMP slave is closed, and closing an RTMP
    /// slave that is still trying to reach an unreachable host can outlast
    /// any reasonable timeout (it waits for the OS connect timeout).
    pub fn finish_with_timeout(mut self, timeout: Duration) -> Result<EncoderStats, OutputError> {
        self.video_tx = None;
        self.audio_tx = None;
        let status = self.wait_until(Instant::now() + timeout);
        // Teardown from here on: broken pipes are expected, not errors.
        self.shared.stop.store(true, Ordering::SeqCst);
        if !matches!(status, Ok(Some(_))) {
            self.kill_and_reap();
        }
        self.join_threads();
        let stats = self.stats();
        match status? {
            None => Err(OutputError::Timeout(timeout)),
            Some(status) if status.success() => Ok(stats),
            Some(status) => Err(OutputError::Failed {
                status: status.to_string(),
                last_error: stats.last_error,
            }),
        }
    }

    /// Polls for ffmpeg's exit until `deadline`; `None` if still running.
    fn wait_until(&self, deadline: Instant) -> std::io::Result<Option<ExitStatus>> {
        loop {
            if let Some(status) = self.lock_child().try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(EXIT_POLL);
        }
    }

    /// Kills ffmpeg if it is still running and reaps it.
    fn kill_and_reap(&self) {
        let mut child = self.lock_child();
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
        }
        let _ = child.wait();
    }

    /// Joins every thread, recording a panic as an error.
    fn join_threads(&mut self) {
        for t in self.threads.drain(..) {
            if t.join().is_err() {
                self.shared.record_error("an ac-output thread panicked");
            }
        }
    }
}

impl Drop for ProgramEncoder {
    /// Kills ffmpeg (if still running), reaps it and joins the threads.
    /// Idempotent after [`ProgramEncoder::finish`].
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Closing the queues lets idle writers exit; killing ffmpeg breaks
        // the pipes of busy ones and ends the readers.
        self.video_tx = None;
        self.audio_tx = None;
        self.kill_and_reap();
        self.join_threads();
    }
}
