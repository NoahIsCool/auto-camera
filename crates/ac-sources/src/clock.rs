//! Clocks and real-time pacing.
//!
//! A source that decodes a file can produce frames far faster than real time,
//! but downstream stages expect it to behave like a camera. Sources therefore
//! hold every frame (and audio block) back until it is due on a [`Clock`].
//!
//! A `Timeline` maps media time onto the clock: the first item that becomes
//! ready anchors PTS zero to the clock reading at that moment, and an item with
//! PTS `p` is released at `origin + p`. All workers of one source share a
//! timeline, so video and audio stay in step with each other.
//!
//! Late items are released at once. The timeline never re-anchors: after a
//! stall the source catches up in a short burst. The latest-frame mailbox
//! absorbs that burst for video and the bounded audio queue trims it for
//! audio, and A/V alignment is preserved because both still follow PTS.
//!
//! The clock is injectable. [`WallClock`] paces real playback. [`VirtualClock`]
//! advances instantly whenever it is slept on, so tests and offline tools run
//! the identical pacing code unpaced. Waits are sliced into short sleeps so a
//! worker notices a stop request within [`MAX_SLEEP_SLICE`]; the same sliced
//! wait (`sleep_until`) times a supervisor's reconnect backoff, so a backoff
//! on a virtual clock passes at once too.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Longest single sleep while waiting for an item to become due.
pub const MAX_SLEEP_SLICE: Duration = Duration::from_millis(10);

/// A monotonic time source that pacing waits on.
pub trait Clock: Send + Sync {
    /// Time since the clock's own origin. Never decreases.
    fn now(&self) -> Duration;
    /// Blocks for about `duration` (real clocks) or advances simulated time by
    /// `duration` (simulated clocks).
    fn sleep(&self, duration: Duration);
}

/// Real time, measured from when the clock was created.
#[derive(Debug, Clone, Copy)]
pub struct WallClock {
    /// The instant that reads as zero.
    origin: Instant,
}

impl WallClock {
    /// A wall clock reading zero now.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for WallClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for WallClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Simulated time that advances only when slept on or advanced explicitly.
///
/// Sleeping returns immediately after moving the clock forward, so paced code
/// runs as fast as the work allows: this is how tests run sources unpaced.
#[derive(Debug, Default)]
pub struct VirtualClock {
    /// Current simulated time in nanoseconds.
    nanos: AtomicU64,
}

impl VirtualClock {
    /// A simulated clock reading zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves simulated time forward by `duration`, saturating at the maximum.
    pub fn advance(&self, duration: Duration) {
        let step = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        // The closure always returns `Some`, so the update cannot fail.
        let _ = self
            .nanos
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_add(step))
            });
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::Acquire))
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }
}

/// Blocks until `clock` reads at least `due`, sleeping at most
/// [`MAX_SLEEP_SLICE`] at a time. Returns `true` when due (at once if it
/// already is) and `false` when `stop` was raised first.
pub(crate) fn sleep_until(clock: &dyn Clock, due: Duration, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let now = clock.now();
        if now >= due {
            return true;
        }
        clock.sleep((due - now).min(MAX_SLEEP_SLICE));
    }
}

/// Maps media time onto a clock for one source; see the module docs.
pub(crate) struct Timeline {
    /// The clock items are paced against.
    clock: Arc<dyn Clock>,
    /// Clock reading that PTS zero maps to, fixed by the first wait.
    origin: OnceLock<Duration>,
}

impl fmt::Debug for Timeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Timeline")
            .field("origin", &self.origin.get())
            .finish_non_exhaustive()
    }
}

impl Timeline {
    /// A timeline that anchors itself on its first wait.
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            origin: OnceLock::new(),
        }
    }

    /// The clock's current reading.
    pub(crate) fn now(&self) -> Duration {
        self.clock.now()
    }

    /// Blocks until media time `pts` is due.
    ///
    /// Returns `true` when the item is due (immediately if it is late) and
    /// `false` when `stop` was raised first.
    pub(crate) fn wait_until(&self, pts: Duration, stop: &AtomicBool) -> bool {
        let origin = *self.origin.get_or_init(|| self.clock.now());
        sleep_until(&*self.clock, origin.saturating_add(pts), stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_advances_only_when_slept_on() {
        let c = VirtualClock::new();
        assert_eq!(c.now(), Duration::ZERO);
        c.sleep(Duration::from_millis(5));
        c.advance(Duration::from_millis(5));
        assert_eq!(c.now(), Duration::from_millis(10));
        c.advance(Duration::MAX);
        assert_eq!(c.now(), Duration::from_nanos(u64::MAX));
    }

    #[test]
    fn wall_clock_is_monotonic() {
        let c = WallClock::new();
        let a = c.now();
        c.sleep(Duration::from_millis(2));
        assert!(c.now() >= a + Duration::from_millis(2));
    }

    #[test]
    fn virtual_timeline_reaches_due_time_without_blocking() {
        let clock = Arc::new(VirtualClock::new());
        clock.advance(Duration::from_secs(7));
        let t = Timeline::new(clock.clone());
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        assert!(t.wait_until(Duration::ZERO, &stop));
        assert!(t.wait_until(Duration::from_secs(3600), &stop));
        assert!(clock.now() >= Duration::from_secs(3607));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn late_items_are_released_without_sleeping() {
        let clock = Arc::new(VirtualClock::new());
        let t = Timeline::new(clock.clone());
        let stop = AtomicBool::new(false);
        assert!(t.wait_until(Duration::ZERO, &stop));
        clock.advance(Duration::from_secs(2));
        assert!(t.wait_until(Duration::from_secs(1), &stop));
        assert_eq!(clock.now(), Duration::from_secs(2));
    }

    #[test]
    fn wall_timeline_paces_to_pts() {
        let t = Timeline::new(Arc::new(WallClock::new()));
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        assert!(t.wait_until(Duration::ZERO, &stop));
        assert!(t.wait_until(Duration::from_millis(40), &stop));
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn sleep_until_lands_on_the_due_time_in_slices() {
        let clock = VirtualClock::new();
        let stop = AtomicBool::new(false);
        assert!(sleep_until(&clock, Duration::from_millis(25), &stop));
        assert_eq!(clock.now(), Duration::from_millis(25), "no overshoot");
        assert!(sleep_until(&clock, Duration::ZERO, &stop), "already due");
        assert_eq!(clock.now(), Duration::from_millis(25));
        stop.store(true, Ordering::Release);
        assert!(!sleep_until(&clock, Duration::from_secs(9), &stop));
        assert_eq!(clock.now(), Duration::from_millis(25));
    }

    #[test]
    fn raised_stop_aborts_a_wait() {
        let t = Timeline::new(Arc::new(VirtualClock::new()));
        let stop = AtomicBool::new(true);
        assert!(!t.wait_until(Duration::ZERO, &stop));
    }

    #[test]
    fn stop_interrupts_a_long_wall_wait() {
        let t = Arc::new(Timeline::new(Arc::new(WallClock::new())));
        let stop = Arc::new(AtomicBool::new(false));
        let (t2, s2) = (t.clone(), stop.clone());
        let waiter = std::thread::spawn(move || t2.wait_until(Duration::from_secs(60), &s2));
        std::thread::sleep(Duration::from_millis(30));
        let asked = Instant::now();
        stop.store(true, Ordering::Release);
        assert!(!waiter.join().unwrap());
        assert!(asked.elapsed() < Duration::from_secs(1));
    }
}
