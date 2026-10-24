//! Runs an exported OWLv2 model on raw video frames and prints what it finds.
//!
//! This is the check for a real export: it loads the graphs the way the
//! engine does, runs the detector on each frame, and prints every detection
//! and the wall time of each `detect` call. The first call includes CoreML's
//! graph compilation and the prompts' text embeddings, so it is reported
//! separately from the steady-state mean.
//!
//! `MODEL_DIR` holds either a split export (`text_encoder.onnx`,
//! `detector.onnx`) or a combined one (`model.onnx`), plus `tokenizer.json`.
//! `FRAMES` is packed RGBA8 frames of `WIDTH`×`HEIGHT`, for example from
//!
//! ```sh
//! ffmpeg -ss 10 -i talk.mp4 -vf fps=1,scale=1280:720 -frames:v 5 \
//!     -f rawvideo -pix_fmt rgba frames.rgba
//! cargo run --release -p ac-vision --features onnx --example owl_detect -- \
//!     models/owlv2-split-onnx 1280 720 frames.rgba "a person playing guitar"
//! ```
//!
//! Each detection prints as `frame <n> <query> <score> x=… y=… w=… h=…` in
//! normalized source coordinates. Set `AC_OWL_CPU=1` to skip CoreML and
//! `AC_OWL_THRESHOLD` to change the object score threshold (default 0.3;
//! prompts that only describe people use the person threshold, 0.15).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::SourceId;
use ac_media::VideoFrame;
use ac_vision::{Detector, OnnxOwlDetector, OwlConfig, OwlModel, PromptQuery};

/// Prints usage and exits with status 2.
fn usage() -> ! {
    eprintln!("usage: owl_detect MODEL_DIR WIDTH HEIGHT FRAMES.rgba PROMPT...");
    std::process::exit(2)
}

/// The export layout found in `dir`: split when it has a text encoder.
fn model_in(dir: &Path) -> OwlModel {
    let text_encoder = dir.join("text_encoder.onnx");
    if text_encoder.is_file() {
        OwlModel::Split {
            text_encoder,
            detector: dir.join("detector.onnx"),
        }
    } else {
        OwlModel::Combined {
            model: dir.join("model.onnx"),
        }
    }
}

/// Milliseconds of `d`.
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
        usage();
    }
    let dir = PathBuf::from(&args[0]);
    let (Ok(width), Ok(height)) = (args[1].parse::<u32>(), args[2].parse::<u32>()) else {
        usage();
    };
    let raw = std::fs::read(&args[3]).unwrap_or_else(|e| {
        eprintln!("cannot read {}: {e}", args[3]);
        std::process::exit(1)
    });
    let queries: Vec<PromptQuery> = args[4..]
        .iter()
        .enumerate()
        .map(|(i, p)| PromptQuery::new(&format!("q{i}"), p))
        .collect();

    let mut config = OwlConfig {
        use_coreml: std::env::var_os("AC_OWL_CPU").is_none(),
        ..OwlConfig::default()
    };
    if let Some(t) = std::env::var("AC_OWL_THRESHOLD")
        .ok()
        .and_then(|t| t.parse().ok())
    {
        config.score_threshold = t;
    }
    let started = Instant::now();
    let mut detector = OnnxOwlDetector::load(model_in(&dir), &dir.join("tokenizer.json"), config)
        .unwrap_or_else(|e| {
            eprintln!("cannot load the model: {e}");
            std::process::exit(1)
        });
    println!(
        "loaded {} with {} in {:.0} ms",
        dir.display(),
        detector.provider(),
        ms(started.elapsed())
    );
    for q in &queries {
        println!("{} = {:?}", q.id.0, q.prompt);
    }

    let frame_len = width as usize * height as usize * 4;
    if frame_len == 0 || !raw.len().is_multiple_of(frame_len) {
        eprintln!(
            "{} bytes is not a whole number of {width}x{height} RGBA frames",
            raw.len()
        );
        std::process::exit(1);
    }
    let mut times = Vec::new();
    for (seq, pixels) in raw.chunks_exact(frame_len).enumerate() {
        let frame = VideoFrame::new(
            SourceId("probe".into()),
            seq as u64,
            Duration::ZERO,
            width,
            height,
            Arc::from(pixels),
        )
        .expect("the chunk matches the frame size");
        let started = Instant::now();
        let found = detector.detect(&frame, &queries).unwrap_or_else(|e| {
            eprintln!("frame {seq}: {e}");
            std::process::exit(1)
        });
        times.push(started.elapsed());
        for d in &found {
            println!(
                "frame {seq} {} {:.3} x={:.3} y={:.3} w={:.3} h={:.3}",
                d.query.0, d.score, d.bbox.x, d.bbox.y, d.bbox.w, d.bbox.h
            );
        }
    }
    if let Some((first, rest)) = times.split_first() {
        print!("detect: first call {:.0} ms", ms(*first));
        if !rest.is_empty() {
            let mean = rest.iter().sum::<Duration>() / rest.len() as u32;
            let max = rest.iter().max().copied().unwrap_or_default();
            print!(
                ", then mean {:.0} ms, max {:.0} ms over {} calls",
                ms(mean),
                ms(max),
                rest.len()
            );
        }
        println!();
    }
}
