//! Headless runs on small synthetic studios (real time, about a second each).

use super::*;
use ac_core::{ShotSize, TrackState};
use ac_engine::{
    DetectorSettings, EngineCore, OutputView, PresetSpec, QuerySettings, ShotView, TimingView,
};
use ac_media::SourceInfo;
use ac_sources::SourceConfig;

/// A small synthetic studio with ground-truth discovery and no director.
fn studio() -> StudioConfig {
    let mut c = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 2,
        },
    );
    c.detector = DetectorSettings::Synthetic { score: 0.9 };
    c.discovery.rate_hz = 10.0;
    c.queries.push(QuerySettings {
        id: None,
        name: "Person".into(),
        prompt: "person".into(),
        presets: Some(vec![PresetSpec::Size(ShotSize::Medium)]),
        literal: false,
    });
    c.output.width = 320;
    c.output.height = 180;
    // Not a test of the encoder: libx264 behaves the same everywhere.
    c.output.encoder = ac_engine::EncoderSetting::Libx264;
    c.director.mode = DirectorMode::Off;
    c
}

/// Run options.
fn options(seconds: f64, record: Option<PathBuf>) -> HeadlessOptions {
    HeadlessOptions { seconds, record }
}

/// The snapshot of [`studio`] before its first tick, built by a real core:
/// its detector is still preparing.
fn preparing_snapshot() -> Arc<EngineSnapshot> {
    let info = SourceInfo {
        id: ac_core::SourceId("stage".into()),
        width: 320,
        height: 180,
        fps: 30.0,
        audio: None,
    };
    let core = EngineCore::new(&studio(), vec![info], &[true]).unwrap();
    Arc::new(core.snapshot(OutputView::default(), TimingView::default(), true))
}

/// True when `tool -version` succeeds. When it does not and
/// `AC_REQUIRE_FFMPEG` is set (CI), panics instead of letting the caller
/// skip.
fn have(tool: &str) -> bool {
    let found = std::process::Command::new(tool)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let required = std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
    assert!(
        found || !required,
        "{tool} is not installed, and AC_REQUIRE_FFMPEG says it must be"
    );
    found
}

/// A fresh, unique path in the temp directory (not created).
fn temp_path(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("auto-camera-{}-{nanos}-{name}", std::process::id()))
}

#[test]
fn a_run_without_a_director_puts_the_wide_on_program() {
    let s = run(studio(), Path::new("studio.toml"), &options(0.6, None));
    assert!(s.ok, "{:?}", s.error);
    assert!(s.ticks > 10);
    assert_eq!(s.program.last.as_deref(), Some("stage/wide"));
    assert_eq!(s.program.log[0].1, "stage/wide");
    assert!(s.shots.iter().any(|v| v.id == "stage/wide"));
    assert_eq!(s.max_subjects, 2);
    assert_eq!(s.sources[0].size, [320, 180]);
    assert!(s.sources[0].discovery_applied > 0);
    assert!(!s.sources[0].discovery_preparing);
    let ready = s.sources[0]
        .detector_ready_s
        .expect("the detector was ready");
    assert!((0.0..s.media_seconds).contains(&ready), "{ready}");
    assert_eq!(s.subjects.len(), 2);
    for (subject, id) in s.subjects.iter().zip([1, 2]) {
        assert_eq!((subject.id, subject.source.as_str()), (id, "stage"));
        assert_eq!(
            (subject.query.as_str(), subject.state),
            ("person", TrackState::Tracking)
        );
        assert!(subject.aliases.is_empty());
        assert!(
            subject.bbox[2] > 0.0 && subject.bbox[3] > 0.0,
            "{subject:?}"
        );
    }
    assert!(s.output.is_none());
    assert_eq!(s.threads_joined, 1, "the discovery worker");
    assert_eq!(s.director.mode, DirectorMode::Off);
    assert!((s.timing.budget_ms - 33.33).abs() < 0.01);
}

#[test]
fn a_run_with_the_auto_director_switches_by_itself() {
    let mut c = studio();
    c.director.mode = DirectorMode::Auto;
    // The establishing take mixes up from black over 0.8 s.
    let s = run(c, Path::new("studio.toml"), &options(1.5, None));
    assert!(s.ok, "{:?}", s.error);
    assert!(s.program.changes >= 1);
    assert!(!s.program.log.is_empty());
    assert!(s.director.takes >= 1);
}

