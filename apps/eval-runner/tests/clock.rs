//! The runner reaches no clock but the virtual one it is handed.
//!
//! [`Runner::run`](eval_runner::Runner::run) takes a `&VirtualClock`, so
//! the type system keeps the wall clock out of a run; its docs carry a
//! `compile_fail` example that passes a wall clock, which rustdoc checks
//! does not compile. This test keeps real time out of the rest of the
//! crate: outside that example, no source file under `src/` may name the
//! wall clock, a timer or a sleep, nor what runs on the wall clock (the
//! engine's runtime and workers, a live source), so nothing in the runner
//! can read real time. Its dependencies are a fixed list ([`ALLOWED`],
//! checked with `cargo metadata --no-deps --offline`, which reads only the
//! workspace's manifests): neither the output crate, whose encoder runs on
//! the wall clock, nor the sidecar host, which answers in real time, nor a
//! signal handler or a timer can come in without a decision made here.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The crates the runner may depend on (normal and build dependencies),
/// sorted.
const ALLOWED: [&str; 9] = [
    "ac-core",
    "ac-director",
    "ac-engine",
    "ac-media",
    "ac-sources",
    "ac-vision",
    "clap",
    "serde",
    "serde_json",
];

/// Names that would bring real time into the runner.
const FORBIDDEN: [&str; 10] = [
    "WallClock",
    "Instant",
    "SystemTime",
    "thread::sleep",
    "Engine::start",
    "EngineOptions",
    "open_source",
    "FileSource",
    "SyntheticSource",
    "DiscoveryWorker",
];

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
    files
}

/// `text` split into its code and the lines of its `compile_fail` doc
/// examples.
fn split_examples(text: &str) -> (String, String) {
    let (mut code, mut examples) = (String::new(), String::new());
    let mut inside = false;
    for line in text.lines() {
        let doc = line.trim_start();
        if doc == "/// ```compile_fail" {
            inside = true;
        } else if inside && doc == "/// ```" {
            inside = false;
        } else if inside {
            examples.push_str(line);
            examples.push('\n');
            continue;
        }
        code.push_str(line);
        code.push('\n');
    }
    (code, examples)
}

#[test]
fn no_source_file_names_a_wall_clock_or_a_real_time_component() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src);
    assert!(files.len() >= 5, "found {files:?}");
    let (mut virtual_clock, mut refused) = (false, false);
    for file in &files {
        let (code, examples) = split_examples(&std::fs::read_to_string(file).unwrap());
        for name in FORBIDDEN {
            assert!(
                !code.contains(name),
                "{} names {name}, which runs on real time",
                file.display()
            );
        }
        virtual_clock |= code.contains("clock: &VirtualClock");
        refused |= examples.contains("WallClock::new()") && examples.contains("runner.run(&clock");
    }
    assert!(virtual_clock, "the run takes a VirtualClock");
    assert!(refused, "the docs show that a wall clock does not compile");
}

#[test]
fn only_compile_fail_examples_are_set_aside() {
    let text = "/// ```\n/// let a = WallClock::new();\n/// ```\n\
                /// ```compile_fail\n/// let b = WallClock::new();\n/// ```\nfn f() {}\n";
    let (code, examples) = split_examples(text);
    assert!(code.contains("let a") && code.contains("fn f"), "{code}");
    assert!(!code.contains("let b"), "{code}");
    assert_eq!(examples, "/// let b = WallClock::new();\n");
}

/// The runner's normal and build dependencies, sorted, from the
/// workspace's metadata.
fn library_dependencies() -> Vec<String> {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let cargo = option_env!("CARGO").unwrap_or("cargo");
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .args(["--manifest-path", manifest])
        .output()
        .expect("cargo runs");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata is JSON");
    let package = meta["packages"]
        .as_array()
        .expect("a package list")
        .iter()
        .find(|p| p["name"] == "eval-runner")
        .expect("the runner is a workspace member")
        .clone();
    let mut names: Vec<String> = package["dependencies"]
        .as_array()
        .expect("a dependency list")
        .iter()
        .filter(|d| d["kind"].is_null() || d["kind"] == "build")
        .map(|d| d["name"].as_str().expect("a name").to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

#[test]
fn the_runner_depends_only_on_the_allowed_crates() {
    let deps = library_dependencies();
    assert!(!deps.contains(&"ac-output".to_string()), "no encoder");
    assert_eq!(deps, ALLOWED);
}
