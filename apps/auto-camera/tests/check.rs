//! `--check` on the built binary: every example config passes on a machine
//! made of fake files laid out as on Noah's Mac (an ffmpeg and ffprobe that
//! only print a version, empty model files, a venv whose python would
//! leave a mark if it ever ran), a config with three problems lists all
//! three, also when they are in one table, the `schema` key is checked, and
//! the default config path gets a hint only when nothing is there. Nothing
//! here needs a camera, a model, a network or a real ffmpeg. Unix only: the
//! fakes are shell scripts.
//!
//! The app runs at the tree's root, as the examples say to run it from the
//! repository root. A relative recording path is taken from the working
//! folder (the config's other paths from the config's folder), so the
//! examples' `recordings/...` then names the tree's `recordings` folder.
//!
//! Writing an executable while another thread forks can leave the child a
//! writable copy of it, and executing it then fails ("text file busy"), so
//! every script is written, and every process spawned, under one lock.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The app built with these tests.
const APP: &str = env!("CARGO_BIN_EXE_auto-camera");

/// The repository's example configs.
const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config");

/// Held while writing an executable or spawning a process; see the module
/// docs.
static FORK: Mutex<()> = Mutex::new(());

/// A scratch studio tree, removed when dropped.
struct Studio {
    /// Its root, standing for the repository root.
    root: PathBuf,
}

impl Drop for Studio {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Studio {
    /// A tree with every file the example configs name, as fakes.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ac-check-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let studio = Self { root };
        for dir in [
            "config",
            "bin",
            "recordings",
            "python-home",
            "models/sam31-mlx",
        ] {
            fs::create_dir_all(studio.path(dir)).unwrap();
        }
        for file in [
            "media/rehearsal.mp4",
            "models/owlv2-split-onnx/detector.onnx",
            "models/owlv2-split-onnx/text_encoder.onnx",
            "models/owlv2-split-onnx/tokenizer.json",
            "sidecars/sam3-mlx/server.py",
        ] {
            studio.write(file, "fake\n");
        }
        studio.write(
            "models/sam3/.venv/pyvenv.cfg",
            &format!(
                "home = {}\nversion = 3.12.4\n",
                studio.path("python-home").display()
            ),
        );
        let log = studio.path("tools.log");
        for tool in ["ffmpeg", "ffprobe"] {
            studio.script(
                &format!("bin/{tool}"),
                &format!(
                    "echo \"$@\" >> '{}'\necho '{tool} version 0.0-fake Copyright (c) nobody'\n",
                    log.display()
                ),
            );
        }
        let mark = studio.path("sidecar-ran");
        studio.script(
            "models/sam3/.venv/bin/python",
            &format!("touch '{}'\nexit 1\n", mark.display()),
        );
        studio
    }

    /// `rel` under the root.
    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Writes `text` to `rel`, making its folder.
    fn write(&self, rel: &str, text: &str) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Writes an executable shell script with `body` to `rel`.
    fn script(&self, rel: &str, body: &str) {
        let _fork = FORK.lock().unwrap_or_else(|e| e.into_inner());
        self.write(rel, &format!("#!/bin/sh\n{body}"));
        fs::set_permissions(self.path(rel), fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Copies the example config `name` into the tree's `config` folder.
    fn example(&self, name: &str) -> PathBuf {
        let to = self.path(&format!("config/{name}"));
        fs::copy(Path::new(EXAMPLES).join(name), &to).unwrap();
        to
    }

    /// Runs the app with `args` at the tree's root, its tools being the
    /// tree's fakes; waits up to a minute and kills it after that.
    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &self.path("bin/ffmpeg"))
    }

