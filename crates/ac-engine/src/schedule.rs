//! The master clock's tick schedule and small timing statistics.
//!
//! The engine thread renders one program frame per tick at the output frame
//! rate. Tick `n` is *due* at `origin + n · period` on the engine's
//! [`Clock`](ac_sources::Clock); its media time is exactly `n · period`, so
//! every stage downstream (framing springs, transitions, the director's
//! dwell timers, the encoder's frame count) sees a perfectly regular clock
//! even when the thread itself wakes up late.
//!
//! [`TickSchedule`] decides, from a clock reading, whether to wait or to run
//! the next tick:
//!
//! * a tick that starts more than half a period after it was due is *late*;
//! * when the thread falls more than `max_lag` behind (a debugger pause, a
//!   suspended laptop, a virtual clock pushed forward by other threads), the
//!   schedule skips the missed ticks and re-anchors instead of replaying a
//!   burst. Skipped ticks are counted; the media time still advances by the
//!   skipped amount, so the program clock stays aligned with the sources.
//!
//! [`RateMeter`] measures event rates (source frame rates) over a sliding
//! window of media time, and [`TickTimer`] keeps tick-duration statistics.

use std::collections::VecDeque;
use std::time::Duration;

/// What the engine thread should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickDecision {
    /// Sleep this long before asking again.
    Wait(Duration),
    /// Run a tick now.
    Tick {
        /// Index of the tick (0 for the first).
        index: u64,
        /// Media time of the tick: `index · period`.
        media_time: Duration,
        /// The tick started more than half a period after it was due.
        late: bool,
        /// Ticks skipped just before this one because the thread fell behind.
        skipped: u64,
    },
}

/// Fixed-rate tick schedule on an arbitrary clock; see the module docs.
#[derive(Debug, Clone)]
pub struct TickSchedule {
    /// Tick interval.
    period: Duration,
    /// Clock reading at which tick 0 is due.
    origin: Duration,
    /// Index of the next tick.
    next: u64,
    /// Falling further behind than this skips ticks.
    max_lag: Duration,
}

impl TickSchedule {
    /// A schedule with tick 0 due at `origin`. `period` must be positive.
    pub fn new(origin: Duration, period: Duration, max_lag: Duration) -> Self {
        Self {
            period: period.max(Duration::from_nanos(1)),
            origin,
            next: 0,
            max_lag: max_lag.max(period),
        }
    }

    /// The tick interval.
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Clock reading at which tick `index` is due.
    fn due(&self, index: u64) -> Duration {
        self.origin
            + self
                .period
                .saturating_mul(u32::try_from(index).unwrap_or(u32::MAX))
    }

    /// Decides what to do at clock reading `now`.
    pub fn poll(&mut self, now: Duration) -> TickDecision {
        let due = self.due(self.next);
        if now < due {
            return TickDecision::Wait(due - now);
        }
        let mut skipped = 0;
        if now - due > self.max_lag {
            let behind = (now - due).as_nanos() / self.period.as_nanos().max(1);
            skipped = u64::try_from(behind).unwrap_or(u64::MAX);
            self.next = self.next.saturating_add(skipped);
        }
        let index = self.next;
        let due = self.due(index);
        self.next = self.next.saturating_add(1);
        TickDecision::Tick {
            index,
            media_time: due - self.origin,
            late: now.saturating_sub(due) > self.period / 2,
            skipped,
        }
    }
}

/// Counts events over a sliding window of media time.
#[derive(Debug, Clone)]
pub struct RateMeter {
    /// Window length.
    window: Duration,
    /// Times of the events inside the window, oldest first.
    events: VecDeque<Duration>,
}

impl RateMeter {
    /// A meter averaging over `window`.
    pub fn new(window: Duration) -> Self {
        Self {
            window: window.max(Duration::from_millis(1)),
            events: VecDeque::new(),
        }
    }

    /// Records an event at `at`.
    pub fn record(&mut self, at: Duration) {
        self.events.push_back(at);
        self.expire(at);
    }

    /// Drops events older than the window ending at `now`.
    fn expire(&mut self, now: Duration) {
        while self
            .events
            .front()
            .is_some_and(|&t| now.saturating_sub(t) > self.window)
        {
            self.events.pop_front();
        }
    }

    /// Events per second over the window ending at `now`.
    pub fn rate(&self, now: Duration) -> f64 {
        let n = self
            .events
            .iter()
            .filter(|&&t| t <= now && now - t <= self.window)
            .count();
        n as f64 / self.window.as_secs_f64()
    }
}

