//! Termination signals: Ctrl-C, `SIGTERM` and `SIGHUP` quit the way
//! closing the window does.
//!
//! # Theory
//!
//! The detector sidecar runs in its own process group (ADR 0004), so a
//! terminal's Ctrl-C reaches only the app. Without a handler the app would
//! die at once, `Engine::shutdown` would never run, and the group isolation
//! would keep the sidecar alive until it next touched its pipes: after its
//! current load or pass, or never if it is wedged. So the app installs one
//! (the `ctrlc` crate, with its `termination` feature: the handler runs on
//! its own thread, not in signal context). The first signal sets a flag and
//! wakes whoever is waiting: the window closes, a headless run stops early,
//! and both then shut the engine down as usual (the sidecar's stdin is
//! closed, it gets 2 s, then its group is killed and reaped). A second
//! signal exits at once with status 130, for a shutdown that hangs; the
//! sidecar's own parent watchdog then ends it.
//!
//! # Invariants
//!
//! * The handler is installed at most once per process (`ctrlc` refuses a
//!   second one); [`QuitSignal::install`] reports that as an error.
//! * A request is never lost: the flag is set before the waker runs, and
//!   the window also polls the flag every frame.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

/// Exit status after a second signal (128 + SIGINT, as shells report it).
pub const INTERRUPTED: u8 = 130;

/// Something to call when a quit is requested, such as closing the window.
type Waker = Box<dyn Fn() + Send + Sync>;

/// What a signal means, by how many came before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// The first: quit cleanly.
    First,
    /// Another: give up on the clean quit.
    Repeated,
}

/// Termination requests, shared by the signal handler and the app; cheap
/// to clone. See the module docs.
#[derive(Clone, Default)]
pub struct QuitSignal {
    /// Signals received.
    count: Arc<AtomicU32>,
    /// Called on the first signal, once set.
    waker: Arc<OnceLock<Waker>>,
}

impl std::fmt::Debug for QuitSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuitSignal")
            .field("count", &self.count.load(Ordering::Acquire))
            .field("waker", &self.waker.get().is_some())
            .finish()
    }
}

impl QuitSignal {
    /// Requests that are not yet hooked to the process's signals.
    pub fn new() -> Self {
        Self::default()
    }

    /// Routes Ctrl-C, `SIGTERM` and `SIGHUP` here: the first requests a
    /// quit, a later one exits the process with [`INTERRUPTED`].
    pub fn install(&self) -> Result<(), String> {
        let quit = self.clone();
        ctrlc::set_handler(move || {
            if quit.signal() == Signal::Repeated {
                eprintln!("auto-camera: second termination signal, exiting now");
                std::process::exit(i32::from(INTERRUPTED));
            }
            eprintln!("auto-camera: termination signal, shutting down (again to force)");
            tracing::warn!("termination signal, shutting down");
        })
        .map_err(|e| format!("cannot handle termination signals: {e}"))
    }

    /// Records one signal and wakes the waiter on the first; what the
    /// handler does, callable without a real signal.
    pub fn signal(&self) -> Signal {
        if self.count.fetch_add(1, Ordering::AcqRel) > 0 {
            return Signal::Repeated;
        }
        if let Some(wake) = self.waker.get() {
            wake();
        }
        Signal::First
    }

    /// True once a quit was requested.
    pub fn requested(&self) -> bool {
        self.count.load(Ordering::Acquire) > 0
    }

    /// Calls `wake` on the first signal, or now if it already came. Only
    /// the first waker set is kept; a later one is dropped unused. (A
    /// signal racing this call may wake twice, which closing a window or
    /// ending a wait tolerates.)
    pub fn wake_with(&self, wake: impl Fn() + Send + Sync + 'static) {
        if self.waker.set(Box::new(wake)).is_ok()
            && self.requested()
            && let Some(wake) = self.waker.get()
        {
            wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn the_first_signal_requests_a_quit_and_wakes_once() {
        let quit = QuitSignal::new();
        let woken = Arc::new(AtomicUsize::new(0));
        let w = woken.clone();
        quit.wake_with(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        assert!(!quit.requested());
        assert_eq!(woken.load(Ordering::SeqCst), 0, "no signal yet");
        let shared = quit.clone();
        assert_eq!(shared.signal(), Signal::First);
        assert!(quit.requested(), "clones share the request");
        assert_eq!(woken.load(Ordering::SeqCst), 1);
        assert_eq!(quit.signal(), Signal::Repeated);
        assert_eq!(woken.load(Ordering::SeqCst), 1, "woken once");
    }

    #[test]
    fn a_waker_set_after_the_signal_runs_at_once() {
        let quit = QuitSignal::new();
        assert_eq!(quit.signal(), Signal::First);
        let woken = Arc::new(AtomicUsize::new(0));
        let w = woken.clone();
        quit.wake_with(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(woken.load(Ordering::SeqCst), 1);
        // A second waker is ignored.
        quit.wake_with(|| panic!("only the first waker is kept"));
    }

    #[test]
    fn without_a_waker_a_signal_is_still_recorded() {
        let quit = QuitSignal::new();
        assert_eq!(quit.signal(), Signal::First);
        assert!(quit.requested());
        assert!(format!("{quit:?}").contains("count: 1"));
    }
}
