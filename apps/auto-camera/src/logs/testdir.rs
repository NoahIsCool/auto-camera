//! Temporary folders for the log module's tests: every test that writes a
//! file writes it into one of these, never into the user's folders.

use std::path::{Path, PathBuf};

/// A fresh folder under the system's temporary directory, removed with
/// everything in it when dropped.
#[derive(Debug)]
pub struct TempDir {
    /// The folder.
    path: PathBuf,
}

impl TempDir {
    /// Creates a folder whose name contains `name`, the process id and the
    /// time, so tests running at once never share one.
    pub fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "auto-camera-logs-{}-{nanos}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("a temporary folder");
        Self { path }
    }

    /// The folder.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
