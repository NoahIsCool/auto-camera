//! The per-tick trace from a running engine: one record per tick, in order,
//! with the clock's late and skipped ticks, and a writer that cannot keep
//! up never holding up the master clock.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use ac_engine::{Engine, EngineOptions, EngineReport, TraceRecord};
use ac_sources::Clock;

use common::{SPEED, WAIT, fast_time, studio};

/// Room for every record of these short runs.
const ROOM: usize = 100_000;

/// Runs `engine` until its snapshot says `ticks` ticks ran, then shuts it
/// down.
fn run_ticks(engine: Engine, ticks: u64) -> EngineReport {
    engine
        .handle()
        .wait_until(WAIT, |s| s.tick >= ticks)
        .expect("the engine ticks");
    engine.shutdown().expect("the engine stops")
}

/// Every record of a closed channel.
fn drain(rx: &Receiver<TraceRecord>) -> Vec<TraceRecord> {
    rx.iter().collect()
}

#[test]
fn the_runtime_sends_one_record_per_tick_in_order() {
    let (tx, rx) = sync_channel(ROOM);
    let engine = Engine::start_with(
        studio(2),
        EngineOptions {
            trace: Some(tx),
            ..fast_time()
        },
    )
    .unwrap();
    engine
        .handle()
        .wait_until(WAIT, |s| s.subjects.len() == 2)
        .expect("two subjects");
    let report = engine.shutdown().unwrap();
    // The engine's sender went with its thread, so the channel has closed.
    let records = drain(&rx);
    assert_eq!(records.len() as u64, report.ticks);
    let ticks: Vec<u64> = records.iter().map(|r| r.tick).collect();
    assert_eq!(ticks, (1..=report.ticks).collect::<Vec<u64>>());
    assert!(records.windows(2).all(|w| w[0].pts < w[1].pts));
    let timing = report.final_snapshot.timing;
    let late = records.iter().filter(|r| r.late).count() as u64;
    let skipped: u64 = records.iter().map(|r| r.skipped).sum();
    assert_eq!((late, skipped), (timing.late_ticks, timing.skipped_ticks));
    let last = records.last().unwrap();
    assert_eq!(last.pts, report.final_snapshot.media_time);
    assert_eq!(last.sources[0].subjects.len(), 2);
    assert!(
        last.shots
            .iter()
            .any(|s| s.target.is_some() && s.subject.is_some())
    );
}

/// Real time sped up by [`SPEED`], which jumps five seconds ahead once
/// told to, as a suspended laptop would.
#[derive(Debug)]
struct JumpClock {
    /// The instant that reads as zero.
    origin: Instant,
    /// The jump has happened.
    jumped: AtomicBool,
}

impl Clock for JumpClock {
    fn now(&self) -> Duration {
        let jump = if self.jumped.load(Ordering::SeqCst) {
            Duration::from_secs(5)
        } else {
            Duration::ZERO
        };
        self.origin.elapsed() * SPEED + jump
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration / SPEED);
    }
}

#[test]
fn skipped_ticks_are_marked_on_the_tick_after_them() {
    let clock = Arc::new(JumpClock {
        origin: Instant::now(),
        jumped: AtomicBool::new(false),
    });
    let (tx, rx) = sync_channel(ROOM);
    let engine = Engine::start_with(
        studio(1),
        EngineOptions {
            clock: clock.clone(),
            trace: Some(tx),
            ..EngineOptions::default()
        },
    )
    .unwrap();
    let h = engine.handle();
    h.wait_until(WAIT, |s| s.tick >= 10).unwrap();
    clock.jumped.store(true, Ordering::SeqCst);
    let at = h.snapshot().tick;
    let report = run_ticks(engine, at + 10);
    let records = drain(&rx);
    assert_eq!(records.len() as u64, report.ticks);
    // A loaded machine may skip a few ticks of its own; the jump skips
    // many more.
    let after: Vec<&TraceRecord> = records.iter().filter(|r| r.skipped >= 100).collect();
    assert_eq!(after.len(), 1, "one jump, one record after it");
    let r = after[0];
    // About five seconds at 30 ticks a second.
    assert!(r.skipped <= 160, "{}", r.skipped);
    let before = &records[(r.tick - 2) as usize];
    assert_eq!(before.tick + 1, r.tick, "skipped ticks have no record");
    // Media time went on over the skipped ticks: tick n is n periods in.
    let period = Duration::from_secs_f64(1.0 / 30.0);
    let gap = u32::try_from(r.skipped + 1).unwrap();
    assert_eq!(r.pts - before.pts, period * gap);
    let skipped: u64 = records.iter().map(|r| r.skipped).sum();
    assert_eq!(skipped, report.final_snapshot.timing.skipped_ticks);
}

#[test]
fn a_writer_that_falls_behind_never_holds_up_the_engine() {
    // Room for one record, and nobody reading.
    let (tx, rx) = sync_channel(1);
    let engine = Engine::start_with(
        studio(1),
        EngineOptions {
            trace: Some(tx),
            ..fast_time()
        },
    )
    .unwrap();
    let report = run_ticks(engine, 40);
    assert!(report.ticks >= 40);
    let ticks: Vec<u64> = drain(&rx).iter().map(|r| r.tick).collect();
    assert_eq!(ticks, [1], "the rest were dropped, not queued");

    // A writer that is gone ends the trace, not the engine.
    let (tx, rx) = sync_channel(ROOM);
    drop(rx);
    let engine = Engine::start_with(
        studio(1),
        EngineOptions {
            trace: Some(tx),
            ..fast_time()
        },
    )
    .unwrap();
    assert!(run_ticks(engine, 20).ticks >= 20);
}
