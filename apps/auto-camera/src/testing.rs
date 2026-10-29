//! Helpers shared by the app's tests: whether a tool such as ffmpeg is
//! installed (CI requires it, so its tests run rather than skip there) and
//! fresh temporary paths.

use std::path::PathBuf;

/// True when `tool -version` succeeds. When it does not and
/// `AC_REQUIRE_FFMPEG` is set (CI), panics instead of letting the caller
/// skip.
pub fn have(tool: &str) -> bool {
    let found = std::process::Command::new(tool)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let required = std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
    assert!(
        found || !required,
        "{tool} is not installed, and AC_REQUIRE_FFMPEG says it must be"
    );
    found
}

/// A fresh, unique path in the temp directory (not created).
pub fn temp_path(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("auto-camera-{}-{nanos}-{name}", std::process::id()))
}
