//! Execution providers for the OWL graphs, and CoreML's compile cache.
//!
//! The provider is CoreML on macOS when the loaded ONNX Runtime has it
//! (unsupported nodes fall back to the CPU), and the CPU elsewhere. CoreML
//! is given only nodes with static input shapes: the query axis of both
//! graphs is dynamic, and handing such nodes to an MLProgram makes MPSGraph
//! abort the whole process while compiling (a failed assertion, not an
//! error ONNX Runtime could return). With static shapes the image tower,
//! where the time goes, still runs on CoreML. Compiling OWLv2 base for CoreML
//! takes minutes, so each graph's compiled model is cached in
//! `coreml-cache/` next to it, under a name that includes the graph file's
//! size and modification time: a re-exported graph gets a fresh compile
//! instead of a stale one (ONNX Runtime keys its CoreML cache on the model's
//! path and the provider options, not on the graph's structure or weights,
//! so a re-export at the same path would otherwise reuse the old compile).
//! That cache survives rebuilding the app; CoreML's own per-executable
//! specialization, which every new build pays again, is outside it (see the
//! crate README). ONNX Runtime takes an existing cache folder for a
//! finished compile, so one left half-written by an interrupted first load
//! can fail every later load with "Error compiling model"; that error names
//! the folder to delete. It is never deleted here, since another loader
//! (another source, or another process) may be writing it.

use std::path::{Path, PathBuf};

use ort::ep::ExecutionProviderDispatch;

/// The execution provider expected to run: `"CoreML"` when it was asked for
/// and the loaded ONNX Runtime has it, else `"CPU"`.
pub(super) fn provider_name(use_coreml: bool) -> &'static str {
    #[cfg(target_os = "macos")]
    if use_coreml {
        use ort::ep::ExecutionProvider;
        if ort::ep::CoreML::default().is_available().unwrap_or(false) {
            return "CoreML";
        }
    }
    let _ = use_coreml;
    "CPU"
}

/// Execution providers to register for the graph at `path` when `provider`
/// is in use; see the module docs for the CoreML options.
pub(super) fn execution_providers(provider: &str, path: &Path) -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "macos")]
    if provider == "CoreML" {
        let mut coreml = ort::ep::CoreML::default()
            .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
            .with_static_input_shapes(true);
        if let Some(dir) = coreml_cache_dir(path) {
            coreml = coreml.with_model_cache_dir(dir.display());
        }
        return vec![coreml.build()];
    }
    let _ = (provider, path);
    Vec::new()
}

/// Where CoreML caches its compiled copy of the graph at `path`:
/// `coreml-cache/<stem>-<size>-<mtime>` next to it, or `None` when the file's
/// metadata cannot be read.
#[cfg(target_os = "macos")]
pub(super) fn coreml_cache_dir(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let stem = path.file_stem()?.to_string_lossy();
    let dir = path.parent()?.join("coreml-cache");
    Some(dir.join(format!("{stem}-{}-{modified}", meta.len())))
}

/// The folder CoreML's compiled copy of the graph at `path` is cached in,
/// when `provider` is CoreML and the folder can be named.
pub(super) fn cache_in_use(provider: &str, path: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    if provider == "CoreML" {
        return coreml_cache_dir(path);
    }
    let _ = (provider, path);
    None
}

/// `message`, from creating a session whose CoreML cache is `cache`, with
/// the folder to delete when CoreML could not compile the graph; see the
/// module docs.
pub(super) fn with_cache_hint(message: String, cache: Option<&Path>) -> String {
    match cache {
        Some(dir) if message.contains("Error compiling model") => format!(
            "{message} (an interrupted compile can leave a broken cache: \
             delete {} and restart)",
            dir.display()
        ),
        _ => message,
    }
}
