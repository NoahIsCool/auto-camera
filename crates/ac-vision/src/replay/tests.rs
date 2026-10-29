//! The detection log and the replay detector: lines that read back to the
//! same bytes and bits, refused versions and bad lines, and answers by PTS
//! for an exact hit, a hit within the tolerance, a miss, a pass removed
//! from the log and a log of another source.

use std::sync::Arc;

use ac_core::{Extent, Rect};

use super::*;

/// The id of source `name`.
fn source(name: &str) -> SourceId {
    SourceId(name.into())
}

/// A 2x2 frame of source `name` with sequence number `seq` at `pts_ms`.
fn frame(name: &str, seq: u64, pts_ms: u64) -> VideoFrame {
    VideoFrame::new(
        source(name),
        seq,
        Duration::from_millis(pts_ms),
        2,
        2,
        Arc::from(vec![0u8; 16]),
    )
    .unwrap()
}

/// A detection of `query` at `x`.
fn det(query: &str, x: f64, score: f32, extent: Extent) -> Detection {
    Detection {
        query: QueryId(query.into()),
        bbox: Rect::new(x, 0.25, 0.125, 0.5),
        score,
        extent,
    }
}

/// The pass of source `name` on frame `seq` at `pts_ms`, which looked for
/// "person" and "guitar" and found one person at `x`.
fn pass(name: &str, seq: u64, pts_ms: u64, x: f64) -> PassRecord {
    PassRecord::new(
        source(name),
        seq,
        Duration::from_millis(pts_ms),
        vec![QueryId("person".into()), QueryId("guitar".into())],
        vec![det("person", x, 0.9, Extent::Person)],
    )
}

/// The queries a request asks for.
fn queries() -> Vec<PromptQuery> {
    vec![
        PromptQuery::new("person", "a person"),
        PromptQuery::new("guitar", "a guitar"),
    ]
}

/// A replay of `log` for source "cam" with a tolerance of `tolerance_ms`.
fn replay(log: Vec<PassRecord>, tolerance_ms: u64) -> ReplayDetector {
    ReplayDetector::new(source("cam"), log, Duration::from_millis(tolerance_ms))
}

/// The detection log text of `passes`.
fn text(passes: &[PassRecord]) -> String {
    passes.iter().map(PassRecord::to_json_line).collect()
}

/// True when `e` is a miss.
fn is_miss(e: &VisionError) -> bool {
    matches!(e, VisionError::NotRecorded { .. })
}

