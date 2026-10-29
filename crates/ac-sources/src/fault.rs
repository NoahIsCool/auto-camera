//! Fault injection: a synthetic source whose input drops out on a schedule.
//!
//! Reconnect handling is only as good as its tests, and a real outage
//! (unplugging a camera, killing ffmpeg) is slow and racy. [`FlakySynthetic`]
//! is a [`SourceOpener`] for a [`SyntheticPattern`] with [`Outage`]s planned
//! on the clock:
//!
//! * while an outage lasts, opening fails, as for a device that is not
//!   plugged in;
//! * a connection opened before an outage fails with an error when the
//!   clock reaches the outage, as a source does when its input is lost: the
//!   first frame due at or after the outage's start is not published.
//!
//! Every attempt is logged with its clock reading and outcome ([`OpenLog`]),
//! so a test can check when a supervisor tried and when it got through. On
//! a [`crate::VirtualClock`] the whole schedule is deterministic: the
//! pattern renders unpaced, and a supervisor's backoff waits pass at once.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ac_core::SourceId;
use ac_media::{MediaError, VideoSource};

use crate::clock::Clock;
use crate::status::lock;
use crate::supervisor::SourceOpener;
use crate::synthetic::{SyntheticPattern, SyntheticSource};

/// A span of clock readings, `from` inclusive to `until` exclusive, during
/// which a [`FlakySynthetic`]'s input is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outage {
    /// When the input drops out.
    pub from: Duration,
    /// When it can be opened again.
    pub until: Duration,
}

impl Outage {
    /// True when the input is gone at clock reading `t`.
    pub fn covers(&self, t: Duration) -> bool {
        self.from <= t && t < self.until
    }
}

/// One attempt to open a [`FlakySynthetic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAttempt {
    /// Clock reading of the attempt.
    pub at: Duration,
    /// Whether it opened.
    pub opened: bool,
}

/// The attempts made on one [`FlakySynthetic`], oldest first; a clone shares
/// the log, so a test keeps one while the opener moves into a supervisor.
#[derive(Debug, Clone, Default)]
pub struct OpenLog(Arc<Mutex<Vec<OpenAttempt>>>);

impl OpenLog {
    /// Every attempt so far.
    pub fn attempts(&self) -> Vec<OpenAttempt> {
        lock(&self.0).clone()
    }

    /// Records one attempt.
    fn record(&self, attempt: OpenAttempt) {
        lock(&self.0).push(attempt);
    }
}

/// Opens a synthetic pattern whose input drops out during planned outages;
/// see the module docs.
pub struct FlakySynthetic {
    /// Id of every connection.
    id: SourceId,
    /// The pattern every connection plays from its start.
    pattern: SyntheticPattern,
    /// Paces the connections and times the outages.
    clock: Arc<dyn Clock>,
    /// When the input is gone.
    outages: Vec<Outage>,
    /// The attempts so far.
    log: OpenLog,
}

impl fmt::Debug for FlakySynthetic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlakySynthetic")
            .field("id", &self.id)
            .field("outages", &self.outages)
            .finish_non_exhaustive()
    }
}

impl FlakySynthetic {
    /// An opener of `pattern` as source `id`, paced by `clock`, whose input
    /// is gone during each of `outages`.
    pub fn new(
        id: SourceId,
        pattern: SyntheticPattern,
        clock: Arc<dyn Clock>,
        outages: Vec<Outage>,
    ) -> Self {
        Self {
            id,
            pattern,
            clock,
            outages,
            log: OpenLog::default(),
        }
    }

    /// The log of attempts, shared with this opener.
    pub fn log(&self) -> OpenLog {
        self.log.clone()
    }
}

impl SourceOpener for FlakySynthetic {
    fn open(&mut self) -> Result<Box<dyn VideoSource>, MediaError> {
        let now = self.clock.now();
        let gone = self.outages.iter().find(|o| o.covers(now));
        self.log.record(OpenAttempt {
            at: now,
            opened: gone.is_none(),
        });
        if let Some(outage) = gone {
            return Err(MediaError::Open(format!(
                "{}: input gone until {:?}",
                self.id.0, outage.until
            )));
        }
        let fail_at = self
            .outages
            .iter()
            .map(|o| o.from)
            .filter(|&from| from > now)
            .min();
        let source = SyntheticSource::start_failing(
            self.id.clone(),
            self.pattern.clone(),
            self.clock.clone(),
            fail_at,
        )?;
        Ok(Box::new(source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::VirtualClock;
    use ac_media::SourceState;
    use std::time::Instant;

    /// `s` seconds.
    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn outages_cover_their_start_but_not_their_end() {
        let o = Outage {
            from: secs(1.0),
            until: secs(2.0),
        };
        assert!(!o.covers(secs(0.999)));
        assert!(o.covers(secs(1.0)) && o.covers(secs(1.999)));
        assert!(!o.covers(secs(2.0)));
    }

    #[test]
    fn opening_fails_during_an_outage_and_a_connection_fails_at_the_next() {
        let clock = Arc::new(VirtualClock::new());
        let pattern = SyntheticPattern::new(32, 18, 30.0, 1).unwrap();
        let outages = vec![
            Outage {
                from: secs(1.0),
                until: secs(2.0),
            },
            Outage {
                from: secs(3.0),
                until: secs(4.0),
            },
        ];
        let mut opener = FlakySynthetic::new(SourceId("f".into()), pattern, clock.clone(), outages);
        let log = opener.log();
        let mut source = opener.open().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while source.state() != SourceState::Stopped && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(source.state(), SourceState::Stopped);
        assert!(source.error().unwrap().contains("injected fault"));
        let last = source.latest().unwrap();
        assert!(last.pts < secs(1.0), "nothing at or after the outage");
        assert_eq!(
            clock.now(),
            secs(1.0),
            "it failed on the first frame due then"
        );
        source.stop();
        let err = opener.open().err().unwrap();
        assert!(err.to_string().contains("gone until"), "{err}");
        clock.advance(secs(1.5));
        let mut again = opener.open().unwrap();
        again.stop();
        assert_eq!(
            log.attempts(),
            [
                OpenAttempt {
                    at: Duration::ZERO,
                    opened: true
                },
                OpenAttempt {
                    at: secs(1.0),
                    opened: false
                },
                OpenAttempt {
                    at: secs(2.5),
                    opened: true
                },
            ]
        );
    }
}