    /// [`Self::run`] with `ffmpeg` as the ffmpeg (`AC_FFMPEG`).
    fn run_with(&self, args: &[&str], ffmpeg: &Path) -> Output {
        let child = {
            let _fork = FORK.lock().unwrap_or_else(|e| e.into_inner());
            Command::new(APP)
                .args(args)
                .current_dir(&self.root)
                .env("AC_FFMPEG", ffmpeg)
                .env("AC_FFPROBE", self.path("bin/ffprobe"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the app starts")
        };
        let id = child.id();
        let deadline = Instant::now() + Duration::from_secs(60);
        let waiter = std::thread::spawn(move || child.wait_with_output().unwrap());
        while !waiter.is_finished() {
            if Instant::now() > deadline {
                let _ = Command::new("kill").args(["-9", &id.to_string()]).status();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        waiter.join().unwrap()
    }

    /// `--check` on `config`: the exit status and stdout.
    fn check(&self, config: &Path) -> (Option<i32>, String) {
        stdout_of(self.run(&["--check", "--config", config.to_str().unwrap()]))
    }

    /// What the fake tools were asked, one line per run.
    fn tool_calls(&self) -> Vec<String> {
        fs::read_to_string(self.path("tools.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The exit status and stdout of a `--check` run, which prints nothing on
/// stderr.
fn stdout_of(out: Output) -> (Option<i32>, String) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.is_empty(), "nothing on stderr: {stderr}");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The problem lines of a report.
fn problems(report: &str) -> Vec<&str> {
    report
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("problem"))
        .map(str::trim)
        .collect()
}

#[test]
fn every_example_config_is_ready_on_a_machine_with_its_files() {
    let studio = Studio::new("examples");
    for name in [
        "demo.toml",
        "file-source.example.toml",
        "sam3-sidecar.example.toml",
    ] {
        let (code, report) = studio.check(&studio.example(name));
        if name == "file-source.example.toml" && !cfg!(feature = "onnx") {
            // The OWL detector is in the build only with `--features onnx`,
            // and without it the studio would not start: that is the one
            // problem.
            assert_eq!(code, Some(2), "{name}: {report}");
            let [only] = problems(&report)[..] else {
                panic!("{name}: {report}");
            };
            assert!(only.contains("built with `--features onnx`"), "{only}");
            continue;
        }
        assert_eq!(code, Some(0), "{name}: {report}");
        assert!(report.ends_with("Ready: nothing found that would stop this studio.\n"));
        assert!(!report.contains("warning"), "{name}: {report}");
        assert!(
            report.contains("ffmpeg version 0.0-fake"),
            "{name}: {report}"
        );
    }
    let calls = studio.tool_calls();
    assert!(!calls.is_empty());
    assert!(
        calls.iter().all(|c| c == "-version"),
        "the tools are only asked their version: {calls:?}"
    );
    assert!(
        !studio.path("sidecar-ran").exists(),
        "the sidecar's python was never run"
    );
    let entries = fs::read_dir(studio.path("recordings")).unwrap().count();
    assert_eq!(entries, 0, "the probe file is gone");
}

#[test]
fn a_config_with_three_problems_lists_all_three() {
    let studio = Studio::new("three");
    let config = studio.path("config/broken.toml");
    fs::write(
        &config,
        "schema = 1\n\n[[sources]]\nid = \"cam-1\"\nkind = \"file\"\n\
         path = \"../media/missing.mp4\"\n\n[output]\nkeyframe_seconds = 30\n\
         record_path = \"no-such-folder/show.mp4\"\n",
    )
    .unwrap();
    let (code, report) = studio.check(&config);
    assert_eq!(code, Some(2), "{report}");
    let found = problems(&report);
    assert_eq!(found.len(), 3, "{report}");
    assert_eq!(
        found[0],
        "config: output: keyframe_seconds = 30 is outside 1..=10"
    );
    assert!(
        found[1].starts_with("source \"cam-1\": ")
            && found[1].ends_with("missing.mp4 does not exist"),
        "{}",
        found[1]
    );
    assert!(
        found[2].contains("no-such-folder (from the working folder) does not exist"),
        "{}",
        found[2]
    );
    assert!(
        report.ends_with("3 problems found; fix them and check again.\n"),
        "{report}"
    );

    let one_table = studio.path("config/output.toml");
    fs::write(
        &one_table,
        "schema = 1\n\n[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 640\n\
         height = 360\nfps = 30\n\n[output]\nkeyframe_seconds = 30\nvideo_bitrate_kbps = 50\n\
         audio_bitrate_kbps = 1\n",
    )
    .unwrap();
    let (code, report) = studio.check(&one_table);
    assert_eq!(code, Some(2), "{report}");
    assert_eq!(
        problems(&report),
        [
            "config: output: video_bitrate_kbps = 50 is outside 100..=100000",
            "config: output: keyframe_seconds = 30 is outside 1..=10",
            "config: output: audio_bitrate_kbps = 1 is outside 32..=512",
        ],
        "all three, though they are in one table"
    );
}

#[test]
fn the_default_path_gets_a_hint_only_when_nothing_is_there() {
    let studio = Studio::new("hint");
    let hint = "(run from the repository root, or pass --config PATH)";
    let (code, report) = stdout_of(studio.run(&["--check"]));
    assert_eq!(code, Some(2), "{report}");
    assert!(
        problems(&report)[0].starts_with("cannot read config/demo.toml"),
        "{report}"
    );
    assert!(report.contains(hint), "{report}");
    studio.example("demo.toml");
    let missing = studio.path("bin/no-such-ffmpeg");
    let (code, report) = stdout_of(studio.run_with(&["--check"], &missing));
    assert_eq!(code, Some(2), "{report}");
    let [only] = problems(&report)[..] else {
        panic!("{report}");
    };
    assert!(only.starts_with("output.ffmpeg: "), "{only}");
    assert!(
        !report.contains(hint),
        "the default config loaded, so the path is not the problem: {report}"
    );
}

#[test]
fn the_schema_key_is_checked() {
    let studio = Studio::new("schema");
    let demo = fs::read_to_string(Path::new(EXAMPLES).join("demo.toml")).unwrap();
    assert!(demo.contains("\nschema = 1\n"));
    let old = studio.path("config/old.toml");
    fs::write(&old, demo.replace("\nschema = 1\n", "\n")).unwrap();
    let (code, report) = studio.check(&old);
    assert_eq!(code, Some(0), "a missing key is a warning: {report}");
    let warning = report
        .lines()
        .find(|l| l.trim_start().starts_with("warning"))
        .unwrap_or_else(|| panic!("{report}"));
    assert!(warning.contains("no `schema` key"), "{warning}");
    assert!(warning.contains("Add `schema = 1`"), "{warning}");
    let newer = studio.path("config/newer.toml");
    fs::write(&newer, demo.replace("\nschema = 1\n", "\nschema = 2\n")).unwrap();
    let (code, report) = studio.check(&newer);
    assert_eq!(code, Some(2), "{report}");
    let [only] = problems(&report)[..] else {
        panic!("{report}");
    };
    assert!(only.contains("this file is schema 2, newer"), "{only}");
    assert!(
        studio.tool_calls().len() == 1,
        "the newer file was not inspected"
    );
}

#[test]
fn a_run_without_a_schema_key_starts_and_says_to_add_it() {
    let studio = Studio::new("run");
    let demo = fs::read_to_string(Path::new(EXAMPLES).join("demo.toml")).unwrap();
    let old = studio.path("config/old.toml");
    fs::write(&old, demo.replace("\nschema = 1\n", "\n")).unwrap();
    let out = studio.run(&[
        "--config",
        old.to_str().unwrap(),
        "--headless",
        "--seconds",
        "0.2",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("auto-camera: warning: ") && stderr.contains("Add `schema = 1`"),
        "{stderr}"
    );
    let summary = String::from_utf8_lossy(&out.stdout);
    assert!(summary.contains("\"ok\": true"), "{summary}");
}
