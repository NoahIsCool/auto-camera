//! SHA-256 of the config and the model files, for the log.
//!
//! # Theory
//!
//! A log is evidence when a show went wrong, so it says what ran: the
//! build (version and commit, in the header), the config file and the
//! model files. The config is a few kilobytes and is hashed at once. Model
//! files can be gigabytes, so they are left to one background thread
//! ([`Hasher`], `ac-log-hash`): it first logs each file's path, size and
//! modification time (enough to tell two files apart in practice), then
//! hashes the files one after another and logs each SHA-256 as it is
//! done. Nothing waits for it: the show starts at once, and a file still
//! being hashed when the app closes is logged as not hashed. Whether the
//! weights are the expected ones is the sidecar's check (its hello's
//! `weights_sha256`); these hashes only record what was there.
//!
//! The model files are the OWL detector's model, text encoder and
//! tokenizer, and for a sidecar every path in its command after the
//! program (the server script, the model folder): a folder stands for the
//! files in it, in name order, hidden ones skipped, at most [`MAX_DEPTH`]
//! levels down and [`MAX_FILES`] files in all.
//!
//! # Invariants
//!
//! * The thread reads one [`CHUNK`] at a time into one buffer, and checks
//!   its stop flag between chunks, so [`Hasher::stop`] returns within one
//!   chunk's read.
//! * At most [`MAX_FILES`] files are looked at; more are noted, not read.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use ac_engine::DetectorSettings;
use tracing::Level;

use super::time::Utc;

/// Bytes read at a time.
pub const CHUNK: usize = 1 << 20;

/// Files looked at, at most.
pub const MAX_FILES: usize = 64;

/// Levels of folders below a listed folder that are looked into.
pub const MAX_DEPTH: usize = 3;

/// The target of the hasher's events.
const TARGET: &str = "auto_camera::logs";

/// SHA-256 of `bytes` as 64 lower-case hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&hmac_sha256::Hash::hash(bytes))
}

/// `bytes` as lower-case hex digits.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of the file at `path`, read [`CHUNK`] by chunk into `buf`;
/// `None` when `stop` was set before the end.
fn hash_file(path: &Path, buf: &mut [u8], stop: &AtomicBool) -> io::Result<Option<String>> {
    let mut file = File::open(path)?;
    let mut hash = hmac_sha256::Hash::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = match file.read(buf) {
            Ok(0) => return Ok(Some(hex(&hash.finalize()))),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hash.update(&buf[..n]);
    }
}

/// The paths of `detector`'s model files, as configured (a folder stands
/// for its files; see the module docs).
pub fn model_paths(detector: &DetectorSettings) -> Vec<PathBuf> {
    match detector {
        DetectorSettings::Owl {
            model,
            text_encoder,
            tokenizer,
            ..
        } => std::iter::once(model.clone())
            .chain(text_encoder.clone())
            .chain(std::iter::once(tokenizer.clone()))
            .collect(),
        DetectorSettings::Sidecar(s) => s
            .path_args()
            .filter(|(i, _)| *i > 0)
            .map(|(_, p)| PathBuf::from(p))
            .collect(),
        _ => Vec::new(),
    }
}

/// The files `paths` stand for, at most [`MAX_FILES`], and whether there
/// were more. A path that is neither a folder nor readable is kept, so its
/// error is logged.
fn files(paths: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let mut out = Vec::new();
    let mut more = false;
    for path in paths {
        if path.is_dir() {
            more |= walk(path, 0, &mut out);
        } else if out.len() < MAX_FILES {
            out.push(path.clone());
        } else {
            more = true;
        }
    }
    (out, more)
}

/// Adds the files in folder `dir` (`depth` levels below a listed one) to
/// `out`, in name order; true when some did not fit.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    paths.sort();
    let mut more = false;
    for path in paths {
        if path.is_dir() {
            if depth < MAX_DEPTH {
                more |= walk(&path, depth + 1, out);
            }
        } else if out.len() < MAX_FILES {
            out.push(path);
        } else {
            return true;
        }
    }
    more
}

/// `path`'s size and modification time, or why they cannot be read.
fn describe(path: &Path) -> (Level, String) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .map_or_else(|_| "unknown".to_owned(), |t| Utc::at(t).rfc3339());
            (
                Level::INFO,
                format!(
                    "model file {}: {} bytes, modified {modified}",
                    path.display(),
                    meta.len()
                ),
            )
        }
        Err(e) => (
            Level::WARN,
            format!("model file {}: cannot read: {e}", path.display()),
        ),
    }
}

