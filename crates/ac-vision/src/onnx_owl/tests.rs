//! Runtime tests for [`OnnxOwlDetector`] against the synthetic graphs in
//! `tests/fixtures/owl` (see `make_fixtures.py` there). They need ONNX
//! Runtime, which the `onnx` feature provides, but no model download; they
//! skip when the fixtures are missing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ac_core::{Rect, SourceId};

use super::*;

fn fixtures() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/owl");
    if dir.join("combined.onnx").is_file() {
        Some(dir)
    } else {
        eprintln!("skipping: OWL fixtures not found in {}", dir.display());
        None
    }
}

fn config() -> OwlConfig {
    OwlConfig {
        input_size: 32,
        use_coreml: false,
        ..OwlConfig::default()
    }
}

fn combined(dir: &Path) -> OwlModel {
    OwlModel::Combined {
        model: dir.join("combined.onnx"),
    }
}

fn split(dir: &Path) -> OwlModel {
    OwlModel::Split {
        text_encoder: dir.join("text_encoder.onnx"),
        detector: dir.join("detector.onnx"),
    }
}

/// A 64x32 frame of one grey level.
fn frame(level: u8) -> VideoFrame {
    let data: Vec<u8> = (0..64 * 32)
        .flat_map(|_| [level, level, level, 255])
        .collect();
    VideoFrame::new(
        SourceId("cam".into()),
        0,
        Duration::ZERO,
        64,
        32,
        Arc::from(data),
    )
    .unwrap()
}

fn queries() -> Vec<PromptQuery> {
    vec![
        PromptQuery::new("p", "person"),
        PromptQuery::new("g", "Guitar"),
        PromptQuery::new("d", "drum"),
    ]
}

fn near(a: Rect, b: Rect) -> bool {
    [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)]
        .iter()
        .all(|(u, v)| (u - v).abs() < 1e-5)
}

/// Checks the detections the fixtures are built to produce.
fn assert_expected(dets: &[Detection], bright: bool) {
    let by = |q: &str| dets.iter().filter(|d| d.query.0 == q).collect::<Vec<_>>();
    let person = by("p");
    assert_eq!(
        person.len(),
        1,
        "overlapping person box suppressed: {dets:?}"
    );
    assert!(
        near(person[0].bbox, Rect::new(0.15, 0.1, 0.2, 0.4)),
        "{:?}",
        person[0].bbox
    );
    assert!((person[0].score - crate::owl::sigmoid(3.0)).abs() < 1e-5);
    let guitar = by("g");
    assert_eq!(guitar.len(), 1);
    assert!(
        near(guitar[0].bbox, Rect::new(0.65, 0.1, 0.2, 0.4)),
        "{:?}",
        guitar[0].bbox
    );
    let drum = by("d");
    if bright {
        assert_eq!(drum.len(), 1, "bright frame lights the drum patch");
        assert!(
            near(drum[0].bbox, Rect::new(0.4, 0.4, 0.2, 0.4)),
            "{:?}",
            drum[0].bbox
        );
    } else {
        assert!(drum.is_empty(), "dark frame: {drum:?}");
    }
}

#[test]
fn combined_graph_detects_and_decodes() {
    let Some(dir) = fixtures() else { return };
    let mut det =
        OnnxOwlDetector::load(combined(&dir), &dir.join("tokenizer.json"), config()).unwrap();
    assert_eq!(det.name(), "owl-onnx");
    assert_eq!(det.provider(), "CPU");
    assert_expected(&det.detect(&frame(255), &queries()).unwrap(), true);
    assert_expected(&det.detect(&frame(0), &queries()).unwrap(), false);
    assert!(det.detect(&frame(255), &[]).unwrap().is_empty());
}

#[test]
fn split_graphs_cache_text_embeddings() {
    let Some(dir) = fixtures() else { return };
    let mut det =
        OnnxOwlDetector::load(split(&dir), &dir.join("tokenizer.json"), config()).unwrap();
    assert_eq!(det.cached_embeddings(), 0);
    assert_expected(&det.detect(&frame(255), &queries()).unwrap(), true);
    assert_eq!(det.cached_embeddings(), 3);
    // Same prompts up to case and spacing reuse the cache.
    let respelled = vec![
        PromptQuery::new("p", "  PERSON "),
        PromptQuery::new("g", "guitar"),
    ];
    let dets = det.detect(&frame(0), &respelled).unwrap();
    assert_eq!(det.cached_embeddings(), 3);
    assert_eq!(dets.len(), 2);
}

