//! Health and loop bookkeeping shared between a source and its workers.
//!
//! [`Status`] holds the [`SourceState`], which only ever moves forward:
//! `Starting -> Running -> Stopped`. Advancing is a `fetch_max` on the state's
//! ordinal, so racing workers can never move a stopped source back to running.
//! The first error that stops a source is kept for the operator; later errors
//! are consequences of the first and are ignored.
//!
//! [`VideoProgress`] is how a file keeps its audio locked to its video. The
//! video worker defines the timeline (`pts = seq / fps`): it publishes how far
//! it has got in the current pass over the file and, when looping, the PTS at
//! which each pass ends and the next begins. The audio worker never releases
//! audio beyond the video's published progress unless it knows where the pass
//! ends; it trims audio that runs past the end and pads with silence when the
//! audio ends early, then starts its next pass exactly there. Without this,
//! any difference between the audio and video lengths of a file would
//! accumulate into A/V drift on every loop. Because the rule is "never ahead
//! of the published video", it holds under any pacing and any machine load.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use ac_media::SourceState;

/// Locks `mutex`, recovering the data if a worker panicked while holding it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ordinal of a state; the order is the only allowed direction of travel.
/// `Offline` and `Reconnecting` belong to supervised sources, which keep
/// their own state ([`crate::supervisor`]); here they rank with `Starting`
/// (not delivering, not finished), so a stray advance to one of them can
/// neither stop nor start a plain source.
fn ordinal(state: SourceState) -> u8 {
    match state {
        SourceState::Starting | SourceState::Offline | SourceState::Reconnecting => 0,
        SourceState::Running => 1,
        SourceState::Stopped => 2,
    }
}

/// Forward-only source state plus the error that stopped it, if any.
#[derive(Debug, Default)]
pub(crate) struct Status {
    /// Ordinal of the current [`SourceState`].
    state: AtomicU8,
    /// First error that stopped the source.
    error: Mutex<Option<String>>,
}

impl Status {
    /// A status in [`SourceState::Starting`].
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The current state.
    pub(crate) fn get(&self) -> SourceState {
        match self.state.load(Ordering::Acquire) {
            0 => SourceState::Starting,
            1 => SourceState::Running,
            _ => SourceState::Stopped,
        }
    }

    /// Moves to `state` unless the source is already further along.
    pub(crate) fn advance(&self, state: SourceState) {
        self.state.fetch_max(ordinal(state), Ordering::AcqRel);
    }

    /// Records `message` (unless an earlier error is kept) and stops.
    pub(crate) fn fail(&self, message: impl Into<String>) {
        lock(&self.error).get_or_insert_with(|| message.into());
        self.advance(SourceState::Stopped);
    }

    /// The error that stopped the source, if it stopped because of one.
    pub(crate) fn error(&self) -> Option<String> {
        lock(&self.error).clone()
    }
}

/// How far the video worker has got, read by the audio worker.
#[derive(Debug, Default)]
pub(crate) struct VideoProgress {
    /// Progress so far and whether more can follow.
    inner: Mutex<Progress>,
    /// Signalled on every change.
    changed: Condvar,
}

/// Contents of [`VideoProgress`].
#[derive(Debug, Default)]
struct Progress {
    /// Pass the video worker is in.
    pass: usize,
    /// PTS of the next frame the video worker would publish in `pass`.
    next_pts: Duration,
    /// `ends[k]` is the PTS where pass `k` ends and pass `k + 1` begins.
    ends: Vec<Duration>,
    /// The video worker has exited; nothing will change any more.
    closed: bool,
}

impl VideoProgress {
    /// Nothing published yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records that every frame of `pass` before `next_pts` is published.
    pub(crate) fn frame_published(&self, pass: usize, next_pts: Duration) {
        let mut p = lock(&self.inner);
        (p.pass, p.next_pts) = (pass, next_pts);
        self.changed.notify_all();
    }

    /// Records that the current pass ended at `end` and the next one starts
    /// there. Ignored once closed.
    pub(crate) fn pass_ended(&self, end: Duration) {
        let mut p = lock(&self.inner);
        if !p.closed {
            p.ends.push(end);
            p.pass = p.ends.len();
            p.next_pts = end;
            self.changed.notify_all();
        }
    }

    /// Declares that the video worker is done and wakes all waiters.
    pub(crate) fn close(&self) {
        lock(&self.inner).closed = true;
        self.changed.notify_all();
    }

