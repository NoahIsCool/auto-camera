//! OWLv2 / OWL-ViT open-vocabulary detection through ONNX Runtime.
//!
//! Compiled with the `onnx` feature. [`OnnxOwlDetector`] tokenizes prompts
//! with the model's CLIP tokenizer (`tokenizer.json`), preprocesses the frame
//! and decodes the outputs with the pure functions in [`crate::owl`], so only
//! the runtime calls live here.
//!
//! Two export layouts are supported (see the crate README for how to make
//! them):
//!
//! * [`OwlModel::Combined`]: one graph with inputs `input_ids`,
//!   `attention_mask` and `pixel_values` and outputs `logits` and `pred_boxes`,
//!   as produced by `optimum-cli export onnx --task zero-shot-object-detection`.
//!   The text tower runs on every call; tokenizations are cached per prompt.
//! * [`OwlModel::Split`]: a text encoder (`input_ids`, `attention_mask` ->
//!   `text_embeds`) and an image detector (`pixel_values`, `query_embeds` ->
//!   `logits`, `pred_boxes`). Text embeddings are cached per normalized prompt,
//!   so steady-state detection runs only the image side.
//!
//! By default (`OwlConfig::people_first`) the detector returns whole people
//! (`Extent::Person`): each pass asks for people and the prompts' objects, and
//! [`crate::matching`] picks, per prompt, the people holding its object. With
//! `people_first` off it returns each prompt's own boxes (`Extent::Object`).
//!
//! The execution provider is CoreML on macOS when the loaded ONNX Runtime has
//! it (unsupported nodes fall back to the CPU), and the CPU elsewhere. CoreML
//! is given only nodes with static input shapes: the query axis of both
//! graphs is dynamic, and handing such nodes to an MLProgram makes MPSGraph
//! abort the whole process while compiling (a failed assertion, not an
//! error ONNX Runtime could return). With static shapes the image tower,
//! where the time goes, still runs on CoreML. Compiling OWLv2 base for CoreML
//! takes minutes, so each graph's compiled model is cached in
//! `coreml-cache/` next to it, under a name that includes the graph file's
//! size and modification time: a re-exported graph gets a fresh compile
//! instead of a stale one (CoreML keys its cache on graph structure, not
//! weights).
//!
//! Status: exercised in tests against synthetic graphs with the documented
//! names and shapes; not yet validated against a real OWLv2 export on a Mac.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ac_core::QueryId;
use ac_media::VideoFrame;
use ort::ep::ExecutionProviderDispatch;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionOutputs};
use ort::value::{Tensor, TensorRef};
use tokenizers::Tokenizer;

use crate::api::{Detection, Detector, PromptQuery};
use crate::error::VisionError;
use crate::matching::{MatchParams, MatchPlan};
use crate::owl::{
    DecodeParams, OWL_MAX_TOKENS, OwlOutputs, decode, normalize_prompt, pack_tokens,
    preprocess_into,
};

/// Where the exported graphs are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwlModel {
    /// One graph taking token ids and pixels.
    Combined {
        /// Path of the `.onnx` file.
        model: PathBuf,
    },
    /// A text encoder plus an image detector that takes query embeddings.
    Split {
        /// Path of the text encoder `.onnx` file.
        text_encoder: PathBuf,
        /// Path of the detector `.onnx` file.
        detector: PathBuf,
    },
}

/// Tensor names in the exported graphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwlTensorNames {
    /// Token ids, `[queries, tokens]` `i64`.
    pub input_ids: String,
    /// Attention mask, `[queries, tokens]` `i64`.
    pub attention_mask: String,
    /// Normalized image, `[1, 3, side, side]` `f32`.
    pub pixel_values: String,
    /// Detector input of the split layout, `[1, queries, dim]` `f32`.
    pub query_embeds: String,
    /// Text encoder output of the split layout, `[queries, dim]` `f32`.
    pub text_embeds: String,
    /// Logits, `[1, patches, queries]` `f32`.
    pub logits: String,
    /// Boxes, `[1, patches, 4]` `f32` as `(cx, cy, w, h)`.
    pub pred_boxes: String,
}