/// Logs `text` at `level` under the hasher's target, as a header line
/// (see the `writer` module): what the model files were is part of what
/// ran.
pub fn log(level: Level, text: &str) {
    if level == Level::WARN {
        tracing::warn!(target: TARGET, log_header = true, "{text}");
    } else {
        tracing::info!(target: TARGET, log_header = true, "{text}");
    }
}

/// The background thread that hashes model files; see the module docs.
#[derive(Debug)]
pub struct Hasher {
    /// Set to stop.
    stop: Arc<AtomicBool>,
    /// The thread, until joined.
    thread: Option<JoinHandle<()>>,
}

impl Hasher {
    /// Starts hashing the files `paths` stand for, giving every line to
    /// `emit`; `None` when the thread cannot be started (which is logged).
    pub fn start(
        paths: Vec<PathBuf>,
        mut emit: impl FnMut(Level, &str) + Send + 'static,
    ) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ac-log-hash".into())
            .spawn(move || run(&paths, &flag, &mut emit));
        match thread {
            Ok(thread) => Some(Self {
                stop,
                thread: Some(thread),
            }),
            Err(e) => {
                log(Level::WARN, &format!("model files not hashed: {e}"));
                None
            }
        }
    }

    /// Stops hashing and joins the thread. Idempotent.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// True once the thread has ended (tests wait for it).
    #[cfg(test)]
    fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Hasher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The thread: describes every file, then hashes each in turn.
