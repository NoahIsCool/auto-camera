//! A source other than the phase reference, shown through its presentation
//! buffer (`ac_engine::presentation`), in the running engine on a sped-up
//! clock: its frames reach the core and keep up with the reference's, its
//! subjects are tracked on them, and its shots go on air.

mod common;

use ac_core::ShotId;
use ac_engine::{EngineCommand, SourceEntry};

use common::{WAIT, start, studio};

/// The shot with `id`.
fn shot(id: &str) -> ShotId {
    ShotId(id.into())
}

#[test]
fn a_second_source_is_presented_tracked_and_put_on_air() {
    let mut config = studio(1);
    let second = config.sources[0].clone();
    config.sources.push(SourceEntry {
        id: "side".into(),
        ..second
    });
    let engine = start(config);
    let h = engine.handle();
    h.wait_until(WAIT, |s| {
        s.sources[1].frames > 60 && s.shot(&shot("side/1/medium")).is_some_and(|v| v.available)
    })
    .expect("the second source runs and its subject is framed");
    h.send(EngineCommand::SetPreview {
        shot: shot("side/1/medium"),
    })
    .unwrap();
    h.send(EngineCommand::Cut).unwrap();
    let snap = h
        .wait_until(WAIT, |s| s.switcher.program == Some(shot("side/1/medium")))
        .expect("on air");
    let (stage, side) = (&snap.sources[0], &snap.sources[1]);
    assert!(side.available && side.vision.frames > 0, "{side:?}");
    // The buffer shows the source a frame or two late but keeps up with it.
    assert!(side.frames + 10 >= stage.frames, "{stage:?} {side:?}");
    assert!(snap.render_issues.is_empty(), "{:?}", snap.render_issues);
    let report = engine.shutdown().unwrap();
    assert!(!report.final_snapshot.running);
}
