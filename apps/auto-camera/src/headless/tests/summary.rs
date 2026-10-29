//! The summary of snapshots made by hand or by a core that never ticked:
//! what it says about a detector still preparing, about shots that look
//! the same, about action roles on each kind of detector, and about the
//! stream and the program loudness of an output.

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
fn the_output_reports_the_program_loudness() {
    let summary = |snap: &EngineSnapshot| {
        let s = summarize(
            Path::new("x.toml"),
            &options(1.0, Some(PathBuf::from("show.mp4"))),
            Instant::now(),
            None,
            Totals {
                ticks: 0,
                threads_joined: 0,
                trace: None,
            },
            Some(snap),
            Some(snap),
            &Sampler::default(),
        );
        serde_json::to_value(&s).unwrap()
    };
    let mut snap = (*preparing_snapshot()).clone();
    // An output without program audio has no meter.
    let v = summary(&snap);
    assert!(v["output"].is_object() && v["output"]["loudness"].is_null());
    snap.output.loudness = Some(ac_audio::LoudnessReading {
        momentary: Some(-22.96),
        short_term: Some(-23.04),
        integrated: Some(-23.012),
        sample_peak: Some(-9.0309),
        true_peak: Some(-6.08),
        measured: Duration::from_millis(61_234),
    });
    let v = summary(&snap);
    let l = &v["output"]["loudness"];
    assert_eq!(l["integrated_lufs"], -23.0);
    assert_eq!(l["momentary_lufs"], -23.0);
    assert_eq!(l["short_term_lufs"], -23.0);
    assert_eq!(l["sample_peak_dbfs"], -9.0);
    assert_eq!(l["true_peak_dbtp"], -6.1);
    assert_eq!(l["seconds"], 61.23);
    // Digital silence: no reading anywhere, as null rather than -inf.
    snap.output.loudness = Some(ac_audio::LoudnessReading {
        measured: Duration::from_secs(2),
        ..Default::default()
    });
    let v = summary(&snap);
    let l = &v["output"]["loudness"];
    for key in [
        "integrated_lufs",
        "momentary_lufs",
        "short_term_lufs",
        "sample_peak_dbfs",
        "true_peak_dbtp",
    ] {
        assert!(l[key].is_null(), "{key}: {l}");
    }
    assert_eq!(l["seconds"], 2.0);
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

/// The output of a session that records and streams, in `state`, with
/// the stream's health `stream` (`None`: the session only records).
fn streaming(state: OutputState, stream: Option<ac_output::StreamHealth>) -> OutputView {
    let stats = ac_output::EncoderStats {
        encoder: ac_output::EncoderChoice::Libx264,
        frames_in: 30,
        frames_dropped: 0,
        frames_repeated: 0,
        frames_written: 30,
        audio_samples_in: 0,
        audio_samples_dropped: 0,
        audio_silence_samples: 0,
        audio_samples_written: 0,
        audio_connected: false,
        ffmpeg_alive: state == OutputState::Running,
        exit_code: None,
        last_error: None,
        progress: Default::default(),
        stream,
    };
    OutputView {
        state,
        record_path: Some(PathBuf::from("/rec/program.mp4")),
        stats: Some(stats),
        ..OutputView::default()
    }
}

#[test]
fn the_stream_reads_as_the_running_output_saw_it_with_the_sessions_counts() {
    use ac_output::{StreamHealth, StreamState};
    let health = |state, outages, reconnects, bitrate_kbps| StreamHealth {
        state,
        stalled: false,
        outages,
        reconnects,
        bitrate_kbps,
    };
    let with = |output: OutputView| {
        let mut snap = (*preparing_snapshot()).clone();
        snap.output = output;
        Arc::new(snap)
    };
    let summary = |sampler: &Sampler, fin: &EngineSnapshot| {
        let s = summarize(
            Path::new("x.toml"),
            &options(1.0, Some("/rec/program.mp4".into())),
            Instant::now(),
            None,
            Totals {
                ticks: 0,
                threads_joined: 0,
                trace: None,
            },
            sampler.last.as_deref(),
            Some(fin),
            sampler,
        );
        serde_json::to_value(&s).unwrap()["output"]["stream"].clone()
    };

    let mut sampler = Sampler::default();
    let retrying = health(StreamState::Reconnecting { attempt: 2 }, 1, 0, Some(0.0));
    sampler.record(&with(streaming(OutputState::Running, Some(retrying))));
    let fin = with(streaming(OutputState::Finished, Some(retrying)));
    let v = summary(&sampler, &fin);
    assert_eq!(v["state"], "reconnecting");
    assert_eq!(v["attempt"], 2);
    assert_eq!(v["bitrate_kbps"], 0.0);

    let up = health(StreamState::Connected, 1, 1, Some(812.04));
    sampler.record(&with(streaming(OutputState::Running, Some(up))));
    // Stopping, and then finished, the stream is down: not what it was.
    let down = health(StreamState::Down, 2, 1, Some(0.0));
    sampler.record(&with(streaming(OutputState::Stopping, Some(down))));
    let fin = with(streaming(OutputState::Finished, Some(down)));
    let v = summary(&sampler, &fin);
    assert_eq!(
        v,
        serde_json::json!({
            "state": "connected",
            "attempt": null,
            "stalled": false,
            // The outage after the last running sample counts.
            "outages": 2,
            "reconnects": 1,
            "bitrate_kbps": 812.0,
        })
    );

    // Without a running sample, the end is all there is.
    let v = summary(&Sampler::default(), &fin);
    assert_eq!((&v["state"], &v["outages"]), (&"down".into(), &2.into()));
    // A session that only records has no stream.
    let mut sampler = Sampler::default();
    sampler.record(&with(streaming(OutputState::Running, None)));
    let fin = with(streaming(OutputState::Finished, None));
    assert!(summary(&sampler, &fin).is_null());
}