impl Default for OwlTensorNames {
    fn default() -> Self {
        Self {
            input_ids: "input_ids".into(),
            attention_mask: "attention_mask".into(),
            pixel_values: "pixel_values".into(),
            query_embeds: "query_embeds".into(),
            text_embeds: "text_embeds".into(),
            logits: "logits".into(),
            pred_boxes: "pred_boxes".into(),
        }
    }
}

/// Tuning for [`OnnxOwlDetector`].
#[derive(Debug, Clone, PartialEq)]
pub struct OwlConfig {
    /// Side of the square model input: 960 for OWLv2 base, 1008 for large, 768 for OWL-ViT B/32.
    pub input_size: u32,
    /// Token positions per query.
    pub max_tokens: usize,
    /// Padding token id; `None` uses the tokenizer's `!` (OWL's pad token), else 0.
    pub pad_token_id: Option<u32>,
    /// Score threshold for queries without their own (default 0.3). With
    /// `people_first` it applies to the object a prompt names; everyone and
    /// described-person prompts use `person_threshold` (see
    /// [`crate::matching`]).
    pub score_threshold: f32,
    /// Per-query score thresholds.
    pub query_thresholds: HashMap<QueryId, f32>,
    /// IoU above which a lower-scored box of the same query is suppressed.
    pub nms_iou: f64,
    /// Largest number of detections kept per query.
    pub max_per_query: usize,
    /// Try the CoreML execution provider on macOS.
    pub use_coreml: bool,
    /// Intra-op CPU threads; `None` lets ONNX Runtime decide.
    pub intra_threads: Option<usize>,
    /// Most prompts kept in the tokenization/embedding caches.
    pub max_cached_prompts: usize,
    /// Tensor names in the graphs.
    pub names: OwlTensorNames,
    /// Return whole people chosen by each prompt instead of the prompt's
    /// own boxes ([`crate::matching`]).
    pub people_first: bool,
    /// The prompt that finds people when `people_first` is set.
    pub person_prompt: String,
    /// Lowest person score kept when `people_first` is set, and the
    /// threshold of everyone and described-person prompts without their own.
    pub person_threshold: f32,
    /// Fraction of the smaller of an object's and a person's box that must
    /// lie inside the other for the person to be chosen, when `people_first`
    /// is set.
    pub min_overlap: f64,
}

impl Default for OwlConfig {
    fn default() -> Self {
        let matching = MatchParams::default();
        Self {
            input_size: 960,
            max_tokens: OWL_MAX_TOKENS,
            pad_token_id: None,
            score_threshold: matching.object_threshold,
            query_thresholds: HashMap::new(),
            nms_iou: 0.3,
            max_per_query: 16,
            use_coreml: true,
            intra_threads: None,
            max_cached_prompts: 256,
            names: OwlTensorNames::default(),
            people_first: true,
            person_prompt: matching.person_prompt,
            person_threshold: matching.person_threshold,
            min_overlap: matching.min_overlap,
        }
    }
}

impl OwlConfig {
    /// Checks that every value is in range.
    pub fn validate(&self) -> Result<(), VisionError> {
        let unit = |v: f32| (0.0..=1.0).contains(&v);
        let ok = self.input_size >= 16
            && self.max_tokens >= 2
            && unit(self.score_threshold)
            && self.query_thresholds.values().all(|&t| unit(t))
            && (0.0..=1.0).contains(&self.nms_iou)
            && self.max_per_query >= 1
            && self.max_cached_prompts >= 1
            && unit(self.person_threshold)
            && self.min_overlap > 0.0
            && self.min_overlap <= 1.0
            && !self.person_prompt.trim().is_empty();
        if ok {
            Ok(())
        } else {
            Err(VisionError::InvalidConfig(format!(
                "out-of-range OWL configuration: {self:?}"
            )))
        }
    }
}

/// The loaded graphs.
enum Graphs {
    /// One graph for text and image.
    Combined(Session),
    /// Text encoder and image detector.
    Split {
        /// Text encoder.
        text: Session,
        /// Image detector.
        detector: Session,
    },
}

