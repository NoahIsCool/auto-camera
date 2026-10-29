//! Helpers shared by the integration tests: the ffmpeg tools (or a skip),
//! scratch folders, generated clips and polling.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ac_sources::FfmpegTools;

/// The tools, or `None` (and a note) when ffmpeg is not installed; panics
/// instead when `AC_REQUIRE_FFMPEG` is set.
pub fn tools() -> Option<FfmpegTools> {
    let tools = FfmpegTools::from_env();
    if tools.is_available() {
        Some(tools)
    } else {
        let required =
            std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
        assert!(!required, "ffmpeg/ffprobe are required (AC_REQUIRE_FFMPEG)");
        eprintln!("skipping: ffmpeg/ffprobe not found");
        None
    }
}

/// A temporary directory removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    /// A fresh directory unique to this process and `name`.
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ac-sources-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a 160x120 30 fps `testsrc` clip of `video_secs`, with a stereo
/// 48 kHz 440 Hz tone of `audio_secs` when given.
pub fn make_clip(
    tools: &FfmpegTools,
    dir: &Path,
    video_secs: f64,
    audio_secs: Option<f64>,
) -> PathBuf {
    let path = dir.join("clip.mkv");
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!(
            "testsrc=size=160x120:rate=30:duration={video_secs}"
        ));
    if let Some(a) = audio_secs {
        cmd.args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:sample_rate=48000:duration={a}"))
            .args(["-ac", "2", "-c:a", "pcm_s16le"]);
    }
    cmd.args(["-c:v", "mpeg4", "-q:v", "4"]).arg(&path);
    assert!(
        cmd.status().unwrap().success(),
        "ffmpeg could not make a clip"
    );
    path
}

/// Polls `step` every 2 ms until it returns true or `timeout` passes.
pub fn poll(timeout: Duration, mut step: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if step() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
