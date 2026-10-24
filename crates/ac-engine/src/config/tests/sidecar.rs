//! Tests for the `[detector] kind = "sidecar"` section as part of a studio:
//! parsing, defaults, typos, the store floor, the deadline check and path
//! resolution on load.

use super::*;

/// [`MINIMAL`] with a sidecar detector section holding `keys`.
fn sidecar(keys: &str) -> Result<StudioConfig, String> {
    StudioConfig::parse(
        &format!("{MINIMAL}\n[detector]\nkind = \"sidecar\"\n{keys}\n"),
        false,
    )
}

/// The sidecar settings of `c`.
fn settings(c: &StudioConfig) -> &SidecarSettings {
    match &c.detector {
        DetectorSettings::Sidecar(s) => s,
        other => panic!("not a sidecar: {other:?}"),
    }
}

/// `c` must fail validation; returns the message.
fn invalid(c: &StudioConfig) -> String {
    match c.validate() {
        Err(ConfigError::Invalid(msg)) => msg,
        other => panic!("expected invalid, got {other:?}"),
    }
}

#[test]
fn a_sidecar_section_parses_with_defaults() {
    let c = sidecar(r#"command = ["python3", "server.py", "--backend", "fake"]"#).unwrap();
    c.validate().unwrap();
    let s = settings(&c);
    assert_eq!(
        s,
        &SidecarSettings::new(["python3", "server.py", "--backend", "fake"])
    );
    assert_eq!(
        (
            s.send_side,
            s.startup_timeout_s,
            s.request_timeout_s,
            s.score_threshold
        ),
        (1008, 60.0, 5.0, 0.5)
    );
    let c = sidecar(
        r#"command = ["py", "s.py"]
send_side = 672
startup_timeout_s = 90
request_timeout_s = 3.5
score_threshold = 0.3"#,
    )
    .unwrap();
    c.validate().unwrap();
    let s = settings(&c);
    assert_eq!(
        (
            s.send_side,
            s.startup_timeout_s,
            s.request_timeout_s,
            s.score_threshold
        ),
        (672, 90.0, 3.5, 0.3)
    );
}

#[test]
fn a_missing_command_or_a_typo_is_a_parse_error() {
    let missing = sidecar("send_side = 672").unwrap_err();
    assert!(missing.contains("command"), "{missing}");
    let typo = sidecar("command = [\"py\"]\nscore_treshold = 0.3").unwrap_err();
    assert!(typo.contains("score_treshold"), "{typo}");
    let owl_key = sidecar("command = [\"py\"]\nmodel = \"m.onnx\"").unwrap_err();
    assert!(owl_key.contains("model"), "{owl_key}");
    assert!(
        sidecar("command = \"python3 server.py\"").is_err(),
        "argv, not a shell line"
    );
}

#[test]
fn bad_sidecar_values_are_rejected_with_their_key() {
    for (keys, key) in [
        ("command = []", "detector.command"),
        ("command = [\"py\", \"\"]", "detector.command[1]"),
        ("command = [\"py\"]\nsend_side = 0", "detector.send_side"),
        ("command = [\"py\"]\nsend_side = 5000", "detector.send_side"),
        (
            "command = [\"py\"]\nstartup_timeout_s = -1",
            "detector.startup_timeout_s",
        ),
        (
            "command = [\"py\"]\nrequest_timeout_s = 0",
            "detector.request_timeout_s",
        ),
        (
            "command = [\"py\"]\nscore_threshold = 1.5",
            "detector.score_threshold",
        ),
    ] {
        let msg = invalid(&sidecar(keys).unwrap());
        assert!(msg.contains(key), "{keys}: {msg}");
    }
}

#[test]
fn the_request_deadline_must_be_below_the_engines() {
    let mut c = sidecar("command = [\"py\"]\nrequest_timeout_s = 10").unwrap();
    let msg = invalid(&c);
    assert!(
        msg.contains("detector.request_timeout_s") && msg.contains("discovery.timeout_s"),
        "{msg}"
    );
    c.discovery.timeout_s = 10.5;
    c.validate().unwrap();
}

#[test]
fn the_store_floor_follows_a_low_sidecar_threshold() {
    let store_default = ac_subjects::StoreConfig::default().min_score;
    let at = |t: &str| {
        sidecar(&format!("command = [\"py\"]\nscore_threshold = {t}"))
            .unwrap()
            .store_config()
            .unwrap()
            .min_score
    };
    assert_eq!(at("0.5"), store_default, "0.5 is above the 0.35 default");
    assert_eq!(at("0.3"), 0.3, "boxes from 0.3 up are kept");
    assert_eq!(at("0.0"), 0.0);
    let mut explicit = sidecar("command = [\"py\"]\nscore_threshold = 0.2").unwrap();
    explicit.subjects.min_score = Some(0.4);
    assert_eq!(explicit.store_config().unwrap().min_score, 0.4, "set wins");
}

#[test]
fn load_resolves_the_command_against_the_config_file() {
    let dir = std::env::temp_dir().join(format!("ac-engine-sidecar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("studio.toml");
    std::fs::write(
        &path,
        "[[sources]]\nid = \"s\"\nkind = \"synthetic\"\nwidth = 64\nheight = 36\nfps = 30\n\
         [detector]\nkind = \"sidecar\"\n\
         command = [\"../venv/bin/python\", \"server.py\", \"--model\", \"m/sam\", \"--side\", \"672\"]\n",
    )
    .unwrap();
    let c = StudioConfig::load(&path).unwrap();
    let command = &settings(&c).command;
    assert_eq!(command[0], dir.join("../venv/bin/python").to_str().unwrap());
    assert_eq!(command[1], "server.py", "no slash: as written");
    assert_eq!(command[3], dir.join("m/sam").to_str().unwrap());
    assert_eq!(
        command[2..],
        [
            "--model".to_string(),
            command[3].clone(),
            "--side".into(),
            "672".into()
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_sidecar_studio_round_trips_through_toml_and_json() {
    let mut c = minimal();
    c.detector = DetectorSettings::Sidecar(SidecarSettings {
        score_threshold: 0.3,
        ..SidecarSettings::new(["python3", "../server.py", "--backend", "fake"])
    });
    let text = toml::to_string(&c).unwrap();
    assert!(text.contains("kind = \"sidecar\""), "{text}");
    assert_eq!(StudioConfig::parse(&text, false).unwrap(), c);
    let json = serde_json::to_string(&c).unwrap();
    assert_eq!(StudioConfig::parse(&json, true).unwrap(), c);
}
