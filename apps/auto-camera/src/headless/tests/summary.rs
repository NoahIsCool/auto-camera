//! The summary of snapshots made by hand or by a core that never ticked:
//! what it says about a detector still preparing, about shots that look
//! the same, and about action roles on each kind of detector.

use super::*;

/// The JSON summary of a run whose last and final snapshot is `snap`,
/// ended by `error`.
fn summary_of(snap: &EngineSnapshot, error: Option<&str>) -> serde_json::Value {
    let s = summarize(
        Path::new("x.toml"),
        &options(1.0, None),
        Instant::now(),
        error.map(str::to_owned),
        Totals {
            ticks: 0,
            threads_joined: 0,
            trace: None,
            detections: None,
        },
        Some(snap),
        Some(snap),
        &Sampler::default(),
    );
    serde_json::to_value(&s).unwrap()
}

#[test]
fn a_run_that_ends_while_the_detector_loads_says_so() {
    let mut snap = (*preparing_snapshot()).clone();
    let summary = |snap: &EngineSnapshot| summary_of(snap, Some("stopped"));
    let v = summary(&snap);
    assert_eq!(v["sources"][0]["discovery_preparing"], true);
    assert!(v["sources"][0]["detector_ready_s"].is_null());
    assert_eq!(v["sources"][0]["discovery_requests"], 0);
    // Unsampled shots report their crop as the whole range.
    assert_eq!(v["shots"][0]["crop_width_min"], 1.0);

    let d = &mut snap.sources[0].discovery;
    d.preparing = false;
    d.ready_after = Some(Duration::from_millis(152_500));
    let v = summary(&snap);
    assert_eq!(v["sources"][0]["discovery_preparing"], false);
    assert_eq!(v["sources"][0]["detector_ready_s"], 152.5);
}

#[test]
fn the_notes_name_an_action_role_only_when_the_detector_cannot_see_actions() {
    let sidecar = DetectorSettings::Sidecar(SidecarSettings::new(["sidecar"]));
    let synthetic = DetectorSettings::Synthetic { score: 0.9 };
    for (detector, notes) in [(sidecar, 0), (synthetic, 1)] {
        let mut config = studio();
        config.detector = detector;
        config
            .queries
            .push(QuerySettings::new("Speaker", "speaker upper body"));
        let v = summary_of(&first_snapshot(&config), None);
        let speaker = &v["queries"][1];
        assert_eq!(speaker["detector_phrase"], "a person speaking");
        assert_eq!(
            speaker["notes"].as_array().unwrap().len(),
            notes,
            "{speaker}"
        );
    }
}

#[test]
fn shots_name_the_shot_they_look_the_same_as() {
    let mut snap = (*preparing_snapshot()).clone();
    let wide = snap.shots[0].clone();
    let medium = ShotView {
        id: ShotId("stage/1/medium".into()),
        limited_by_zoom: true,
        ..wide.clone()
    };
    let medium_close = ShotView {
        id: ShotId("stage/1/medium_close".into()),
        limited_by_zoom: true,
        same_picture_as: Some(medium.id.clone()),
        ..wide
    };
    snap.shots.extend([medium, medium_close]);
    let v = summary_of(&snap, None);
    let shots = v["shots"].as_array().unwrap();
    assert_eq!(shots.len(), 3);
    assert!(shots[0]["same_picture_as"].is_null());
    assert!(shots[1]["same_picture_as"].is_null());
    assert_eq!(shots[1]["limited_by_zoom"], true);
    assert_eq!(shots[2]["same_picture_as"], "stage/1/medium");
    assert_eq!(shots[2]["limited_by_zoom"], true);
}
