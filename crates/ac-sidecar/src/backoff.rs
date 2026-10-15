//! Restart pacing for a failing sidecar, pure.
//!
//! # Theory
//!
//! A sidecar that crashes on every request must not be respawned in a
//! tight loop: each start loads gigabytes of weights (4.6 s and 3.3 GiB
//! for SAM 3.1, measured) and would starve the machine the show runs on.
//! After the k-th consecutive failure the next start waits
//! `initial * 2^(k-1)`, capped at `max`. Requests that arrive while it
//! waits are refused at once (the caller asks again at its own rate), and
//! the first request after the wait starts a new process.
//!
//! A child that answers a request, with a result or with an error reply,
//! is healthy: the count resets. Saying hello is not enough, so a sidecar
//! that loads and then dies on every request still backs off.
//!
//! # Invariants
//!
//! * The delay never exceeds `max` and never decreases while failures
//!   accumulate.
//! * Time enters as an argument; nothing here reads a clock.

use std::time::{Duration, Instant};

/// Consecutive-failure counter and the earliest time of the next start.
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    /// Wait after the first failure.
    initial: Duration,
    /// Longest wait.
    max: Duration,
    /// Consecutive failures since the last healthy answer.
    failures: u32,
    /// Earliest time a new process may be started.
    retry_at: Option<Instant>,
}

impl Backoff {
    /// A counter with no failures.
    pub(crate) fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            failures: 0,
            retry_at: None,
        }
    }

    /// The wait after `failures` consecutive failures.
    pub(crate) fn delay(&self, failures: u32) -> Duration {
        if failures == 0 {
            return Duration::ZERO;
        }
        let doubling = 1u32 << (failures - 1).min(31);
        self.initial.saturating_mul(doubling).min(self.max)
    }

    /// Records a failure at `now` and returns the wait before the next start.
    pub(crate) fn fail(&mut self, now: Instant) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let delay = self.delay(self.failures);
        self.retry_at = Some(now.checked_add(delay).unwrap_or(now));
        delay
    }

    /// Records a healthy answer: the next failure waits `initial` again.
    pub(crate) fn reset(&mut self) {
        self.failures = 0;
        self.retry_at = None;
    }

    /// How long a start must still wait at `now`, if at all.
    pub(crate) fn wait_left(&self, now: Instant) -> Option<Duration> {
        self.retry_at
            .and_then(|t| t.checked_duration_since(now))
            .filter(|d| !d.is_zero())
    }

    /// Earliest time of the next start, while one must wait.
    pub(crate) fn retry_at(&self) -> Option<Instant> {
        self.retry_at
    }

    /// Consecutive failures since the last healthy answer.
    pub(crate) fn failures(&self) -> u32 {
        self.failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `v` milliseconds.
    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn delays_double_up_to_the_cap() {
        let b = Backoff::new(ms(1000), ms(30_000));
        let delays: Vec<u64> = (0..8).map(|k| b.delay(k).as_millis() as u64).collect();
        assert_eq!(delays, [0, 1000, 2000, 4000, 8000, 16_000, 30_000, 30_000]);
        assert_eq!(b.delay(u32::MAX), ms(30_000));
    }

    #[test]
    fn a_failure_blocks_starts_until_its_delay_passes() {
        let t0 = Instant::now();
        let mut b = Backoff::new(ms(100), ms(1000));
        assert_eq!(b.wait_left(t0), None);
        assert_eq!(b.fail(t0), ms(100));
        assert_eq!(b.wait_left(t0 + ms(40)), Some(ms(60)));
        assert_eq!(b.wait_left(t0 + ms(100)), None);
        assert_eq!(b.wait_left(t0 + ms(500)), None);
        assert_eq!(b.fail(t0 + ms(500)), ms(200));
        assert_eq!(b.failures(), 2);
        assert_eq!(b.retry_at(), Some(t0 + ms(700)));
    }

    #[test]
    fn a_healthy_answer_resets_the_count() {
        let t0 = Instant::now();
        let mut b = Backoff::new(ms(100), ms(1000));
        for _ in 0..5 {
            b.fail(t0);
        }
        assert_eq!(b.failures(), 5);
        b.reset();
        assert_eq!(b.wait_left(t0), None);
        assert_eq!(b.fail(t0), ms(100));
    }

    #[test]
    fn a_zero_initial_delay_never_waits() {
        let t0 = Instant::now();
        let mut b = Backoff::new(Duration::ZERO, ms(1000));
        assert_eq!(b.fail(t0), Duration::ZERO);
        assert_eq!(b.wait_left(t0), None);
    }
}
