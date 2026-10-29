//! A detector sidecar that dies is restarted, and the engine's snapshot
//! says so: its host status (`EngineSnapshot::sidecar`) counts the restart
//! and the failure, names the new process, and stays in the final snapshot
//! with the host stopped.
//!
//! The studio's detector is this binary as a fake sidecar. The test kills
//! the fake with `SIGKILL`, as a crash or an out-of-memory kill would; the
//! host sees its output end on the next pass, reaps it, waits out its 1 s
//! backoff and starts a new one, whose hello makes it ready again. Like the
//! shutdown test, the binary runs without libtest's harness so that it can
//! be the fake too (`common::one_test`); processes are checked with
//! `kill -0`, so it runs on every Unix.

#[cfg(unix)]
mod common;

/// The one test's name, for filters and `--list`.
#[cfg(unix)]
const NAME: &str = "killing_the_sidecar_raises_the_restart_count_in_the_snapshot";

/// Runs the test, or the fake sidecar; see `common::one_test`.
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    common::one_test::run(
        NAME,
        unix::killing_the_sidecar_raises_the_restart_count_in_the_snapshot,
    )
}

/// Nothing to run without Unix processes and signals.
#[cfg(not(unix))]
fn main() {}

/// The test.
#[cfg(unix)]
mod unix {
    use std::process::{Command, Stdio};

    use ac_engine::{DetectorSettings, EngineSnapshot, SidecarSettings};
    use ac_sidecar::{HostState, HostStatus};

    use crate::common::one_test::{FAKE_FLAG, FAKE_MODEL};
    use crate::common::{WAIT, start, studio};

    /// True while process `pid` exists (zombies included).
    fn exists(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// The snapshot's sidecar status; the studio has a sidecar.
    fn sidecar(s: &EngineSnapshot) -> &HostStatus {
        s.sidecar
            .as_ref()
            .expect("the studio's detector is a sidecar")
    }

    /// Detector passes applied on the studio's one source.
    fn applied(s: &EngineSnapshot) -> u64 {
        s.sources[0].discovery.applied
    }

    /// The test; see the crate docs.
    pub fn killing_the_sidecar_raises_the_restart_count_in_the_snapshot() {
        let mut config = studio(1);
        let me = std::env::current_exe().expect("this test binary");
        config.detector = DetectorSettings::Sidecar(SidecarSettings::new([
            me.to_str().expect("a UTF-8 path").to_owned(),
            FAKE_FLAG.to_owned(),
        ]));
        let engine = start(config);
        let h = engine.handle();
        let snap = h
            .wait_until(WAIT, |s| {
                sidecar(s).state == HostState::Ready && applied(s) >= 2
            })
            .expect("the sidecar answers");
        let status = sidecar(&snap);
        let first = status.pid.expect("a running sidecar");
        assert_eq!(status.hello.as_ref().unwrap().model, FAKE_MODEL);
        assert_eq!(
            (status.stats.spawns, status.stats.restarts()),
            (1, 0),
            "{status:?}"
        );
        assert_eq!(status.stats.failures, 0);

        let killed = Command::new("kill")
            .args(["-KILL", &first.to_string()])
            .status()
            .expect("kill runs");
        assert!(killed.success());
        let before = applied(&snap);
        let snap = h
            .wait_until(WAIT, |s| {
                let status = sidecar(s);
                status.stats.restarts() >= 1
                    && status.state == HostState::Ready
                    && applied(s) > before + 1
            })
            .expect("the sidecar is restarted and answers again");
        let status = sidecar(&snap);
        let second = status.pid.expect("the new sidecar");
        assert_ne!(second, first, "a new process");
        assert!(status.stats.failures >= 1, "{status:?}");
        assert!(!exists(first), "the killed sidecar was reaped");
        let errors = snap.sources[0].discovery.errors;
        assert!(errors >= 1, "the pass that met the dead sidecar failed");

        let report = engine.shutdown().expect("clean shutdown");
        let last = sidecar(&report.final_snapshot);
        assert_eq!((&last.state, last.pid), (&HostState::Stopped, None));
        assert!(last.stats.restarts() >= 1, "the count survives shutdown");
        assert_eq!(last.stats.restarts(), last.stats.spawns - 1);
        assert!(!exists(second), "shutdown ended the new sidecar");
    }
}