    /// Blocks until audio of pass `pass` up to PTS `until` may be released:
    /// either the video has published frames of that pass up to `until`
    /// (returns `None`: the pass is at least that long), or the pass has ended
    /// (returns its end, past which audio must be trimmed), or the video worker
    /// is done (returns the end if known, else `None`).
    pub(crate) fn wait_clear(&self, pass: usize, until: Duration) -> Option<Duration> {
        let mut p = lock(&self.inner);
        loop {
            if let Some(&end) = p.ends.get(pass) {
                return Some(end);
            }
            if p.closed || (p.pass == pass && p.next_pts >= until) {
                return None;
            }
            p = self.changed.wait(p).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Blocks until pass `pass` has ended and returns its end; `None` when the
    /// video worker finished without ending it (end of input or stop).
    pub(crate) fn wait_end(&self, pass: usize) -> Option<Duration> {
        let mut p = lock(&self.inner);
        loop {
            if let Some(&end) = p.ends.get(pass) {
                return Some(end);
            }
            if p.closed {
                return None;
            }
            p = self.changed.wait(p).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Closes a [`VideoProgress`] when dropped, so a video worker that exits for
/// any reason (including a panic) never leaves the audio worker waiting.
pub(crate) struct CloseOnDrop<'a>(pub(crate) &'a VideoProgress);

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn state_only_moves_forward() {
        let s = Status::new();
        assert_eq!(s.get(), SourceState::Starting);
        s.advance(SourceState::Running);
        assert_eq!(s.get(), SourceState::Running);
        s.advance(SourceState::Starting);
        assert_eq!(s.get(), SourceState::Running);
        s.advance(SourceState::Offline);
        s.advance(SourceState::Reconnecting);
        assert_eq!(
            s.get(),
            SourceState::Running,
            "supervised states do not apply"
        );
        s.advance(SourceState::Stopped);
        s.advance(SourceState::Running);
        assert_eq!(s.get(), SourceState::Stopped);
    }

    #[test]
    fn first_error_is_kept_and_stops() {
        let s = Status::new();
        assert_eq!(s.error(), None);
        s.fail("first");
        s.fail("second");
        assert_eq!(s.error().as_deref(), Some("first"));
        assert_eq!(s.get(), SourceState::Stopped);
    }

    #[test]
    fn audio_waits_for_video_within_a_pass() {
        let v = Arc::new(VideoProgress::new());
        v.frame_published(0, Duration::from_millis(100));
        assert_eq!(v.wait_clear(0, Duration::from_millis(100)), None);
        let v2 = v.clone();
        let waiter = std::thread::spawn(move || v2.wait_clear(0, Duration::from_millis(150)));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!waiter.is_finished());
        v.frame_published(0, Duration::from_millis(167));
        assert_eq!(waiter.join().unwrap(), None);
    }

    #[test]
    fn ended_pass_reports_its_end() {
        let v = Arc::new(VideoProgress::new());
        let v2 = v.clone();
        let waiter = std::thread::spawn(move || v2.wait_clear(0, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        v.pass_ended(Duration::from_secs(1));
        assert_eq!(waiter.join().unwrap(), Some(Duration::from_secs(1)));
        assert_eq!(v.wait_end(0), Some(Duration::from_secs(1)));
        // Pass 1 starts at the boundary; audio of pass 1 waits for its frames.
        assert_eq!(v.wait_clear(1, Duration::from_secs(1)), None);
        v.frame_published(1, Duration::from_millis(1500));
        assert_eq!(v.wait_clear(1, Duration::from_millis(1400)), None);
        v.pass_ended(Duration::from_secs(2));
        assert_eq!(
            v.wait_clear(0, Duration::from_secs(9)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(v.wait_end(1), Some(Duration::from_secs(2)));
    }

    #[test]
    fn closing_releases_waiters_and_ignores_later_ends() {
        let v = Arc::new(VideoProgress::new());
        let (a, b) = (v.clone(), v.clone());
        let clear = std::thread::spawn(move || a.wait_clear(0, Duration::from_secs(5)));
        let end = std::thread::spawn(move || b.wait_end(0));
        std::thread::sleep(Duration::from_millis(20));
        drop(CloseOnDrop(&v));
        assert_eq!(clear.join().unwrap(), None);
        assert_eq!(end.join().unwrap(), None);
        v.pass_ended(Duration::from_secs(1));
        assert_eq!(v.wait_end(0), None);
    }
}
