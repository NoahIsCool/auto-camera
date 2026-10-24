//! Tests for the studio configuration.

use super::*;

/// The smallest valid config.
const MINIMAL: &str = r#"
[[sources]]
id = "stage"
kind = "synthetic"
width = 640
height = 360
fps = 30
"#;

/// [`MINIMAL`], parsed.
fn minimal() -> StudioConfig {
    StudioConfig::parse(MINIMAL, false).unwrap()
}

#[test]
fn minimal_config_fills_every_default() {
    let c = minimal();
    c.validate().unwrap();
    assert_eq!(c.sources.len(), 1);
    assert_eq!(c.sources[0].display_name(), "stage");
    assert_eq!(
        c.sources[0].input,
        SourceConfig::Synthetic {
            width: 640,
            height: 360,
            fps: 30.0,
            subjects: 2
        }
    );
    assert!(c.queries.is_empty());
    assert_eq!(c.detector, DetectorSettings::None);
    assert_eq!(c.output, OutputSettings::default());
    assert_eq!(c.tracker, TemplateTrackerConfig::default());
    assert_eq!(c.framing, FramerConfig::default());
    assert_eq!(c.frame_period(), Duration::from_secs_f64(1.0 / 30.0));
}

#[test]
fn full_config_parses_every_section() {
    let text = r#"
[[sources]]
id = "cam-1"
name = "Stage"
kind = "file"
path = "clips/rehearsal.mp4"
loop_playback = true
max_width = 1280

[[queries]]
name = "Lead Singer"
prompt = "person singing"
presets = ["medium", { id = "tight", name = "Tight", size = "close", headroom = 0.02, max_zoom = 6.0 }]

[detector]
kind = "owl"
model = "models/owlv2.onnx"
tokenizer = "models/tokenizer.json"
people_first = true
person_threshold = 0.2

[discovery]
rate_hz = 2.0

[tracker]
work_width = 320

[subjects]
max_subjects = 4
lost_after_s = 1.5

[framing]
pan_half_life = 1

[director]
mode = "auto"
profile = "music"

[switcher]
transition = "dip"
duration = 1.2

[output]
width = 1280
height = 720
audio_source = "cam-1"
record_path = "/tmp/show.mp4"

[monitors]
thumb_rate_hz = 5.0
"#;
    let c = StudioConfig::parse(text, false).unwrap();
    c.validate().unwrap();
    let q = &c.queries[0];
    assert_eq!(q.query_id(), QueryId("lead-singer".into()));
    let presets: Vec<Preset> = q.presets.iter().map(PresetSpec::to_preset).collect();
    assert_eq!(presets[0], preset_for(ShotSize::Medium));
    assert_eq!(presets[1].id, "tight");
    assert_eq!(presets[1].max_zoom, 6.0);
    assert_eq!(c.tracker.work_width, 320);
    assert_eq!(c.tracker.template_size, 32);
    assert_eq!(c.framing.pan_half_life, 1.0);
    assert_eq!(c.director.mode, ac_director::DirectorMode::Auto);
    assert_eq!(
        c.store_config().unwrap().lost_after,
        Duration::from_millis(1500)
    );
    assert_eq!(
        c.switcher.to_transition().kind,
        ac_switcher::TransitionKind::Dip
    );
    assert_eq!(c.monitors.thumb_rate_hz, 5.0);
    assert!(matches!(
        c.detector,
        DetectorSettings::Owl {
            people_first: Some(true),
            person_threshold: Some(t),
            ..
        } if t == 0.2
    ));
    assert_eq!(
        c.store_config().unwrap().min_score,
        0.2,
        "the store floor follows the person threshold"
    );
}

/// An OWL detector section, with `extra` lines appended.
fn owl_config(extra: &str) -> StudioConfig {
    let text = format!(
        "{MINIMAL}\n[detector]\nkind = \"owl\"\nmodel = \"m.onnx\"\ntokenizer = \"t.json\"\n{extra}"
    );
    StudioConfig::parse(&text, false).unwrap()
}

#[test]
fn the_store_floor_admits_person_scored_detections() {
    let store_default = ac_subjects::StoreConfig::default().min_score;
    assert_eq!(minimal().store_config().unwrap().min_score, store_default);
    assert_eq!(owl_config("").store_config().unwrap().min_score, 0.15);
    assert_eq!(
        owl_config("person_threshold = 0.5")
            .store_config()
            .unwrap()
            .min_score,
        store_default,
        "a floor above the store default changes nothing"
    );
    assert_eq!(
        owl_config("people_first = false")
            .store_config()
            .unwrap()
            .min_score,
        store_default
    );
    let mut explicit = owl_config("");
    explicit.subjects.min_score = Some(0.4);
    assert_eq!(explicit.store_config().unwrap().min_score, 0.4, "set wins");
    explicit.subjects.min_score = Some(f32::NAN);
    assert!(explicit.store_config().is_err());
}

#[test]
fn json_configs_parse_too() {
    let json = r#"{"sources": [{"id": "a", "kind": "synthetic", "width": 64, "height": 36, "fps": 25}],
                   "detector": {"kind": "synthetic"}}"#;
    let c = StudioConfig::parse(json, true).unwrap();
    c.validate().unwrap();
    assert_eq!(c.detector, DetectorSettings::Synthetic { score: 0.9 });
}