fn run(paths: &[PathBuf], stop: &AtomicBool, emit: &mut impl FnMut(Level, &str)) {
    let (files, more) = files(paths);
    for file in &files {
        let (level, text) = describe(file);
        emit(level, &text);
    }
    if more {
        emit(
            Level::INFO,
            &format!("model files: only the first {MAX_FILES} are listed and hashed"),
        );
    }
    let mut buf = vec![0; CHUNK];
    for file in &files {
        let started = Instant::now();
        match hash_file(file, &mut buf, stop) {
            Ok(Some(sha)) => emit(
                Level::INFO,
                &format!(
                    "model file {}: sha256 {sha} ({:.1} s)",
                    file.display(),
                    started.elapsed().as_secs_f64()
                ),
            ),
            Ok(None) => {
                emit(
                    Level::INFO,
                    &format!(
                        "model file {}: not hashed, the app is closing",
                        file.display()
                    ),
                );
                return;
            }
            Err(e) => emit(
                Level::WARN,
                &format!("model file {}: cannot hash: {e}", file.display()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use ac_engine::SidecarSettings;

    use super::*;
    use crate::logs::testdir::TempDir;

    /// SHA-256 of "abc" (FIPS 180-2, appendix B.1).
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn bytes_and_files_hash_to_the_standard_digest() {
        assert_eq!(sha256_hex(b"abc"), ABC);
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let dir = TempDir::new("hash");
        let path = dir.path().join("abc.bin");
        std::fs::write(&path, b"abc").unwrap();
        let stop = AtomicBool::new(false);
        // A buffer of one byte reads the file in three chunks.
        let mut buf = [0; 1];
        assert_eq!(hash_file(&path, &mut buf, &stop).unwrap().unwrap(), ABC);
        stop.store(true, Ordering::Relaxed);
        assert_eq!(hash_file(&path, &mut buf, &stop).unwrap(), None);
        assert!(hash_file(&dir.path().join("missing"), &mut buf, &stop).is_err());
    }

    #[test]
    fn the_model_paths_of_each_detector() {
        let owl = DetectorSettings::Owl {
            model: "/m/image.onnx".into(),
            text_encoder: Some("/m/text.onnx".into()),
            tokenizer: "/m/tokenizer.json".into(),
            input_size: None,
            score_threshold: None,
            use_coreml: None,
            people_first: None,
            person_threshold: None,
        };
        assert_eq!(
            model_paths(&owl),
            [
                PathBuf::from("/m/image.onnx"),
                "/m/text.onnx".into(),
                "/m/tokenizer.json".into()
            ]
        );
        let sidecar = DetectorSettings::Sidecar(SidecarSettings::new([
            "/venv/bin/python",
            "/sidecars/server.py",
            "--model",
            "/models/sam31-mlx",
            "--side",
            "672",
        ]));
        assert_eq!(
            model_paths(&sidecar),
            [
                PathBuf::from("/sidecars/server.py"),
                "/models/sam31-mlx".into()
            ],
            "the program is not a model file"
        );
        assert!(model_paths(&DetectorSettings::None).is_empty());
        assert!(model_paths(&DetectorSettings::Synthetic { score: 0.9 }).is_empty());
    }

    #[test]
    fn folders_stand_for_their_files_in_order_within_bounds() {
        let dir = TempDir::new("walk");
        let root = dir.path();
        std::fs::create_dir_all(root.join("model/sub/a/b/c/d")).unwrap();
        for f in [
            "model/b.bin",
            "model/a.json",
            "model/.hidden",
            "model/sub/w.bin",
        ] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        // Three levels below "model" are looked into; the fourth is not.
        std::fs::write(root.join("model/sub/a/b/deep.bin"), b"x").unwrap();
        std::fs::write(root.join("model/sub/a/b/c/too-deep.bin"), b"x").unwrap();
        let single = root.join("server.py");
        std::fs::write(&single, b"print()").unwrap();
        let missing = root.join("missing.bin");
        let (found, more) = files(&[single.clone(), root.join("model"), missing.clone()]);
        assert!(!more);
        assert_eq!(
            found,
            [
                single,
                root.join("model/a.json"),
                root.join("model/b.bin"),
                root.join("model/sub/a/b/deep.bin"),
                root.join("model/sub/w.bin"),
                missing
            ]
        );
        let many = root.join("many");
        std::fs::create_dir(&many).unwrap();
        for i in 0..MAX_FILES + 3 {
            std::fs::write(many.join(format!("{i:03}.bin")), b"x").unwrap();
        }
        let (found, more) = files(&[many]);
        assert_eq!(found.len(), MAX_FILES);
        assert!(more);
    }

    /// The lines a hasher on `paths` emits until it ends.
    fn hashed(paths: Vec<PathBuf>) -> Vec<(Level, String)> {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let hasher = Hasher::start(paths, move |level, text: &str| {
            sink.lock().unwrap().push((level, text.to_owned()));
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !hasher.finished() {
            assert!(Instant::now() < deadline, "the hasher did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(hasher);
        Arc::try_unwrap(lines).unwrap().into_inner().unwrap()
    }

    #[test]
    fn the_hasher_lists_then_hashes_and_reports_unreadable_files() {
        let dir = TempDir::new("hasher");
        let file = dir.path().join("weights.bin");
        std::fs::write(&file, b"abc").unwrap();
        let missing = dir.path().join("missing.bin");
        let lines = hashed(vec![file.clone(), missing.clone()]);
        assert_eq!(lines.len(), 4, "{lines:?}");
        let (level, text) = &lines[0];
        assert_eq!(*level, Level::INFO);
        assert!(
            text.starts_with(&format!(
                "model file {}: 3 bytes, modified 20",
                file.display()
            )),
            "{text}"
        );
        assert_eq!(lines[1].0, Level::WARN);
        assert!(
            lines[1].1.contains("missing.bin: cannot read"),
            "{}",
            lines[1].1
        );
        assert!(
            lines[2]
                .1
                .starts_with(&format!("model file {}: sha256 {ABC} (", file.display())),
            "{}",
            lines[2].1
        );
        assert!(
            lines[3].1.contains("missing.bin: cannot hash"),
            "{}",
            lines[3].1
        );
        assert!(hashed(Vec::new()).is_empty());
    }

    #[test]
    fn a_stopped_hasher_says_what_it_did_not_hash() {
        let dir = TempDir::new("stopped");
        let file = dir.path().join("big.bin");
        std::fs::write(&file, vec![7; 3 * CHUNK]).unwrap();
        let stop = AtomicBool::new(true);
        let mut lines = Vec::new();
        run(&[file], &stop, &mut |l, t: &str| {
            lines.push((l, t.to_owned()))
        });
        assert_eq!(lines.len(), 2);
        assert!(lines[1].1.ends_with("not hashed, the app is closing"));
        // Stopping a running hasher joins it; again is a no-op.
        let mut h = Hasher::start(vec![dir.path().join("big.bin")], |_, _: &str| {}).unwrap();
        h.stop();
        h.stop();
        assert!(h.finished());
    }
}
