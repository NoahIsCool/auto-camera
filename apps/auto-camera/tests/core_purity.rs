//! The core crates stay free of logging: none depends on `tracing`, not
//! even through another workspace crate.
//!
//! CLAUDE.md keeps the core crates pure and deterministic, with I/O in the
//! adapter crates; a log is I/O. So only the adapters (which run ffmpeg,
//! the sidecar and the engine's threads) and the apps may emit `tracing`
//! events, and the core publishes its state as data, which the app's
//! snapshot watcher logs. This test reads the workspace's manifests with
//! `cargo metadata` (offline, no dependency resolution) and checks every
//! crate outside [`ADAPTERS`]: its normal and build dependencies must not
//! name `tracing` or a `tracing-*` crate, and the workspace crates it uses
//! must be core crates too. Dev-dependencies do not count: tests may use
//! anything. A third-party crate may still pull `tracing` in (the optional
//! ONNX Runtime binding of `ac-vision` does); that is the library's own
//! logging, which stays silent unless the app installs a subscriber.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// The workspace crates that may log: the adapters and the apps (the eval
/// runner drives the engine and decodes files, so it is one of them too).
const ADAPTERS: [&str; 6] = [
    "ac-engine",
    "ac-output",
    "ac-sidecar",
    "ac-sources",
    "auto-camera",
    "eval-runner",
];

/// True when a dependency named `name` is a logging crate.
fn is_logging(name: &str) -> bool {
    name == "tracing" || name.starts_with("tracing-")
}

/// The workspace's packages, by name, with their normal and build
/// dependencies' names.
fn workspace() -> BTreeMap<String, BTreeSet<String>> {
    let cargo = std::env::var_os("CARGO")
        .or_else(|| option_env!("CARGO").map(Into::into))
        .unwrap_or_else(|| "cargo".into());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("cargo runs");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("cargo metadata prints JSON");
    meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| {
            let deps = p["dependencies"]
                .as_array()
                .expect("dependencies")
                .iter()
                .filter(|d| d["kind"].as_str() != Some("dev"))
                .map(|d| d["name"].as_str().expect("a name").to_owned())
                .collect();
            (p["name"].as_str().expect("a name").to_owned(), deps)
        })
        .collect()
}

#[test]
fn no_core_crate_depends_on_tracing() {
    let crates = workspace();
    for adapter in ADAPTERS {
        assert!(
            crates.contains_key(adapter),
            "{adapter} is not in the workspace"
        );
    }
    // The check means something only while the adapters do log.
    assert!(crates["ac-output"].contains("tracing"));
    assert!(crates["auto-camera"].contains("tracing"));
    let core: Vec<&String> = crates
        .keys()
        .filter(|n| !ADAPTERS.contains(&n.as_str()))
        .collect();
    assert!(core.iter().any(|n| *n == "ac-core"), "{core:?}");
    assert!(core.len() >= 8, "the core crates: {core:?}");
    for name in core {
        for dep in &crates[name] {
            assert!(!is_logging(dep), "core crate {name} depends on {dep}");
            assert!(
                !ADAPTERS.contains(&dep.as_str()),
                "core crate {name} depends on the adapter {dep}, which logs"
            );
        }
    }
}

#[test]
fn logging_crates_are_recognised() {
    assert!(is_logging("tracing"));
    assert!(is_logging("tracing-subscriber"));
    assert!(!is_logging("tracingx"));
    assert!(!is_logging("ac-core"));
}
