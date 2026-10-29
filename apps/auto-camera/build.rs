//! Build script: records the git commit the app is built from as
//! `AC_GIT_HASH`, for the log's header (`logs` module).
//!
//! The hash is `AC_GIT_HASH` from the environment when set (a packager
//! building from a source archive), else what `git rev-parse` says, else
//! `unknown` (no git, or not a checkout). The script runs again when the
//! checkout's `HEAD` moves: a commit, checkout or reset writes `HEAD` or
//! its reflog. Edits that are not committed do not change the hash.

use std::process::Command;

/// Prints the `cargo:` instructions; see the module docs.
fn main() {
    println!("cargo:rerun-if-env-changed=AC_GIT_HASH");
    let hash = std::env::var("AC_GIT_HASH")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=AC_GIT_HASH={}", hash.trim());
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/logs/HEAD");
    }
}

/// The trimmed output of `git args`, when git runs and succeeds.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (out.status.success() && !text.is_empty()).then(|| text.to_owned())
}
