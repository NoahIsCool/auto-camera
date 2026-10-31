//! Controlled child-ownership handoffs: a claimed decoder still belongs to
//! the registry until its reaper completes, so competitors must wait before
//! closing its pipe or reporting the source stopped.

use super::*;
use std::sync::mpsc;

/// The first operation claiming the child for reaping.
#[derive(Clone, Copy, Debug)]
enum Claim {
    /// A worker ends one decoder pass.
    Kill,
    /// The source stops every decoder.
    Close,
}

/// A competing operation that must observe completed reaping.
#[derive(Clone, Copy, Debug)]
enum Wait {
    /// Another worker stops the same decoder.
    Kill,
    /// A worker finishes the decoder after reading its output.
    Finish,
    /// Another thread stops the source.
    Close,
}

/// A paused owner prevents every competing stop/finish from returning;
/// releasing it reaps the child and lets all callers close their pipes.
#[test]
fn competing_stops_wait_until_the_owner_reaps_the_child() {
    for claim in [Claim::Kill, Claim::Close] {
        for wait in [Wait::Kill, Wait::Finish, Wait::Close] {
            check_handoff(claim, wait);
        }
    }
}

/// Exercises one ownership handoff with a real child and bounded channel
/// waits, releasing and reaping the child before checking the outcome.
fn check_handoff(claim: Claim, wait: Wait) {
    let children = Arc::new(Children::new());
    let mut command = Command::new("sleep");
    command.arg("30");
    let Spawned { key, stdout } = children.spawn(&mut command).unwrap().unwrap();
    let pid = children.pids()[0];
    let (claimed, observe_claim) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let owner = children.clone();
    let reaper = std::thread::spawn(move || {
        let held = |child| {
            // Even an assertion failure in the controlling thread must
            // release and reap this child before the callback returns.
            let _ = claimed.send(());
            let _ = released.recv();
            reap(child);
        };
        match claim {
            Claim::Kill => owner.kill_with(key, held),
            Claim::Close => owner.close_with(held),
        }
    });
    observe_claim.recv_timeout(Duration::from_secs(5)).unwrap();
    let (started, observe_start) = mpsc::channel();
    let (finished, observe_finish) = mpsc::channel();
    let waiter = children.clone();
    let worker = std::thread::spawn(move || {
        let _ = started.send(());
        match wait {
            Wait::Kill => waiter.kill(key),
            Wait::Finish => {
                waiter.finish_within(key, Duration::ZERO);
            }
            Wait::Close => waiter.close(),
        }
        drop(stdout);
        let _ = finished.send(());
    });
    observe_start.recv_timeout(Duration::from_secs(5)).unwrap();
    let early = observe_finish
        .recv_timeout(Duration::from_millis(30))
        .is_ok();
    release.send(()).unwrap();
    reaper.join().unwrap();
    if !early {
        observe_finish.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    worker.join().unwrap();
    assert!(!alive(pid), "the child was reaped before returning");
    assert!(
        !early,
        "{wait:?} returned before {claim:?} reaped its child"
    );
}
