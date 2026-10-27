//! Finishing encoders: in the background while the engine runs, at once
//! when it stops.
//!
//! [`ProgramEncoder::finish`](ac_output::ProgramEncoder::finish) waits for
//! ffmpeg to flush, up to a timeout, so a session that stops while the
//! engine runs (the operator stopped it, or ffmpeg died) is finished on an
//! `ac-output-finish` helper thread, whose [`Event::Finished`] the next poll
//! reads; stats read just before finishing stand in when the finish fails.
//! [`OutputController::shutdown`] instead
//! finishes on the calling thread and joins every helper, including the one
//! still starting an encoder, which it then finishes too; dropping the
//! controller does the same. So no ffmpeg process or helper thread outlives
//! the controller.

use super::{Event, OutputController};
use crate::snapshot::OutputState;

impl OutputController {
    /// Moves the encoder to a helper thread that finishes it.
    pub(super) fn finish_in_background(&mut self) {
        let Some(encoder) = self.encoder.take() else {
            return;
        };
        let tx = self.sender.clone();
        let session = self.session;
        let spawned = std::thread::Builder::new()
            .name("ac-output-finish".into())
            .spawn(move || {
                let before = Box::new(encoder.stats());
                let result = encoder.finish();
                let _ = tx.send(Event::Finished {
                    session,
                    result,
                    before,
                });
            });
        match spawned {
            Ok(h) => self.helpers.push(h),
            // Could not spawn: the encoder was moved into the failed closure
            // and dropped with it, which kills and reaps ffmpeg.
            Err(e) => self.state = OutputState::Failed(format!("cannot finish output: {e}")),
        }
    }

    /// Finishes a running encoder synchronously and joins every helper.
    /// Returns the number of helper threads joined.
    pub fn shutdown(&mut self) -> usize {
        if let Some(encoder) = self.encoder.take() {
            let before = Box::new(encoder.stats());
            let result = encoder.finish();
            let _ = self.sender.send(Event::Finished {
                session: self.session,
                result,
                before,
            });
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
            let _ = self.sender.send(Event::Finished {
                session: self.session,
                result,
                before,
            });
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