/// Tick-duration statistics, in milliseconds of processing time.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TickTimer {
    /// Ticks measured.
    pub count: u64,
    /// Duration of the most recent tick.
    pub last_ms: f64,
    /// Exponential moving average (time constant about 30 ticks).
    pub avg_ms: f64,
    /// Longest tick so far.
    pub max_ms: f64,
}

impl TickTimer {
    /// Records one tick that took `elapsed`.
    pub fn record(&mut self, elapsed: Duration) {
        let ms = elapsed.as_secs_f64() * 1e3;
        self.avg_ms = if self.count == 0 {
            ms
        } else {
            self.avg_ms + (ms - self.avg_ms) / 30.0
        };
        self.count += 1;
        self.last_ms = ms;
        self.max_ms = self.max_ms.max(ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tick period of the tests (25 fps).
    const P: Duration = Duration::from_millis(40);

    /// `v` milliseconds.
    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn on_time_ticks_follow_the_period() {
        let mut s = TickSchedule::new(ms(1000), P, Duration::from_secs(1));
        assert_eq!(s.poll(ms(990)), TickDecision::Wait(ms(10)));
        for i in 0..5u64 {
            let now = ms(1000 + 40 * i);
            match s.poll(now) {
                TickDecision::Tick {
                    index,
                    media_time,
                    late,
                    skipped,
                } => {
                    assert_eq!(index, i);
                    assert_eq!(media_time, ms(40 * i));
                    assert!(!late && skipped == 0);
                }
                other => panic!("expected a tick, got {other:?}"),
            }
            assert_eq!(s.poll(now), TickDecision::Wait(P));
        }
    }

    #[test]
    fn a_tick_more_than_half_a_period_late_is_flagged() {
        let mut s = TickSchedule::new(Duration::ZERO, P, Duration::from_secs(1));
        assert!(matches!(
            s.poll(ms(25)),
            TickDecision::Tick { late: true, .. }
        ));
        // Catching up: the next tick is already due, and on time relative to
        // its own due time.
        assert!(matches!(
            s.poll(ms(41)),
            TickDecision::Tick {
                index: 1,
                late: false,
                ..
            }
        ));
    }

    #[test]
    fn falling_far_behind_skips_ticks_but_keeps_media_time() {
        let mut s = TickSchedule::new(Duration::ZERO, P, ms(200));
        assert!(matches!(s.poll(Duration::ZERO), TickDecision::Tick { .. }));
        match s.poll(ms(1000)) {
            TickDecision::Tick {
                index,
                media_time,
                skipped,
                ..
            } => {
                assert_eq!(skipped, 24);
                assert_eq!(index, 25);
                assert_eq!(media_time, ms(1000));
            }
            other => panic!("expected a tick, got {other:?}"),
        }
        assert_eq!(s.poll(ms(1000)), TickDecision::Wait(P));
    }

    #[test]
    fn small_lag_is_caught_up_without_skipping() {
        let mut s = TickSchedule::new(Duration::ZERO, P, ms(500));
        let mut ticks = 0;
        while let TickDecision::Tick { skipped, .. } = s.poll(ms(150)) {
            assert_eq!(skipped, 0);
            ticks += 1;
        }
        assert_eq!(ticks, 4); // due at 0, 40, 80 and 120 ms
    }

    #[test]
    fn rate_meter_counts_within_the_window() {
        let mut m = RateMeter::new(Duration::from_secs(1));
        assert_eq!(m.rate(Duration::ZERO), 0.0);
        for i in 0..60 {
            m.record(ms(i * 33));
        }
        let r = m.rate(ms(59 * 33));
        assert!((29.0..=31.0).contains(&r), "{r}");
        assert_eq!(m.rate(ms(10_000)), 0.0);
    }

    #[test]
    fn tick_timer_tracks_last_average_and_max() {
        let mut t = TickTimer::default();
        t.record(ms(10));
        assert_eq!(
            (t.count, t.last_ms, t.avg_ms, t.max_ms),
            (1, 10.0, 10.0, 10.0)
        );
        t.record(ms(40));
        assert_eq!(t.max_ms, 40.0);
        assert!(t.avg_ms > 10.0 && t.avg_ms < 40.0);
        t.record(ms(5));
        assert_eq!(t.last_ms, 5.0);
        assert_eq!(t.max_ms, 40.0);
    }
}
