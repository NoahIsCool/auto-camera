//! A termination signal shuts the app down cleanly: the built binary, run
//! headless on the demo studio (synthetic, no camera, model or network),
//! gets `SIGINT` or `SIGTERM` and must exit with status 130 after printing
//! its summary, which says it was interrupted, and its log (in a
//! temporary folder) says so too. Unix only: the signals are sent with
//! `kill`.

#![cfg(unix)]

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The app built with these tests.
const APP: &str = env!("CARGO_BIN_EXE_auto-camera");

/// The demo studio, from the repository root.
const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/demo.toml");

/// Waits up to `limit` for `child` to exit; kills it if it does not.
fn wait(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs a 30 s headless demo, sends `signal` after it has started, and
/// checks the exit status and summary.
fn interrupted_by(signal: &str) {
    let logs = std::env::temp_dir().join(format!(
        "auto-camera-signals-{}{signal}",
        std::process::id()
    ));
    let mut child = Command::new(APP)
        .args([
            "--config",
            DEMO,
            "--headless",
            "--seconds",
            "30",
            "--log-dir",
        ])
        .arg(&logs)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the app starts");
    std::thread::sleep(Duration::from_millis(1500));
    let sent = Instant::now();
    let kill = Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(kill.success(), "{signal}");
    let status = wait(&mut child, Duration::from_secs(20)).expect("the app exits");
    let mut summary = String::new();
    let mut log = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut summary)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut log)
        .unwrap();
    assert_eq!(status.code(), Some(130), "{signal}: {log}");
    assert!(
        sent.elapsed() < Duration::from_secs(15),
        "{signal}: slow exit"
    );
    assert!(
        summary.contains("\"error\": \"interrupted by a termination signal\""),
        "{signal}: {summary}"
    );
    assert!(log.contains("termination signal, shutting down"), "{log}");
    let file = std::fs::read_to_string(logs.join("auto-camera.log")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&logs);
    assert!(
        file.contains("headless run failed: interrupted by a termination signal"),
        "{signal}: {file}"
    );
}

#[test]
fn ctrl_c_and_sigterm_run_the_normal_shutdown() {
    interrupted_by("-INT");
    interrupted_by("-TERM");
}