#[test]
fn per_query_thresholds_apply() {
    let Some(dir) = fixtures() else { return };
    let mut cfg = config();
    cfg.query_thresholds.insert(QueryId("p".into()), 0.99);
    let mut det = OnnxOwlDetector::load(combined(&dir), &dir.join("tokenizer.json"), cfg).unwrap();
    let dets = det.detect(&frame(255), &queries()).unwrap();
    assert!(dets.iter().all(|d| d.query.0 != "p"));
    assert!(dets.iter().any(|d| d.query.0 == "g"));
}

#[test]
fn duplicate_prompts_get_their_own_columns() {
    let Some(dir) = fixtures() else { return };
    let mut det =
        OnnxOwlDetector::load(split(&dir), &dir.join("tokenizer.json"), config()).unwrap();
    let q = vec![
        PromptQuery::new("a", "person"),
        PromptQuery::new("b", "person"),
    ];
    let dets = det.detect(&frame(255), &q).unwrap();
    assert_eq!(dets.len(), 2);
    assert_eq!(det.cached_embeddings(), 1);
}

#[test]
fn load_reports_missing_files_and_wrong_names() {
    let Some(dir) = fixtures() else { return };
    let tok = dir.join("tokenizer.json");
    let missing = OwlModel::Combined {
        model: dir.join("nope.onnx"),
    };
    assert!(matches!(
        OnnxOwlDetector::load(missing, &tok, config()),
        Err(VisionError::ModelNotFound(_))
    ));
    assert!(matches!(
        OnnxOwlDetector::load(combined(&dir), &dir.join("nope.json"), config()),
        Err(VisionError::ModelNotFound(_))
    ));
    assert!(matches!(
        OnnxOwlDetector::load(combined(&dir), &dir.join("make_fixtures.py"), config()),
        Err(VisionError::Tokenizer(_))
    ));
    let mut renamed = config();
    renamed.names.pixel_values = "images".into();
    let err = OnnxOwlDetector::load(combined(&dir), &tok, renamed).unwrap_err();
    assert!(err.to_string().contains("images"), "{err}");
    let bad = OwlConfig {
        nms_iou: 2.0,
        ..config()
    };
    assert!(matches!(
        OnnxOwlDetector::load(combined(&dir), &tok, bad),
        Err(VisionError::InvalidConfig(_))
    ));
}

#[test]
fn wrong_input_size_is_a_runtime_error_not_a_panic() {
    let Some(dir) = fixtures() else { return };
    let cfg = OwlConfig {
        input_size: 48,
        ..config()
    };
    let mut det = OnnxOwlDetector::load(combined(&dir), &dir.join("tokenizer.json"), cfg).unwrap();
    assert!(matches!(
        det.detect(&frame(255), &queries()),
        Err(VisionError::Model(_))
    ));
}

#[test]
fn shape_check_rejects_mismatches() {
    assert_eq!(check_shapes(&[1, 4, 3], &[1, 4, 4], 3).unwrap(), 4);
    assert!(check_shapes(&[1, 4, 2], &[1, 4, 4], 3).is_err());
    assert!(check_shapes(&[1, 4, 3], &[1, 5, 4], 3).is_err());
    assert!(check_shapes(&[2, 4, 3], &[2, 4, 4], 3).is_err());
    assert!(check_shapes(&[4, 3], &[4, 4], 3).is_err());
}

/// A failed CoreML compile names the cache folder to delete; other errors,
/// and sessions without a CoreML cache, are passed on as they are.
#[test]
fn a_failed_coreml_compile_names_the_cache_to_delete() {
    let dir = Path::new("/models/owl/coreml-cache/detector-123-456");
    let failed = "Error compiling model: compiler error".to_owned();
    let hinted = with_cache_hint(failed.clone(), Some(dir));
    assert!(hinted.starts_with(&failed), "{hinted}");
    assert!(
        hinted.ends_with("delete /models/owl/coreml-cache/detector-123-456 and restart)"),
        "{hinted}"
    );
    assert_eq!(with_cache_hint(failed.clone(), None), failed);
    let other = "Invalid model".to_owned();
    assert_eq!(with_cache_hint(other.clone(), Some(dir)), other);
    assert_eq!(
        cache_in_use("CPU", Path::new("/models/owl/detector.onnx")),
        None
    );
}
