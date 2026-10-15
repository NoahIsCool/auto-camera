//! Helpers shared by the integration tests: scratch directories, process
//! checks, synthetic media, ffprobe queries and a retrying `start`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use ac_output::{OutputConfig, OutputError, ProgramEncoder};

/// A fresh, empty directory under Cargo's per-target scratch space.
pub fn scratch_dir(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("ac-output-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// True while a process with this id exists (signal 0 probes without
/// delivering anything; a reaped child no longer exists).
pub fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `ProgramEncoder::start`, retried while the executable is briefly busy.
///
/// A script written by a test can be momentarily held open for writing by a
/// process another test thread is forking, which makes `exec` fail with
/// `ETXTBSY`; that is a property of the test harness, not of the encoder.
pub fn start(config: &OutputConfig) -> Result<ProgramEncoder, OutputError> {
    for _ in 0..50 {
        match ProgramEncoder::start(config.clone()) {
            Err(OutputError::Io(e)) if e.kind() == io::ErrorKind::ExecutableFileBusy => {
                thread::sleep(Duration::from_millis(20));
            }
            other => return other,
        }
    }
    ProgramEncoder::start(config.clone())
}

/// A moving gradient so consecutive frames differ.
pub fn frame(width: u32, height: u32, index: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[(x + index * 4) as u8, (y * 2) as u8, (index * 8) as u8, 255]);
        }
    }
    data
}

/// `seconds` of an interleaved sine at `hz`, amplitude 0.5.
pub fn sine(sample_rate: u32, channels: u16, hz: f32, seconds: f32) -> Vec<f32> {
    let frames = (sample_rate as f32 * seconds) as usize;
    let mut out = Vec::with_capacity(frames * channels as usize);
    for n in 0..frames {
        let v = 0.5 * (std::f32::consts::TAU * hz * n as f32 / sample_rate as f32).sin();
        out.extend(std::iter::repeat_n(v, channels as usize));
    }
    out
}

/// True when ffmpeg (with libx264) and ffprobe are on the `PATH`; prints a
/// skip notice otherwise, or panics when `AC_REQUIRE_FFMPEG` is set (CI), so
/// a runner without the tools cannot pass by skipping.
pub fn have_ffmpeg() -> bool {
    let ffprobe = Command::new("ffprobe")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    let x264 = ac_output::detect_encoder(Path::new("ffmpeg"), ac_output::Encoder::Libx264).is_ok();
    let required = std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
    if !(ffprobe && x264) {
        assert!(
            !required,
            "ffmpeg with libx264 and ffprobe are required (AC_REQUIRE_FFMPEG)"
        );
        eprintln!("skipping: needs ffmpeg with libx264 and ffprobe on the PATH");
    }
    ffprobe && x264
}

/// Runs ffprobe on `path` and returns the `key=value` lines it prints.
pub fn probe(path: &Path, args: &[&str]) -> HashMap<String, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(args)
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "ffprobe failed: {out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// A numeric ffprobe field.
pub fn num(map: &HashMap<String, String>, key: &str) -> f64 {
    map.get(key)
        .unwrap_or_else(|| panic!("ffprobe gave no {key}: {map:?}"))
        .parse()
        .unwrap_or_else(|_| panic!("{key} is not a number: {map:?}"))
}