/// OWLv2 / OWL-ViT detector on ONNX Runtime; see the [module docs](self).
pub struct OnnxOwlDetector {
    /// Tuning.
    config: OwlConfig,
    /// Loaded graphs.
    graphs: Graphs,
    /// CLIP tokenizer.
    tokenizer: Tokenizer,
    /// Padding token id.
    pad_id: u32,
    /// Name of the execution provider in use.
    provider: &'static str,
    /// Token ids per normalized prompt.
    tokens: HashMap<String, Vec<u32>>,
    /// Text embeddings per normalized prompt (split layout).
    embeddings: HashMap<String, Vec<f32>>,
    /// Reused input buffer.
    pixels: Vec<f32>,
}

impl std::fmt::Debug for OnnxOwlDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxOwlDetector")
            .field("provider", &self.provider)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Wraps a runtime error.
fn model_err(e: impl std::fmt::Display) -> VisionError {
    VisionError::Model(e.to_string())
}

/// The execution provider expected to run: `"CoreML"` when it was asked for
/// and the loaded ONNX Runtime has it, else `"CPU"`.
fn provider_name(use_coreml: bool) -> &'static str {
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
fn execution_providers(provider: &str, path: &Path) -> Vec<ExecutionProviderDispatch> {
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
fn coreml_cache_dir(path: &Path) -> Option<PathBuf> {
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

/// Loads one graph and checks that it has the expected inputs.
fn load_session(
    path: &Path,
    inputs: &[&str],
    provider: &str,
    threads: Option<usize>,
) -> Result<Session, VisionError> {
    if !path.is_file() {
        return Err(VisionError::ModelNotFound(path.to_path_buf()));
    }
    // ONNX Runtime keys CoreML's compiled-model cache on the path, so spell
    // it the same way however the caller did.
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut builder = Session::builder()
        .map_err(model_err)?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(model_err)?;
    if let Some(n) = threads {
        builder = builder.with_intra_threads(n).map_err(model_err)?;
    }
    let eps = execution_providers(provider, path);
    if !eps.is_empty() {
        builder = builder.with_execution_providers(eps).map_err(model_err)?;
    }
    let session = builder.commit_from_file(path).map_err(model_err)?;
    let present: Vec<&str> = session.inputs().iter().map(|o| o.name()).collect();
    if let Some(missing) = inputs.iter().find(|n| !present.contains(n)) {
        return Err(VisionError::Model(format!(
            "{} has no input {missing:?}; inputs are {present:?}",
            path.display()
        )));
    }
    Ok(session)
}

/// Copies an `f32` output and its shape.
fn output(outputs: &SessionOutputs<'_>, name: &str) -> Result<(Vec<i64>, Vec<f32>), VisionError> {
    let value = outputs
        .get(name)
        .ok_or_else(|| VisionError::Shape(format!("missing output {name:?}")))?;
    let (shape, data) = value.try_extract_tensor::<f32>().map_err(model_err)?;
    Ok((shape.to_vec(), data.to_vec()))
}

/// Checks `[1, patches, queries]` logits and `[1, patches, 4]` boxes; returns
/// the number of patches.
fn check_shapes(logits: &[i64], boxes: &[i64], queries: usize) -> Result<usize, VisionError> {
    match (logits, boxes) {
        ([1, p, q], [1, pb, 4]) if p == pb && *q as usize == queries => Ok(*p as usize),
        _ => Err(VisionError::Shape(format!(
            "logits {logits:?} and boxes {boxes:?} for {queries} queries"
        ))),
    }
}

impl OnnxOwlDetector {
    /// Loads the graphs and the tokenizer (`tokenizer.json`).
    pub fn load(model: OwlModel, tokenizer: &Path, config: OwlConfig) -> Result<Self, VisionError> {
        config.validate()?;
        if !tokenizer.is_file() {
            return Err(VisionError::ModelNotFound(tokenizer.to_path_buf()));
        }
        let mut tok =
            Tokenizer::from_file(tokenizer).map_err(|e| VisionError::Tokenizer(e.to_string()))?;
        // Padding and truncation are done by `pack_tokens`.
        tok.with_padding(None);
        tok.with_truncation(None)
            .map_err(|e| VisionError::Tokenizer(e.to_string()))?;
        let pad_id = config
            .pad_token_id
            .or_else(|| tok.token_to_id("!"))
            .unwrap_or(0);
        let provider = provider_name(config.use_coreml);
        let n = &config.names;
        let threads = config.intra_threads;
        let graphs = match &model {
            OwlModel::Combined { model } => Graphs::Combined(load_session(
                model,
                &[&n.input_ids, &n.attention_mask, &n.pixel_values],
                provider,
                threads,
            )?),
            OwlModel::Split {
                text_encoder,
                detector,
            } => Graphs::Split {
                text: load_session(
                    text_encoder,
                    &[&n.input_ids, &n.attention_mask],
                    provider,
                    threads,
                )?,
                detector: load_session(
                    detector,
                    &[&n.pixel_values, &n.query_embeds],
                    provider,
                    threads,
                )?,
            },
        };
        Ok(Self {
            config,
            graphs,
            tokenizer: tok,
            pad_id,
            provider,
            tokens: HashMap::new(),
            embeddings: HashMap::new(),
            pixels: Vec::new(),
        })
    }

    /// Name of the execution provider in use (`"CoreML"` or `"CPU"`).
    pub fn provider(&self) -> &str {
        self.provider
    }

    /// The detector's tuning.
    pub fn config(&self) -> &OwlConfig {
        &self.config
    }

    /// Number of prompts with a cached text embedding (split layout).
    pub fn cached_embeddings(&self) -> usize {
        self.embeddings.len()
    }

    /// Token ids of a normalized prompt, cached.
    fn token_ids(&mut self, key: &str) -> Result<Vec<u32>, VisionError> {
        if let Some(ids) = self.tokens.get(key) {
            return Ok(ids.clone());
        }
        let enc = self
            .tokenizer
            .encode(key, true)
            .map_err(|e| VisionError::Tokenizer(e.to_string()))?;
        let ids = enc.get_ids().to_vec();
        if self.tokens.len() >= self.config.max_cached_prompts {
            self.tokens.clear();
        }
        self.tokens.insert(key.to_owned(), ids.clone());
        Ok(ids)
    }

    /// Packed `[n, max_tokens]` id and mask tensors for `keys`.
    fn text_tensors(&mut self, keys: &[&str]) -> Result<(Tensor<i64>, Tensor<i64>), VisionError> {
        let seqs = keys
            .iter()
            .map(|k| self.token_ids(k))
            .collect::<Result<Vec<_>, _>>()?;
        let (ids, mask) = pack_tokens(&seqs, self.config.max_tokens, self.pad_id);
        let shape = [keys.len(), self.config.max_tokens];
        Ok((
            Tensor::from_array((shape, ids)).map_err(model_err)?,
            Tensor::from_array((shape, mask)).map_err(model_err)?,
        ))
    }

    /// Runs the text encoder for prompts missing from the embedding cache.
    fn embed_missing(&mut self, keys: &[String]) -> Result<(), VisionError> {
        let mut seen = HashSet::new();
        let missing: Vec<&str> = keys
            .iter()
            .filter(|k| !self.embeddings.contains_key(*k) && seen.insert(k.as_str()))
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        if self.embeddings.len() + missing.len() > self.config.max_cached_prompts {
            self.embeddings.retain(|k, _| keys.contains(k));
        }
        let (ids, mask) = self.text_tensors(&missing)?;
        let Graphs::Split { text, .. } = &mut self.graphs else {
            return Ok(());
        };
        let n = &self.config.names;
        let outputs = text
            .run(ort::inputs![n.input_ids.as_str() => ids, n.attention_mask.as_str() => mask])
            .map_err(model_err)?;
        let (shape, data) = output(&outputs, &n.text_embeds)?;
        let dim = match shape.as_slice() {
            [m, d] if *m as usize == missing.len() && *d > 0 => *d as usize,
            _ => {
                return Err(VisionError::Shape(format!(
                    "text_embeds {shape:?} for {} prompts",
                    missing.len()
                )));
            }
        };
        for (key, row) in missing.iter().zip(data.chunks_exact(dim)) {
            self.embeddings.insert((*key).to_owned(), row.to_vec());
        }
        Ok(())
    }

    /// Runs the image side; returns logits, boxes and the number of patches.
    fn run_image(&mut self, keys: &[String]) -> Result<(Vec<f32>, Vec<f32>, usize), VisionError> {
        let side = self.config.input_size as usize;
        let q = keys.len();
        let text = match &self.graphs {
            Graphs::Combined(_) => {
                let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
                Some(self.text_tensors(&refs)?)
            }
            Graphs::Split { .. } => {
                self.embed_missing(keys)?;
                None
            }
        };
        let Self {
            graphs,
            pixels,
            embeddings,
            config,
            ..
        } = self;
        let n = &config.names;
        let image = TensorRef::from_array_view(([1usize, 3, side, side], pixels.as_slice()))
            .map_err(model_err)?;
        let outputs = match (graphs, text) {
            (Graphs::Combined(session), text) => {
                let (ids, mask) =
                    text.ok_or_else(|| VisionError::Model("missing text tensors".into()))?;
                session
                    .run(ort::inputs![
                        n.input_ids.as_str() => ids,
                        n.attention_mask.as_str() => mask,
                        n.pixel_values.as_str() => image,
                    ])
                    .map_err(model_err)?
            }
            (Graphs::Split { detector, .. }, _) => {
                let rows: Vec<&Vec<f32>> = keys.iter().filter_map(|k| embeddings.get(k)).collect();
                let dim = rows.first().map_or(0, |r| r.len());
                if rows.len() != q || rows.iter().any(|r| r.len() != dim) {
                    return Err(VisionError::Shape(
                        "inconsistent cached text embeddings".into(),
                    ));
                }
                let flat: Vec<f32> = rows.into_iter().flatten().copied().collect();
                let query = Tensor::from_array(([1, q, dim], flat)).map_err(model_err)?;
                detector
                    .run(ort::inputs![n.pixel_values.as_str() => image, n.query_embeds.as_str() => query])
                    .map_err(model_err)?
            }
        };
        let (logit_shape, logits) = output(&outputs, &n.logits)?;
        let (box_shape, boxes) = output(&outputs, &n.pred_boxes)?;
        let patches = check_shapes(&logit_shape, &box_shape, q)?;
        Ok((logits, boxes, patches))
    }
}

impl Detector for OnnxOwlDetector {
    fn name(&self) -> &str {
        "owl-onnx"
    }

    fn detect(
        &mut self,
        frame: &VideoFrame,
        queries: &[PromptQuery],
    ) -> Result<Vec<Detection>, VisionError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        if !self.config.people_first {
            let thresholds = queries
                .iter()
                .map(|q| {
                    *self
                        .config
                        .query_thresholds
                        .get(&q.id)
                        .unwrap_or(&self.config.score_threshold)
                })
                .collect();
            let max = self.config.max_per_query;
            return self.detect_queries(frame, queries, thresholds, max);
        }
        let plan = MatchPlan::new(
            queries,
            |id| self.config.query_thresholds.get(id).copied(),
            MatchParams {
                person_prompt: self.config.person_prompt.clone(),
                person_threshold: self.config.person_threshold,
                object_threshold: self.config.score_threshold,
                min_overlap: self.config.min_overlap,
                max_per_query: self.config.max_per_query,
            },
        );
        // Keep enough people for a crowd; the plan caps each query again.
        let max = self.config.max_per_query.max(MAX_PEOPLE);
        let found = self.detect_queries(frame, plan.queries(), plan.thresholds().to_vec(), max)?;
        Ok(plan.assign(&found))
    }
}

/// Most people kept per pass when `people_first` is set.
const MAX_PEOPLE: usize = 64;

impl OnnxOwlDetector {
    /// Runs the model for `queries` and decodes each query's boxes at its
    /// threshold, keeping at most `max_per_query` per query.
    fn detect_queries(
        &mut self,
        frame: &VideoFrame,
        queries: &[PromptQuery],
        thresholds: Vec<f32>,
        max_per_query: usize,
    ) -> Result<Vec<Detection>, VisionError> {
        let letterbox = preprocess_into(frame, self.config.input_size, &mut self.pixels)?;
        let keys: Vec<String> = queries
            .iter()
            .map(|q| normalize_prompt(&q.prompt))
            .collect();
        let (logits, boxes, patches) = self.run_image(&keys)?;
        let params = DecodeParams {
            thresholds,
            nms_iou: self.config.nms_iou,
            max_per_query,
        };
        let outputs = OwlOutputs {
            logits: &logits,
            boxes: &boxes,
            num_boxes: patches,
            num_queries: queries.len(),
        };
        decode(&outputs, queries, &letterbox, &params)
    }
}

#[cfg(test)]
mod tests;