#[test]
fn unusable_setups_fail_with_a_summary() {
    let missing = StudioConfig::with_source(
        "f",
        SourceConfig::File {
            path: "/nonexistent/auto-camera/clip.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: false,
        },
    );
    let s = run(missing, Path::new("x.toml"), &options(1.0, None));
    assert!(!s.ok);
    assert!(s.error.unwrap().contains("did not start"));
    assert!(s.sources.is_empty());

    let existing = temp_path("existing.mp4");
    std::fs::write(&existing, b"keep me").unwrap();
    let s = run(
        studio(),
        Path::new("x.toml"),
        &options(1.0, Some(existing.clone())),
    );
    assert!(!s.ok);
    assert!(
        s.error.as_deref().unwrap().contains("already exists"),
        "{:?}",
        s.error
    );
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
    std::fs::remove_file(existing).unwrap();
}

/// Real time, except that the engine thread's clock reading number `fuse`
/// panics: a stand-in for any bug inside a tick.
#[derive(Debug)]
struct Grenade {
    /// The instant that reads as zero.
    origin: Instant,
    /// Readings taken on the engine thread so far.
    calls: std::sync::atomic::AtomicU64,
    /// Reading that panics.
    fuse: u64,
}

impl ac_sources::Clock for Grenade {
    fn now(&self) -> Duration {
        if std::thread::current().name() == Some("ac-engine")
            && self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == self.fuse
        {
            panic!("injected failure inside a tick");
        }
        self.origin.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[test]
fn a_run_whose_engine_dies_fails_at_once_with_the_reason() {
    let clock = Arc::new(Grenade {
        origin: Instant::now(),
        calls: Default::default(),
        fuse: 200,
    });
    let started = Instant::now();
    let s = run_with(
        studio(),
        Path::new("x.toml"),
        &options(30.0, None),
        EngineOptions {
            clock,
            ..EngineOptions::default()
        },
    );
    assert!(!s.ok);
    let error = s.error.as_deref().unwrap_or_default();
    assert!(error.contains("injected failure"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the run waited out its time: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_recording_run_finalizes_the_file() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let path = temp_path("headless.mp4");
    let s = run(
        studio(),
        Path::new("x.toml"),
        &options(1.0, Some(path.clone())),
    );
    assert!(s.ok, "{:?}", s.error);
    let out = s.output.as_ref().expect("output summary");
    assert_eq!(out.state, "finished");
    assert_eq!(out.exit_code, Some(0));
    assert!(out.frames_written >= 20, "{out:?}");
    assert_eq!(out.record_path.as_deref(), Some(path.as_path()));
    assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 0));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn the_summary_is_json() {
    let mut config = studio();
    // No presets: the shot words decide, and the role word is rewritten.
    config
        .queries
        .push(QuerySettings::new("Speaker", "speaker upper body"));
    let s = run(config, Path::new("studio.toml"), &options(0.5, None));
    let v: serde_json::Value = serde_json::to_value(&s).unwrap();
    for key in [
        "ok", "ticks", "sources", "queries", "shots", "subjects", "program", "director", "timing",
        "output",
    ] {
        assert!(v.get(key).is_some(), "{key}");
    }
    assert_eq!(v["director"]["mode"], "off");
    assert_eq!(v["shots"][0]["status"], "wide");
    assert_eq!(v["shots"][0]["limited_by_zoom"], false);
    assert!(v["shots"][0]["same_picture_as"].is_null());
    let plain = &v["queries"][0];
    assert_eq!(plain["prompt"], "person");
    assert_eq!(plain["detector_phrase"], "person");
    assert_eq!(plain["presets"], serde_json::json!(["medium"]));
    assert_eq!(plain["notes"], serde_json::json!([]));
    let speaker = &v["queries"][1];
    assert_eq!(speaker["prompt"], "speaker upper body");
    assert_eq!(speaker["detector_phrase"], "a person speaking");
    assert_eq!(
        speaker["shot_words"],
        serde_json::json!(["medium", "medium_close"])
    );
    assert_eq!(
        speaker["presets"],
        serde_json::json!(["medium", "medium_close"])
    );
    assert_eq!(speaker["basis"], "person");
    assert_eq!(speaker["literal"], false);
    assert_eq!(
        speaker["parse"],
        "Detector sees: a person speaking · Framing: Medium, Medium close (person)"
    );
    // The role is an action, which the notes say.
    let notes = speaker["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    let note = notes[0].as_str().unwrap();
    assert!(
        note.starts_with("\"speaker\" asks the detector for \"a person speaking\", an action")
            && note.ends_with("take the speaker's shot by hand"),
        "{note}"
    );
    assert!(v["output"].is_null());
    let source = &v["sources"][0];
    assert_eq!(source["discovery_preparing"], false);
    assert!(source["detector_ready_s"].is_number());
    let wide = &v["shots"][0];
    for key in [
        "crop_width_min",
        "crop_width_max",
        "crop_height_min",
        "crop_height_max",
    ] {
        assert_eq!(wide[key], 1.0, "{key}");
    }
    let subject = &v["subjects"][0];
    assert_eq!(subject["id"], 1);
    assert_eq!(subject["source"], "stage");
    assert_eq!(subject["query"], "person");
    // The synthetic detector boxes every subject for every prompt, so the
    // person also answers the speaker query.
    assert_eq!(subject["aliases"], serde_json::json!(["speaker"]));
    assert_eq!(subject["state"], "tracking");
    assert_eq!(subject["bbox"].as_array().map(Vec::len), Some(4));
}

/// A real run reports a crop range for every subject shot. Whether a range
/// is wider than one size depends on how the tracker's scale search drifts
/// on the runner, so the widening itself is checked on scripted snapshots
/// ([`the_sampler_keeps_each_shots_narrowest_and_widest_crop`]).
#[test]
fn moving_subjects_report_the_range_of_their_crop_sizes() {
    let s = run(studio(), Path::new("studio.toml"), &options(2.0, None));
    assert!(s.ok, "{:?}", s.error);
    let subject_shots: Vec<&ShotSummary> =
        s.shots.iter().filter(|v| v.id != "stage/wide").collect();
    assert_eq!(subject_shots.len(), 2, "{:?}", s.shots);
    for v in &subject_shots {
        assert!(
            0.0 < v.crop_width_min && v.crop_width_min <= v.crop_width_max,
            "{v:?}"
        );
        assert!(
            0.0 < v.crop_height_min && v.crop_height_min <= v.crop_height_max,
            "{v:?}"
        );
        assert!(v.crop_width_max <= 1.0 && v.crop_height_max <= 1.0, "{v:?}");
    }
}

/// Every sampled snapshot widens a shot's range to take in its crop, and
/// the summary reports that range.
#[test]
fn the_sampler_keeps_each_shots_narrowest_and_widest_crop() {
    let mut snap = (*preparing_snapshot()).clone();
    let mut sampler = Sampler::default();
    for (w, h) in [(0.5, 0.4), (0.3, 0.6), (0.4, 0.5)] {
        snap.shots[0].crop = Rect::new(0.1, 0.1, w, h);
        sampler.record(&Arc::new(snap.clone()));
    }
    let id = snap.shots[0].id.0.clone();
    let range = sampler.crops[&id];
    assert_eq!((range.width, range.height), ([0.3, 0.5], [0.4, 0.6]));
    // A snapshot after shutdown is not sampled.
    snap.running = false;
    snap.shots[0].crop = Rect::new(0.0, 0.0, 1.0, 1.0);
    sampler.record(&Arc::new(snap));
    assert_eq!(sampler.crops[&id], range);
}

#[test]
fn a_run_that_ends_while_the_detector_loads_says_so() {
    let mut snap = (*preparing_snapshot()).clone();
    let summary = |snap: &EngineSnapshot| {
        let s = summarize(
            Path::new("x.toml"),
            &options(1.0, None),
            Instant::now(),
            Some("stopped".into()),
            Totals {
                ticks: 0,
                threads_joined: 0,
            },
            Some(snap),
            Some(snap),
            &Sampler::default(),
        );
        serde_json::to_value(&s).unwrap()
    };
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
    let s = summarize(
        Path::new("x.toml"),
        &options(1.0, None),
        Instant::now(),
        None,
        Totals {
            ticks: 0,
            threads_joined: 0,
        },
        Some(&snap),
        Some(&snap),
        &Sampler::default(),
    );
    let v = serde_json::to_value(&s).unwrap();
    let shots = v["shots"].as_array().unwrap();
    assert_eq!(shots.len(), 3);
    assert!(shots[0]["same_picture_as"].is_null());
    assert!(shots[1]["same_picture_as"].is_null());
    assert_eq!(shots[1]["limited_by_zoom"], true);
    assert_eq!(shots[2]["same_picture_as"], "stage/1/medium");
    assert_eq!(shots[2]["limited_by_zoom"], true);
}

#[test]
fn the_detector_phase_waits_for_every_detector_and_gives_up_in_time() {
    let preparing = preparing_snapshot();
    let mut ready = (*preparing).clone();
    ready.sources[0].discovery.preparing = false;
    let ready = Arc::new(ready);

    // Ready on the fourth look.
    let mut looks = 0;
    let mut sampler = Sampler::default();
    let next = || {
        looks += 1;
        if looks < 4 {
            preparing.clone()
        } else {
            ready.clone()
        }
    };
    assert_eq!(
        await_detectors(next, &mut sampler, Duration::from_secs(10)),
        Ok(())
    );
    assert_eq!(looks, 4);
    assert!(sampler.last.is_some(), "the wait samples");

    // Never ready.
    let started = Instant::now();
    let error = await_detectors(
        || preparing.clone(),
        &mut Sampler::default(),
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert_eq!(error, "the detector was still loading after 0.1 s");
    assert!(started.elapsed() < Duration::from_secs(5));

    // The engine died while the detector loaded.
    let mut dead = (*preparing).clone();
    dead.running = false;
    dead.fatal = Some("the engine thread panicked: boom".into());
    let error = await_detectors(
        || Arc::new(dead.clone()),
        &mut Sampler::default(),
        Duration::from_secs(10),
    )
    .unwrap_err();
    assert!(
        error.contains("stopped early") && error.contains("boom"),
        "{error}"
    );

    // Without discovery nothing prepares, so there is nothing to wait for.
    let mut none = (*preparing).clone();
    none.sources[0].discovery_enabled = false;
    none.sources[0].discovery.preparing = false;
    let none = Arc::new(none);
    assert_eq!(
        await_detectors(|| none.clone(), &mut Sampler::default(), Duration::ZERO),
        Ok(())
    );
}

#[test]
fn rounding_keeps_the_requested_digits() {
    assert_eq!(round(1.23456, 2), 1.23);
    assert_eq!(round(-0.005, 2), -0.01);
    assert_eq!(round(7.0, 0), 7.0);
}

#[test]
fn the_sample_configs_load() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let demo = StudioConfig::load(&root.join("demo.toml")).unwrap();
    assert_eq!(demo.sources.len(), 1);
    assert_eq!(demo.detector, DetectorSettings::Synthetic { score: 0.9 });
    // Auto-take is opt-in: the demo only suggests.
    assert_eq!(demo.director.mode, DirectorMode::Suggest);
    assert_eq!(demo.discovery.rate_hz, 10.0);
    let file = StudioConfig::load(&root.join("file-source.example.toml")).unwrap();
    assert_eq!(file.queries.len(), 3);
    let queries = file.build_queries().unwrap();
    // The prompts without shot or role words go to the detector as typed.
    assert_eq!(
        queries[0].detector_phrase(),
        "a person speaking at a lectern"
    );
    assert_eq!(queries[1].detector_phrase(), "a person playing guitar");
    // "drummer wide": the role word is rewritten, the shot word picks Full.
    assert_eq!(queries[2].detector_phrase(), "a person playing drums");
    let sizes: Vec<ShotSize> = queries[2].presets.iter().map(|p| p.size).collect();
    assert_eq!(sizes, [ShotSize::Full]);
    let SourceConfig::File { path, .. } = &file.sources[0].input else {
        panic!("a file source");
    };
    assert!(path.is_absolute() && path.ends_with("media/rehearsal.mp4"));
}
