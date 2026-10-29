//! The detection log from a running engine: every pass it applied, in the
//! order applied and as the detector answered it, and a writer that cannot
//! keep up never holding up the master clock.

mod common;

use std::sync::mpsc::sync_channel;

use ac_engine::{Engine, EngineOptions, PassRecord};
use ac_sources::SyntheticPattern;

use common::{WAIT, fast_time, studio};

/// Room for every pass of these short runs.
const ROOM: usize = 100_000;

#[test]
fn the_runtime_sends_every_applied_pass_in_order() {
    let (tx, rx) = sync_channel(ROOM);
    let engine = Engine::start_with(
        studio(2),
        EngineOptions {
            detections: Some(tx),
            ..fast_time()
        },
    )
    .unwrap();
    engine
        .handle()
        .wait_until(WAIT, |s| {
            s.subjects.len() == 2 && s.sources[0].discovery.applied >= 5
        })
        .expect("two subjects, found by several passes");
    let report = engine.shutdown().unwrap();
    // The engine's sender went with its thread, so the channel has closed.
    let passes: Vec<PassRecord> = rx.iter().collect();
    let applied = report.final_snapshot.sources[0].discovery.applied;
    assert_eq!(passes.len() as u64, applied, "one record per applied pass");
    assert!(passes.windows(2).all(|w| w[0].seq < w[1].seq), "in order");
    // Each is the ground truth of its frame, as the detector answered.
    let pattern = SyntheticPattern::new(320, 180, 30.0, 2).unwrap();
    for pass in &passes {
        assert_eq!(pass.source.0, "stage");
        assert_eq!(pass.pts, pattern.frame_rate().pts(pass.seq));
        assert_eq!(pass.queries.len(), 1);
        let truth = pattern.ground_truth(pass.pts);
        let boxes: Vec<_> = pass.detections.iter().map(|d| d.bbox).collect();
        assert_eq!(boxes, truth.iter().map(|g| g.bbox).collect::<Vec<_>>());
        assert!(pass.detections.iter().all(|d| d.score == 0.9));
    }
}

#[test]
fn a_full_log_drops_passes_and_a_closed_one_ends_without_stopping_the_engine() {
    // Room for one pass: the rest are dropped, never waited for.
    let (tx, rx) = sync_channel(1);
    let engine = Engine::start_with(
        studio(1),
        EngineOptions {
            detections: Some(tx),
            ..fast_time()
        },
    )
    .unwrap();
    let h = engine.handle();
    h.wait_until(WAIT, |s| s.sources[0].discovery.applied >= 3)
        .expect("passes are applied");
    assert_eq!(rx.try_iter().count(), 1, "the one that fitted");
    drop(rx);
    let ticks = h.snapshot().tick;
    h.wait_until(WAIT, |s| {
        s.tick >= ticks + 30 && s.sources[0].discovery.applied >= 6
    })
    .expect("the engine runs on without its log");
    engine.shutdown().unwrap();
}
