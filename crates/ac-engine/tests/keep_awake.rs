//! The engine holds the machine awake while output runs: a stand-in
//! `caffeinate` (a shell, so Unix only) starts with the output session, is
//! given the display flag because the studio's monitors show program, ends
//! with the session and is reaped, also when the engine shuts down with the
//! output running. Skipped when ffmpeg is not installed.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ac_core::ShotId;
use ac_engine::{Engine, EngineCommand, EngineHandle, EngineOptions, KeepAwakeState, OutputState};
use ac_output::{AwakeMode, Caffeinate, caffeinate_args};

use common::{WAIT, fast_time, have, studio, temp_path};

/// A stand-in that writes its arguments, one per line, to `record`
/// (atomically), then waits to be killed.
fn stand_in(record: &Path) -> Caffeinate {
    let script = r#"printf '%s\n' "$@" > "$0.tmp" && mv "$0.tmp" "$0"; exec sleep 60"#;
    Caffeinate::command("/bin/sh", ["-c", script, record.to_str().unwrap()])
}

/// True while process `pid` exists, a zombie included.
fn exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Starts the studio with `record`'s stand-in, puts the wide on program and
/// starts recording to a fresh path; returns the engine and that path once
/// the output runs, after checking that nothing was held before.
fn start_recording(record: &Path) -> (Engine, PathBuf) {
    let engine = Engine::start_with(
        studio(1),
        EngineOptions {
            keep_awake: stand_in(record),
            ..fast_time()
        },
    )
    .expect("engine starts");
    let h = engine.handle();
    let wide = ShotId("stage/wide".into());
    let snap = h
        .wait_until(WAIT, |s| s.shot(&wide).is_some_and(|v| v.available))
        .expect("the source delivers frames");
    assert_eq!(snap.output.keep_awake, KeepAwakeState::Idle);
    assert!(!record.exists(), "held before the output started");
    h.send(EngineCommand::SetPreview { shot: wide }).unwrap();
    h.send(EngineCommand::Cut).unwrap();
    let path = temp_path("awake.mp4");
    h.send(EngineCommand::StartOutput {
        rtmp_url: None,
        record_path: Some(path.clone()),
    })
    .unwrap();
    h.wait_until(WAIT, |s| s.output.state == OutputState::Running)
        .expect("the encoder starts");
    (engine, path)
}

/// The process id the newest snapshot says is held.
fn held(h: &EngineHandle) -> u32 {
    match h.snapshot().output.keep_awake.clone() {
        KeepAwakeState::Held(pid) => pid,
        other => panic!("not held while the output runs: {other:?}"),
    }
}

/// The arguments the stand-in wrote to `record`, once it has.
fn recorded(record: &Path) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !record.exists() {
        assert!(Instant::now() < deadline, "the stand-in never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
    let text = std::fs::read_to_string(record).unwrap();
    text.lines().map(String::from).collect()
}

#[test]
fn the_hold_starts_with_the_output_and_ends_with_it() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let record = temp_path("caffeinate-args");
    let (engine, path) = start_recording(&record);
    let h = engine.handle();
    let pid = held(&h);
    assert!(exists(pid));
    // The studio's monitors show program, so the display stays awake too.
    assert_eq!(
        recorded(&record),
        caffeinate_args(AwakeMode::SystemAndDisplay, std::process::id())
    );
    h.send(EngineCommand::StopOutput).unwrap();
    let done = h
        .wait_until(WAIT, |s| {
            !matches!(s.output.state, OutputState::Running | OutputState::Stopping)
        })
        .expect("the encoder finishes");
    assert_eq!(
        done.output.state,
        OutputState::Finished,
        "{:?}",
        done.output
    );
    assert_eq!(done.output.keep_awake, KeepAwakeState::Idle);
    assert!(!exists(pid), "{pid} is left behind");
    engine.shutdown().unwrap();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&record);
}

#[test]
fn shutting_down_with_the_output_running_ends_the_hold() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let record = temp_path("caffeinate-args");
    let (engine, path) = start_recording(&record);
    let pid = held(&engine.handle());
    let report = engine.shutdown().unwrap();
    let output = &report.final_snapshot.output;
    assert_eq!(output.state, OutputState::Finished, "{output:?}");
    assert_eq!(output.keep_awake, KeepAwakeState::Idle);
    assert!(!exists(pid), "{pid} is left behind");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&record);
}
