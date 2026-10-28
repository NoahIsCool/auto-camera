//! A run keeps a log: the built binary, run headless for a second on the
//! demo studio (synthetic, no camera, model or network) with `--log-dir`
//! set to a temporary folder, must leave a log there that says which build
//! ran which config, what the studio did and that the app exited, and no
//! crash report.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The app built with these tests.
const APP: &str = env!("CARGO_BIN_EXE_auto-camera");

/// The demo studio, from the repository root.
const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/demo.toml");

/// A fresh folder under the temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    /// Creates a folder whose name contains `name`, the process id and the
    /// time.
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "auto-camera-log-test-{}-{nanos}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The files in `dir` whose names start with `prefix`.
fn named(dir: &Path, prefix: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix))
        .collect()
}

#[test]
fn a_headless_run_leaves_a_log_that_says_what_ran() {
    let dir = TempDir::new("run");
    // The folder is created when missing.
    let logs = dir.0.join("logs");
    let out = Command::new(APP)
        .args([
            "--config",
            DEMO,
            "--headless",
            "--seconds",
            "1",
            "--log-dir",
        ])
        .arg(&logs)
        .stdin(Stdio::null())
        .output()
        .expect("the app runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let log = std::fs::read_to_string(logs.join("auto-camera.log")).expect("a log");
    let lines: Vec<&str> = log.lines().collect();
    // Every line: RFC 3339 UTC time, level, target, thread, message.
    for line in &lines {
        let time = line.split(' ').next().unwrap();
        assert!(time.len() == 24 && time.ends_with('Z'), "{line}");
    }
    let has = |level: &str, target: &str, text: &str| {
        lines
            .iter()
            .any(|l| l.contains(&format!(" {level:<5} {target} [")) && l.contains(text))
    };
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        has(
            "INFO",
            "auto_camera::logs",
            &format!("auto-camera {version} (git ")
        ),
        "{log}"
    );
    assert!(
        lines[0].contains(" starting: pid "),
        "the header comes first: {log}"
    );
    let config = std::fs::read(DEMO).unwrap();
    let sha: String = hmac_sha256::Hash::hash(&config)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(
        has(
            "INFO",
            "auto_camera::logs",
            &format!("demo.toml: sha256 {sha}, 1 sources")
        ),
        "{log}"
    );
    assert!(
        has("INFO", "auto_camera::engine", "engine running: 1 sources"),
        "{log}"
    );
    assert!(
        has(
            "INFO",
            "auto_camera::engine",
            "source stage (Stage): 1280x720"
        ),
        "{log}"
    );
    assert!(has("INFO", "auto_camera::engine", "program: "), "{log}");
    assert!(
        has("INFO", "auto_camera::engine", "engine stopped after "),
        "{log}"
    );
    assert!(has("INFO", "auto_camera", "headless run done"), "{log}");
    assert!(
        lines.last().unwrap().ends_with("auto-camera exiting"),
        "the last line: {log}"
    );
    assert!(!log.contains(" ERROR "), "{log}");
    assert!(named(&logs, "crash-").is_empty());
}
