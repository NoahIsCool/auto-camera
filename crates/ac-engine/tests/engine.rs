//! The engine runtime, headless on a sped-up clock: threads, commands,
//! snapshots and shutdown.

mod common;

use ac_core::{QueryId, ShotId, SubjectId};
use ac_director::DirectorMode;
use ac_engine::{
    CommandError, CommandReply, DetectorSettings, Engine, EngineCommand, EngineError, OutputState,
    StudioConfig,
};
use ac_sources::SourceConfig;
use ac_switcher::SwitchError;

use common::{WAIT, fast_time, start, studio};

/// The shot with `id`.
fn shot(id: &str) -> ShotId {
    ShotId(id.into())
}

#[test]
fn subjects_are_discovered_and_shots_appear() {
    let engine = start(studio(2));
    let snap = engine
        .handle()
        .wait_until(WAIT, |s| s.subjects.len() == 2 && s.shots.len() == 5)
        .expect("two subjects and five shots");
    assert_eq!(snap.shots[0].id, shot("stage/wide"));
    assert!(snap.shot(&shot("stage/1/medium")).is_some());
    assert!(snap.shot(&shot("stage/2/close")).is_some());
    let src = &snap.sources[0];
    assert!(src.discovery_enabled && src.discovery.applied > 0);
    assert!(snap.tick > 0);
    let report = engine.shutdown().unwrap();
    assert!(!report.final_snapshot.running);
    assert!(report.ticks >= snap.tick);
    assert_eq!(report.threads_joined, 1, "the discovery worker");
}

#[test]
fn cut_and_auto_change_program() {
    let engine = start(studio(1));
    let h = engine.handle();
    h.wait_until(WAIT, |s| {
        s.shot(&shot("stage/1/medium")).is_some_and(|v| v.available)
    })
    .expect("subject shot");
    h.send(EngineCommand::SetPreview {
        shot: shot("stage/wide"),
    })
    .unwrap();
    h.send(EngineCommand::Cut).unwrap();
    let snap = h
        .wait_until(WAIT, |s| s.switcher.program == Some(shot("stage/wide")))
        .unwrap();
    assert!(snap.shot(&shot("stage/wide")).unwrap().tally.program);
    h.send(EngineCommand::SetPreview {
        shot: shot("stage/1/medium"),
    })
    .unwrap();
    h.send(EngineCommand::Auto).unwrap();
    let snap = h
        .wait_until(WAIT, |s| {
            s.switcher.program == Some(shot("stage/1/medium")) && !s.switcher.in_transition
        })
        .expect("auto completes");
    assert_eq!(snap.switcher.preview, Some(shot("stage/wide")));
    assert!(snap.program_changes >= 2);
    let medium = snap.shot(&shot("stage/1/medium")).unwrap();
    assert!(medium.tally.program && !medium.tally.preview);
    engine.shutdown().unwrap();
}

#[test]
fn invalid_commands_are_answered_with_errors() {
    let engine = start(studio(1));
    let h = engine.handle();
    assert_eq!(
        h.send(EngineCommand::SetPreview { shot: shot("nope") }),
        Err(CommandError::UnknownShot(shot("nope")))
    );
    assert_eq!(
        h.send(EngineCommand::Auto),
        Err(CommandError::Switch(SwitchError::NoPreview))
    );
    assert_eq!(
        h.send(EngineCommand::StopOutput),
        Err(CommandError::OutputIdle)
    );
    assert!(matches!(
        h.send(EngineCommand::StartOutput {
            rtmp_url: None,
            record_path: None
        }),
        Err(CommandError::Output(_))
    ));
    let pending = h.post(EngineCommand::RemoveQuery {
        query: QueryId("nope".into()),
    });
    assert_eq!(
        pending.wait(WAIT),
        Err(CommandError::UnknownQuery(QueryId("nope".into())))
    );
    engine.shutdown().unwrap();
    assert!(matches!(
        h.send(EngineCommand::Cut),
        Err(CommandError::Unavailable(_))
    ));
}

#[test]
fn protected_subjects_are_not_retired_until_they_leave_the_air() {
    let engine = start(studio(2));
    let h = engine.handle();
    h.wait_until(WAIT, |s| s.shots.len() == 5).unwrap();
    h.send(EngineCommand::SetPreview {
        shot: shot("stage/1/close"),
    })
    .unwrap();
    h.send(EngineCommand::Cut).unwrap();
    h.send(EngineCommand::RemoveQuery {
        query: QueryId("person".into()),
    })
    .unwrap();
    // Subject 2 goes at once; subject 1 is on program and stays.
    let snap = h
        .wait_until(WAIT, |s| s.subjects.len() == 1)
        .expect("unprotected subject retired");
    assert_eq!(snap.subjects[0].id, SubjectId(1));
    assert!(snap.subjects[0].protected);
    assert_eq!(snap.switcher.program, Some(shot("stage/1/close")));
    assert!(snap.shot(&shot("stage/1/close")).is_some());
    // Once off the air, it is retired too.
    h.send(EngineCommand::SetPreview {
        shot: shot("stage/wide"),
    })
    .unwrap();
    h.send(EngineCommand::Cut).unwrap();
    h.send(EngineCommand::SetPreview {
        shot: shot("stage/wide"),
    })
    .unwrap();
    let snap = h
        .wait_until(WAIT, |s| s.subjects.is_empty())
        .expect("retired after leaving the air");
    assert_eq!(snap.shots.len(), 1);
    engine.shutdown().unwrap();
}

