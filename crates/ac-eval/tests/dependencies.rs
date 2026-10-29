//! What `ac-eval` may depend on, checked with `cargo metadata`.
//!
//! The engine depends on `ac-eval` to emit its per-tick trace, so `ac-eval`
//! must never depend on the engine or any other pipeline crate: scorers read
//! a trace with nothing but the core types and the JSON schema. Of the
//! workspace's crates it may use only `ac-core`, and its normal and build
//! dependencies are exactly [`ALLOWED`]; adding one is a decision to make
//! here, on purpose. Besides the core types, `serde` and `serde_json` for
//! the trace and label schemas, the label validator uses
//! `serde_path_to_error` to name the field at fault and `thiserror` for its
//! error types, and the scorers read their thresholds file with `toml`, a
//! parser like `serde_json` that does no I/O of its own. Dev-dependencies
//! are not part of the library and are not checked.
//!
//! `cargo metadata --no-deps --offline` reads only the workspace manifests,
//! so the test needs no network and no registry.

use std::process::Command;

/// The crates `ac-eval` may depend on, sorted.
const ALLOWED: [&str; 6] = [
    "ac-core",
    "serde",
    "serde_json",
    "serde_path_to_error",
    "thiserror",
    "toml",
];

/// The workspace's metadata, without its dependencies' packages.
fn metadata() -> serde_json::Value {
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
    serde_json::from_slice(&out.stdout).expect("metadata is JSON")
}

/// The names of the workspace's crates, and of `ac-eval`'s normal and build
/// dependencies, sorted.
fn members_and_library_dependencies() -> (Vec<String>, Vec<String>) {
    let meta = metadata();
    let packages = meta["packages"].as_array().expect("a package list");
    let members = packages
        .iter()
        .map(|p| p["name"].as_str().expect("a name").to_owned())
        .collect();
    let package = packages
        .iter()
        .find(|p| p["name"] == "ac-eval")
        .expect("ac-eval is a workspace member");
    let mut names: Vec<String> = package["dependencies"]
        .as_array()
        .expect("a dependency list")
        .iter()
        .filter(|d| d["kind"].is_null() || d["kind"] == "build")
        .map(|d| d["name"].as_str().expect("a name").to_owned())
        .collect();
    names.sort();
    names.dedup();
    (members, names)
}

#[test]
fn ac_eval_depends_only_on_the_core_types_and_serde() {
    let (members, deps) = members_and_library_dependencies();
    assert!(
        ["ac-core", "ac-engine", "auto-camera"]
            .iter()
            .all(|m| members.iter().any(|n| n == m)),
        "the workspace is read: {members:?}"
    );
    let workspace: Vec<&String> = deps.iter().filter(|d| members.contains(d)).collect();
    assert_eq!(workspace, ["ac-core"], "no pipeline crate");
    assert_eq!(deps, ALLOWED);
}