#[test]
fn typos_are_errors_not_silently_ignored() {
    for (section, text) in [
        ("top level", format!("{MINIMAL}\nmonitor = 1\n")),
        ("output", format!("{MINIMAL}\n[output]\nwidht = 1280\n")),
        ("tracker", format!("{MINIMAL}\n[tracker]\nwork_widht = 1\n")),
        ("framing", format!("{MINIMAL}\n[framing]\npan = 1.0\n")),
        (
            "source",
            "[[sources]]\nid = \"a\"\nkind = \"synthetic\"\nwidth = 64\nheight = 36\nfps = 30\nsubject = 3\n".into(),
        ),
    ] {
        assert!(
            StudioConfig::parse(&text, false).is_err(),
            "{section} typo accepted"
        );
    }
}

/// A named way to break a valid config.
type Breakage = (&'static str, Box<dyn Fn(&mut StudioConfig)>);

#[test]
fn invalid_values_are_rejected_with_a_message() {
    let cases: Vec<Breakage> = vec![
        ("no sources", Box::new(|c| c.sources.clear())),
        ("bad id", Box::new(|c| c.sources[0].id = "a b".into())),
        (
            "duplicate source",
            Box::new(|c| {
                let s = c.sources[0].clone();
                c.sources.push(s);
            }),
        ),
        ("odd width", Box::new(|c| c.output.width = 1281)),
        ("zero fps", Box::new(|c| c.output.fps = 0)),
        ("rate", Box::new(|c| c.discovery.rate_hz = 0.0)),
        ("lost", Box::new(|c| c.subjects.lost_after_s = -1.0)),
        ("cap", Box::new(|c| c.subjects.max_subjects = 99)),
        ("tracker", Box::new(|c| c.tracker.work_width = 0)),
        ("framing", Box::new(|c| c.framing.pan_half_life = f64::NAN)),
        ("duration", Box::new(|c| c.switcher.duration = 0.0)),
        ("monitor", Box::new(|c| c.monitors.width = 4)),
        (
            "audio source",
            Box::new(|c| c.output.audio_source = Some("nope".into())),
        ),
        (
            "score",
            Box::new(|c| c.detector = DetectorSettings::Synthetic { score: 1.5 }),
        ),
        (
            "person threshold",
            Box::new(|c| {
                c.detector = DetectorSettings::Owl {
                    model: "m.onnx".into(),
                    text_encoder: None,
                    tokenizer: "t.json".into(),
                    input_size: None,
                    score_threshold: None,
                    use_coreml: None,
                    people_first: None,
                    person_threshold: Some(1.5),
                }
            }),
        ),
        (
            "empty prompt",
            Box::new(|c| {
                c.queries.push(QuerySettings {
                    id: None,
                    name: "P".into(),
                    prompt: " ".into(),
                    presets: default_presets(),
                })
            }),
        ),
        (
            "wide preset",
            Box::new(|c| {
                c.queries.push(QuerySettings {
                    id: None,
                    name: "P".into(),
                    prompt: "person".into(),
                    presets: vec![PresetSpec::Size(ShotSize::Wide)],
                })
            }),
        ),
        (
            "duplicate query",
            Box::new(|c| {
                let q = QuerySettings {
                    id: None,
                    name: "P".into(),
                    prompt: "person".into(),
                    presets: default_presets(),
                };
                c.queries.push(q.clone());
                c.queries.push(q);
            }),
        ),
    ];
    for (name, mutate) in cases {
        let mut c = minimal();
        mutate(&mut c);
        match c.validate() {
            Err(ConfigError::Invalid(msg)) => assert!(!msg.is_empty(), "{name}"),
            other => panic!("{name}: expected invalid, got {other:?}"),
        }
    }
}

#[test]
fn synthetic_detector_needs_synthetic_sources() {
    let mut c = StudioConfig::with_source(
        "f",
        SourceConfig::File {
            path: "a.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: true,
        },
    );
    c.validate().unwrap();
    c.detector = DetectorSettings::Synthetic { score: 0.9 };
    assert!(matches!(c.validate(), Err(ConfigError::Invalid(_))));
}

#[test]
fn load_resolves_relative_paths_and_reports_errors() {
    let dir = std::env::temp_dir().join(format!("ac-engine-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("studio.toml");
    std::fs::write(
        &path,
        "[[sources]]\nid = \"f\"\nkind = \"file\"\npath = \"clip.mp4\"\n",
    )
    .unwrap();
    let c = StudioConfig::load(&path).unwrap();
    match &c.sources[0].input {
        SourceConfig::File { path, .. } => assert_eq!(path, &dir.join("clip.mp4")),
        other => panic!("{other:?}"),
    }
    std::fs::write(&path, "[[sources]]\nid = 3\n").unwrap();
    assert!(matches!(
        StudioConfig::load(&path),
        Err(ConfigError::Parse { .. })
    ));
    assert!(matches!(
        StudioConfig::load(&dir.join("missing.toml")),
        Err(ConfigError::Io { .. })
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn slugs_are_stable_ids() {
    assert_eq!(slug("Lead Singer"), "lead-singer");
    assert_eq!(slug("  Pastor's mic! "), "pastor-s-mic");
    assert_eq!(slug("ÄÖ"), "query");
    assert!(valid_id(&slug("anything at all")));
    assert!(!valid_id(""));
    assert!(!valid_id("a/b"));
}

#[test]
fn round_trips_through_toml() {
    let mut c = minimal();
    c.queries.push(QuerySettings {
        id: Some("p".into()),
        name: "Person".into(),
        prompt: "person".into(),
        presets: default_presets(),
    });
    let text = toml::to_string(&c).unwrap();
    let back = StudioConfig::parse(&text, false).unwrap();
    assert_eq!(back, c);
}