#[test]
fn queries_added_at_runtime_create_shots() {
    let mut config = studio(1);
    config.queries.clear();
    let engine = start(config);
    let h = engine.handle();
    assert_eq!(
        h.send(EngineCommand::AddQuery {
            name: "Speaker".into(),
            // No presets: the shot word picks a full shot.
            prompt: "person full".into(),
            presets: None,
            literal: false,
        }),
        Ok(CommandReply::QueryAdded {
            id: QueryId("speaker".into())
        })
    );
    let snap = h
        .wait_until(WAIT, |s| s.shot(&shot("stage/1/full")).is_some())
        .expect("shot for the new query");
    assert_eq!(snap.queries.len(), 1);
    let q = &snap.queries[0];
    assert_eq!(q.prompt, "person full");
    assert_eq!(q.parse.model_phrase, "person");
    assert_eq!(q.parse.shots, [ac_core::ShotSize::Full]);
    assert!(
        snap.shots
            .iter()
            .all(|s| s.preset.as_deref() != Some("medium"))
    );
    engine.shutdown().unwrap();
}

#[test]
fn the_director_in_auto_mode_switches_by_itself() {
    let engine = start(studio(2));
    let h = engine.handle();
    h.send(EngineCommand::SetDirectorMode {
        mode: DirectorMode::Auto,
    })
    .unwrap();
    let snap = h
        .wait_until(WAIT, |s| s.program_changes >= 3)
        .expect("several automatic takes");
    assert_eq!(snap.director.mode, DirectorMode::Auto);
    assert!(snap.director.takes >= 1);
    assert!(!snap.director.reason_text.is_empty());
    engine.shutdown().unwrap();
}

#[test]
fn snapshots_carry_monitors_tiles_sources_and_timing() {
    let engine = start(studio(1));
    let h = engine.handle();
    h.wait_until(WAIT, |s| s.shots.len() == 3).unwrap();
    h.send(EngineCommand::SetPreview {
        shot: shot("stage/wide"),
    })
    .unwrap();
    h.send(EngineCommand::Cut).unwrap();
    h.send(EngineCommand::SetPreview {
        shot: shot("stage/1/close"),
    })
    .unwrap();
    let snap = h
        .wait_until(WAIT, |s| {
            s.preview_monitor.is_some() && s.shots.iter().all(|v| v.thumbnail.is_some())
        })
        .expect("monitors and tiles");
    let pgm = snap.program_monitor.as_ref().unwrap();
    assert_eq!((pgm.width, pgm.height), (160, 90));
    let tile = snap.shots[0].thumbnail.as_ref().unwrap();
    assert_eq!((tile.width, tile.height), (64, 36));
    let src = &snap.sources[0];
    assert!(src.frames > 0 && src.width == 320);
    assert!(snap.timing.tick.count > 0);
    assert!((snap.timing.budget_ms - 1000.0 / 30.0).abs() < 1e-6);
    assert_eq!(snap.output.state, OutputState::Idle);
    assert!(h.snapshot_version() > 1);
    engine.shutdown().unwrap();
}

#[test]
fn dropping_the_engine_shuts_it_down() {
    let engine = start(studio(1));
    let h = engine.handle();
    h.wait_until(WAIT, |s| s.tick > 5).unwrap();
    drop(engine);
    assert!(!h.snapshot().running);
}

#[test]
fn start_reports_unusable_setups() {
    let mut bad = studio(1);
    bad.output.width = 3;
    assert!(matches!(
        Engine::start_with(bad, fast_time()),
        Err(EngineError::Config(_))
    ));
    let missing = StudioConfig::with_source(
        "f",
        SourceConfig::File {
            path: "/nonexistent/auto-camera/clip.mp4".into(),
            loop_playback: false,
            max_width: None,
            audio: false,
        },
    );
    assert!(matches!(
        Engine::start_with(missing, fast_time()),
        Err(EngineError::Source { .. })
    ));
    let mut owl = studio(1);
    owl.detector = DetectorSettings::Owl {
        model: "/nonexistent/owl.onnx".into(),
        text_encoder: None,
        tokenizer: "/nonexistent/tokenizer.json".into(),
        input_size: None,
        score_threshold: None,
        use_coreml: None,
        people_first: None,
        person_threshold: None,
    };
    assert!(matches!(
        Engine::start_with(owl, fast_time()),
        Err(EngineError::Detector { .. })
    ));
    // A sidecar whose program or model folder is missing fails at start,
    // with the reason, before anything is spawned.
    for command in [
        [
            "/nonexistent/venv/bin/python",
            "server.py",
            "--backend",
            "fake",
        ],
        ["sh", "-c", "--model", "/nonexistent/models/sam31-mlx"],
    ] {
        let mut sidecar = studio(1);
        sidecar.detector = DetectorSettings::Sidecar(ac_engine::SidecarSettings::new(command));
        match Engine::start_with(sidecar, fast_time()) {
            Err(EngineError::Sidecar(m)) => assert!(m.contains("/nonexistent/"), "{m}"),
            other => panic!("{command:?}: {other:?}"),
        }
    }
}