#[test]
fn a_line_reads_back_to_the_same_record_and_bytes() {
    let record = PassRecord::new(
        source("stage"),
        41,
        Duration::from_nanos(1_366_666_666),
        vec![QueryId("person".into())],
        vec![
            det("person", 0.1 + 0.2, 0.123_456_79, Extent::Person),
            Detection {
                bbox: Rect::new(1.0 / 3.0, 2.0 / 7.0, 0.1, 1e-9),
                ..det("person", 0.0, f32::MIN_POSITIVE, Extent::Object)
            },
        ],
    );
    let line = record.to_json_line();
    assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
    assert!(line.starts_with(r#"{"version":1,"source":"stage","seq":41,"pts_ns":1366666666,"#));
    let back = PassRecord::from_json_line(&line).unwrap();
    assert_eq!(back, record);
    assert_eq!(back.to_json_line(), line, "the same bytes");
    for (a, b) in back.detections.iter().zip(&record.detections) {
        assert_eq!(a.bbox.x.to_bits(), b.bbox.x.to_bits());
        assert_eq!(a.bbox.h.to_bits(), b.bbox.h.to_bits());
        assert_eq!(a.score.to_bits(), b.score.to_bits());
    }
}

#[test]
fn scores_and_boxes_read_back_bit_for_bit() {
    // A spread of values with long expansions, as model outputs have.
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    for _ in 0..500 {
        let d = Detection {
            query: QueryId("q".into()),
            bbox: Rect::new(next(), next(), next(), next()),
            score: next() as f32,
            extent: Extent::Object,
        };
        let record = PassRecord::new(source("s"), 1, Duration::ZERO, vec![], vec![d.clone()]);
        let back = PassRecord::from_json_line(&record.to_json_line()).unwrap();
        let b = &back.detections[0];
        assert_eq!(b.score.to_bits(), d.score.to_bits(), "{}", d.score);
        assert_eq!(b.bbox.x.to_bits(), d.bbox.x.to_bits(), "{}", d.bbox.x);
        assert_eq!(b.bbox.w.to_bits(), d.bbox.w.to_bits(), "{}", d.bbox.w);
    }
}

#[test]
fn other_versions_and_bad_lines_are_refused() {
    let line = pass("cam", 1, 0, 0.1).to_json_line();
    let v2 = line.replacen(r#""version":1"#, r#""version":2"#, 1);
    let err = PassRecord::from_json_line(&v2).unwrap_err();
    assert!(err.message.contains("version 2 is not supported"), "{err}");
    let unversioned = line.replacen(r#""version":1,"#, "", 1);
    let err = PassRecord::from_json_line(&unversioned).unwrap_err();
    assert_eq!(err.message, "no format version");
    for bad in ["", "not json", "[1, 2]", r#"{"version":1}"#] {
        assert!(PassRecord::from_json_line(bad).is_err(), "{bad:?}");
    }
    // An older line without extents reads them as objects.
    let old = line.replace(r#","extent":"person""#, "");
    let back = PassRecord::from_json_line(&old).unwrap();
    assert_eq!(back.detections[0].extent, Extent::Object);
    // An unknown field is ignored, so fields can be added within a version.
    let newer = line.replacen(r#""seq":1,"#, r#""seq":1,"new":true,"#, 1);
    assert_eq!(PassRecord::from_json_line(&newer).unwrap().seq, 1);
}

#[test]
fn a_log_reads_in_order_and_names_its_bad_line() {
    let passes = vec![pass("cam", 1, 0, 0.1), pass("cam", 4, 100, 0.2)];
    let log = text(&passes);
    assert_eq!(parse_pass_log(&log).unwrap(), passes);
    assert_eq!(
        parse_pass_log(log.trim_end()).unwrap(),
        passes,
        "no final newline"
    );
    assert_eq!(parse_pass_log("").unwrap(), vec![]);
    // A pass deleted by hand may leave a blank line.
    let gapped = log.replacen('\n', "\n\n", 1);
    assert_eq!(parse_pass_log(&gapped).unwrap(), passes);
    let broken = format!("{log}{{oops\n");
    let err = parse_pass_log(&broken).unwrap_err();
    assert_eq!(err.line, Some(3));
    assert!(
        err.to_string().starts_with("detection log line 3: "),
        "{err}"
    );
    let record_err = PassRecord::from_json_line("{").unwrap_err();
    assert!(record_err.to_string().starts_with("detection log record: "));
}

#[test]
fn an_exact_hit_answers_with_the_recorded_pass() {
    let mut d = replay(vec![pass("cam", 3, 100, 0.1), pass("cam", 6, 200, 0.4)], 0);
    assert_eq!(d.name(), "replay");
    let out = d.detect(&frame("cam", 3, 100), &queries()).unwrap();
    assert_eq!(out, vec![det("person", 0.1, 0.9, Extent::Person)]);
    let out = d.detect(&frame("cam", 6, 200), &queries()).unwrap();
    assert_eq!(out[0].bbox.x, 0.4);
    // Sequence numbers are not compared: PTS is the media position.
    let out = d.detect(&frame("cam", 99, 200), &queries()).unwrap();
    assert_eq!(out[0].bbox.x, 0.4);
    // Asking again gives the same answer: nothing is used up.
    let again = d.detect(&frame("cam", 3, 100), &queries()).unwrap();
    assert_eq!(again[0].bbox.x, 0.1);
}

#[test]
fn a_hit_within_the_tolerance_takes_the_nearest_pass() {
    let d = replay(vec![pass("cam", 3, 100, 0.1), pass("cam", 6, 200, 0.4)], 16);
    let x = |pts_ms| {
        d.find(Duration::from_millis(pts_ms))
            .map(|p| p.detections[0].bbox.x)
    };
    assert_eq!(x(84), Some(0.1));
    assert_eq!(x(116), Some(0.1));
    assert_eq!(x(190), Some(0.4));
    assert_eq!(x(83), None, "17 ms off");
    assert_eq!(x(150), None);
    // Equally near passes: the earlier wins.
    let d = replay(vec![pass("cam", 6, 200, 0.4), pass("cam", 3, 100, 0.1)], 50);
    let x = |pts_ms| {
        d.find(Duration::from_millis(pts_ms))
            .map(|p| p.detections[0].bbox.x)
    };
    assert_eq!(x(150), Some(0.1));
    assert_eq!(x(151), Some(0.4));
    assert_eq!(d.passes()[0].seq, 3, "sorted by PTS");
}

#[test]
fn passes_with_the_same_pts_answer_with_the_first_listed() {
    let log = vec![
        pass("cam", 3, 100, 0.1),
        pass("cam", 3, 100, 0.2),
        pass("cam", 9, 300, 0.3),
    ];
    let d = replay(log, 100);
    for pts_ms in [100, 120, 180] {
        let p = d.find(Duration::from_millis(pts_ms)).unwrap();
        assert_eq!(p.detections[0].bbox.x, 0.1, "at {pts_ms} ms");
    }
    assert_eq!(d.find(Duration::from_millis(250)).unwrap().seq, 9);
}

#[test]
fn a_miss_is_not_recorded() {
    let mut d = replay(vec![pass("cam", 3, 100, 0.1)], 16);
    let err = d.detect(&frame("cam", 5, 166), &queries()).unwrap_err();
    assert!(is_miss(&err), "{err}");
    assert_eq!(
        err.to_string(),
        "no recorded pass of source cam at 166ms",
        "names the source and PTS"
    );
    let mut empty = replay(Vec::new(), 1000);
    assert!(is_miss(
        &empty.detect(&frame("cam", 0, 0), &queries()).unwrap_err()
    ));
    // Without queries there is nothing to look for, recorded or not.
    assert!(d.detect(&frame("cam", 5, 166), &[]).unwrap().is_empty());
}

#[test]
fn a_pass_removed_from_the_log_misses_and_the_rest_still_hit() {
    let passes = vec![
        pass("cam", 0, 0, 0.1),
        pass("cam", 3, 100, 0.2),
        pass("cam", 6, 200, 0.3),
    ];
    let full = text(&passes);
    let line = passes[1].to_json_line();
    let edited = full.replacen(&line, "", 1);
    let mut d = replay(parse_pass_log(&edited).unwrap(), 16);
    assert_eq!(d.passes().len(), 2);
    let err = d.detect(&frame("cam", 3, 100), &queries()).unwrap_err();
    assert!(is_miss(&err), "{err}");
    for (seq, pts_ms, x) in [(0, 0, 0.1), (6, 200, 0.3)] {
        let out = d.detect(&frame("cam", seq, pts_ms), &queries()).unwrap();
        assert_eq!(out[0].bbox.x, x);
    }
}

#[test]
fn a_log_of_another_source_never_answers() {
    let log = vec![pass("stage", 3, 100, 0.1), pass("stage", 6, 200, 0.2)];
    let mut d = ReplayDetector::new(source("cam"), log.clone(), Duration::from_millis(16));
    assert_eq!(d.source(), &source("cam"));
    assert!(d.passes().is_empty(), "only its own source's passes");
    for f in [frame("cam", 3, 100), frame("stage", 3, 100)] {
        let err = d.detect(&f, &queries()).unwrap_err();
        assert!(is_miss(&err), "{err}");
    }
    // The detector of the log's own source answers, also in a mixed log.
    let mixed: Vec<PassRecord> = log
        .iter()
        .cloned()
        .chain([pass("cam", 3, 100, 0.9)])
        .collect();
    let mut stage = ReplayDetector::new(source("stage"), mixed.clone(), Duration::ZERO);
    let out = stage.detect(&frame("stage", 3, 100), &queries()).unwrap();
    assert_eq!(out[0].bbox.x, 0.1);
    let mut cam = ReplayDetector::new(source("cam"), mixed, Duration::ZERO);
    let out = cam.detect(&frame("cam", 3, 100), &queries()).unwrap();
    assert_eq!(out[0].bbox.x, 0.9);
    assert!(is_miss(
        &cam.detect(&frame("stage", 3, 100), &queries()).unwrap_err()
    ));
}

#[test]
fn answers_hold_only_the_queries_asked_for_within_the_frame() {
    let record = PassRecord::new(
        source("cam"),
        1,
        Duration::from_millis(33),
        vec![QueryId("person".into()), QueryId("guitar".into())],
        vec![
            det("guitar", 0.5, 0.7, Extent::Object),
            det("person", 0.1, 0.9, Extent::Person),
            // Half outside the frame, and over 1: clipped and clamped.
            Detection {
                bbox: Rect::new(0.9, 0.25, 0.2, 0.5),
                ..det("person", 0.0, 1.5, Extent::Person)
            },
        ],
    );
    let mut d = replay(vec![record], 0);
    let only_people = [PromptQuery::new("person", "a person")];
    let out = d.detect(&frame("cam", 1, 33), &only_people).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], det("person", 0.1, 0.9, Extent::Person));
    assert!(Rect::FULL.contains_rect(&out[1].bbox));
    assert_eq!(out[1].score, 1.0);
    // A query the pass did not look for gets nothing.
    let drums = [PromptQuery::new("drums", "drums")];
    assert!(d.detect(&frame("cam", 1, 33), &drums).unwrap().is_empty());
    // Order is the detector's, extents are kept.
    let all = d.detect(&frame("cam", 1, 33), &queries()).unwrap();
    assert_eq!(all[0].query, QueryId("guitar".into()));
    assert_eq!(all[0].extent, Extent::Object);
}

#[test]
fn a_replay_is_usable_as_a_trait_object() {
    let mut boxed: Box<dyn Detector> = Box::new(replay(vec![pass("cam", 1, 33, 0.3)], 0));
    assert_eq!(
        boxed
            .detect(&frame("cam", 1, 33), &queries())
            .unwrap()
            .len(),
        1
    );
    assert!(format!("{:?}", replay(Vec::new(), 1)).contains("ReplayDetector"));
}
