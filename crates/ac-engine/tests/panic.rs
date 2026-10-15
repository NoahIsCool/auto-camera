//! A panic on the engine thread is not silent: the last snapshot says the
//! engine stopped and why, waiting on snapshots returns at once, commands
//! are refused and shutdown reports the panic.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ac_engine::{CommandError, Engine, EngineCommand, EngineError, EngineOptions, OutputState};
use ac_sources::Clock;

use common::{WAIT, studio};

/// Real time, except that reading it on the engine thread panics once the
/// fuse has burnt down: a stand-in for any bug inside a tick.
#[derive(Debug)]
struct Grenade {
    /// The instant that reads as zero.
    origin: Instant,
    /// Readings taken on the engine thread so far.
    calls: AtomicU64,
    /// Reading that panics.
    fuse: u64,
}

impl Clock for Grenade {
    fn now(&self) -> Duration {
        if std::thread::current().name() == Some("ac-engine")
            && self.calls.fetch_add(1, Ordering::SeqCst) == self.fuse
        {
            panic!("injected failure inside a tick");
        }
        self.origin.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[test]
fn a_panic_on_the_engine_thread_is_published() {
    let clock = Arc::new(Grenade {
        origin: Instant::now(),
        calls: AtomicU64::new(0),
        fuse: 40,
    });
    let engine = Engine::start_with(
        studio(1),
        EngineOptions {
            clock,
            ..EngineOptions::default()
        },
    )
    .expect("engine starts");
    let h = engine.handle();
    let deadline = Instant::now() + WAIT;
    let last = loop {
        let s = h.snapshot();
        if !s.running {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "the engine never said it stopped"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let fatal = last.fatal.as_deref().unwrap_or_default();
    assert!(
        fatal.contains("panicked") && fatal.contains("injected failure"),
        "{fatal:?}"
    );
    assert!(last.tick > 0, "ticks ran before the failure");
    assert!(!matches!(
        last.output.state,
        OutputState::Starting | OutputState::Running | OutputState::Stopping
    ));
    // Waiting for anything returns at once instead of timing out.
    let started = Instant::now();
    assert!(h.wait_until(WAIT, |s| s.tick > u64::MAX / 2).is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        h.send(EngineCommand::Cut),
        Err(CommandError::Unavailable("engine stopped"))
    );
    assert!(matches!(engine.shutdown(), Err(EngineError::Panicked)));
}
