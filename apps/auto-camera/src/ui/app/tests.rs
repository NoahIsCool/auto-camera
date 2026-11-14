//! Tests of the operator window's app: frames, shortcuts and closing.
use super::*;
use crate::testing::{have, temp_path};
use ac_core::ShotId;
use ac_engine::{EngineOptions, MonitorSettings, OutputState};
use ac_output::Caffeinate;
use ac_sources::SourceConfig;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

/// Runs frames until every posted command was answered, then fails with
/// the notice if the last answer was an error (a refused command fails
/// here, with its reason, instead of as a timeout later).
fn settle(app: &mut OperatorApp, ctx: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.pending.is_empty() && Instant::now() < deadline {
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.pending.is_empty(), "commands were not answered");
    if let Some((text, error)) = app.console.notice(&app.handle.snapshot()) {
        assert!(!error, "{text}");
    }
}

/// A one-source synthetic studio at 160x90 with the monitors off.
fn small_studio() -> StudioConfig {
    let mut config = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 160,
            height: 90,
            fps: 30.0,
            subjects: 1,
        },
    );
    config.output.width = 160;
    config.output.height = 90;
    config.monitors = MonitorSettings::disabled();
    config
}

/// Starts `config` with `options` (in real time: on a virtual clock the
/// engine's sleeps would race ahead of the source thread under load,
/// the wide would go stale and a cut would be refused) and waits until
/// the wide is available.
fn start(config: &StudioConfig, options: EngineOptions) -> Engine {
    let engine = Engine::start_with(config.clone(), options).unwrap();
    engine
        .handle()
        .wait_until(Duration::from_secs(10), |s| {
            s.shots.first().is_some_and(|w| w.available)
        })
        .expect("the source delivers frames");
    engine
}

#[test]
fn frames_draw_and_shortcuts_reach_the_engine() {
    let config = small_studio();
    let warning = "studio.toml: no `schema` key".to_owned();
    let mut app = OperatorApp::new(start(&config, EngineOptions::default()), &config)
        .with_warnings(&[warning]);
    assert!(
        app.console
            .notice(&app.handle.snapshot())
            .is_some_and(|(_, error)| !error)
    );
    let ctx = egui::Context::default();
    let key = |key| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let input = egui::RawInput {
        events: vec![key(egui::Key::Num1)],
        ..egui::RawInput::default()
    };
    let _ = ctx.run_ui(input, |ui| app.frame(ui));
    settle(&mut app, &ctx);
    let snap = app
        .handle
        .wait_until(Duration::from_secs(10), |s| s.switcher.preview.is_some())
        .unwrap();
    assert_eq!(snap.switcher.preview, Some(ShotId("stage/wide".into())));
    // Space cuts; the reply is collected on a later frame.
    let input = egui::RawInput {
        events: vec![key(egui::Key::Space)],
        ..egui::RawInput::default()
    };
    let _ = ctx.run_ui(input, |ui| app.frame(ui));
    settle(&mut app, &ctx);
    let snap = app
        .handle
        .wait_until(Duration::from_secs(10), |s| s.switcher.program.is_some())
        .expect("cut to program");
    assert_eq!(snap.switcher.program, Some(ShotId("stage/wide".into())));
    app.shutdown();
    assert!(app.engine.is_none());
    app.shutdown();
    let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
}

/// Draws frames of `app` until `done` holds for its newest output
/// state, for at most 20 s, then one more.
fn draw_until(app: &mut OperatorApp, ctx: &egui::Context, done: fn(&OutputState) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(&app.handle.snapshot().output.state) {
        assert!(
            Instant::now() < deadline,
            "{:?}",
            app.handle.snapshot().output
        );
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
}

#[test]
fn a_session_that_is_not_held_awake_is_noted_once() {
    if !have("ffmpeg") {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let mut config = small_studio();
    config.output.encoder = ac_engine::EncoderSetting::Libx264;
    // As on a platform without caffeinate, whichever this one is.
    let options = EngineOptions {
        keep_awake: Caffeinate::Unsupported,
        ..EngineOptions::default()
    };
    let mut app = OperatorApp::new(start(&config, options), &config);
    let notes = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = notes.clone();
    app.log = Box::new(move |n| sink.borrow_mut().push(n.to_owned()));
    let ctx = egui::Context::default();
    draw_until(&mut app, &ctx, |_| true);
    assert!(notes.borrow().is_empty(), "noted before any output");
    let path = temp_path("window-awake.mp4");
    app.handle
        .send(EngineCommand::StartOutput {
            rtmp_url: None,
            record_path: Some(path.clone()),
        })
        .unwrap();
    draw_until(&mut app, &ctx, |s| *s == OutputState::Running);
    draw_until(&mut app, &ctx, |_| true);
    let noted = notes.borrow().clone();
    assert_eq!(noted.len(), 1, "{noted:?}");
    assert!(noted[0].contains("only on macOS"), "{noted:?}");
    // Not again while the session runs on, nor when it ends.
    app.handle.send(EngineCommand::StopOutput).unwrap();
    draw_until(&mut app, &ctx, |s| *s == OutputState::Finished);
    assert_eq!(notes.borrow().len(), 1, "{:?}", notes.borrow());
    app.shutdown();
    std::fs::remove_file(&path).unwrap();
}

/// A studio of synthetic sources named `ids` without subjects, so its
/// shots are their wides ("a/wide", ...), small, without monitors and
/// with the director off.
fn wides(ids: &[&str]) -> StudioConfig {
    let source = SourceConfig::Synthetic {
        width: 160,
        height: 90,
        fps: 30.0,
        subjects: 0,
    };
    let mut config = StudioConfig::with_source(ids[0], source);
    for id in &ids[1..] {
        let mut entry = config.sources[0].clone();
        entry.id = (*id).into();
        config.sources.push(entry);
    }
    config.output.width = 160;
    config.output.height = 90;
    config.director.mode = ac_director::DirectorMode::Off;
    config.monitors = MonitorSettings::disabled();
    config
}

/// An app on an engine started from `config` in real time (see
/// [`start`]), once every wide is available.
fn start_app(config: &StudioConfig) -> OperatorApp {
    let engine = Engine::start_with(config.clone(), EngineOptions::default()).unwrap();
    engine
        .handle()
        .wait_until(Duration::from_secs(10), |s| {
            s.shots.len() == config.sources.len() && s.shots.iter().all(|w| w.available)
        })
        .expect("the sources deliver frames");
    OperatorApp::new(engine, config)
}

/// Draws one frame with `keys` pressed.
fn frame_with(app: &mut OperatorApp, ctx: &egui::Context, keys: &[egui::Key]) {
    let events = keys
        .iter()
        .map(|&key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        })
        .collect();
    let input = egui::RawInput {
        events,
        ..egui::RawInput::default()
    };
    let _ = ctx.run_ui(input, |ui| app.frame(ui));
}

#[test]
fn a_frame_drawn_before_the_engine_applies_a_key_still_cuts_the_staged_shot() {
    let config = wides(&["a", "b"]);
    let mut app = start_app(&config);
    let (a, b) = (ShotId("a/wide".into()), ShotId("b/wide".into()));
    app.handle
        .send(EngineCommand::SetPreview { shot: a.clone() })
        .unwrap();
    app.handle
        .send(EngineCommand::Cut {
            preview: Some(a.clone()),
        })
        .unwrap();
    let ctx = egui::Context::default();
    frame_with(&mut app, &ctx, &[]);
    // Key 2 stages "b", and the next frame is drawn at once, almost
    // always from a snapshot older than the staging: the engine applies
    // commands only on its next tick.
    frame_with(&mut app, &ctx, &[egui::Key::Num2]);
    frame_with(&mut app, &ctx, &[]);
    app.handle
        .wait_until(Duration::from_secs(10), |s| {
            s.switcher.preview.as_ref() == Some(&b)
        })
        .expect("b staged");
    // Space, pressed looking at that frame, takes "b".
    frame_with(&mut app, &ctx, &[egui::Key::Space]);
    settle(&mut app, &ctx);
    let snap = app
        .handle
        .wait_until(Duration::from_secs(10), |s| {
            s.switcher.program.as_ref() == Some(&b)
        })
        .expect("b cut to program");
    assert_eq!(snap.switcher.preview, Some(a));
    app.shutdown();
}

#[test]
fn a_staging_no_snapshot_shows_is_seen_until_the_engine_has_answered() {
    // With the director setting, a director in Auto mode stages its own
    // shot over the operator's on the very tick that applies it, so no
    // snapshot ever shows the operator's shot.
    let mut config = wides(&["a", "b", "c"]);
    config.director.mode = ac_director::DirectorMode::Auto;
    config.director.takeover = ac_director::Takeover::Director;
    config.switcher.transition = ac_switcher::TransitionKind::Cut;
    let mut app = start_app(&config);
    let ctx = egui::Context::default();
    let snap = app
        .handle
        .wait_until(Duration::from_secs(10), |s| {
            s.switcher.program.is_some() && s.switcher.preview.is_some()
        })
        .expect("the director cuts a wide to program and stages another");
    let theirs = snap.switcher.preview.clone();
    let takes = snap.director.takes;
    let mine = snap
        .shots
        .iter()
        .position(|s| {
            Some(&s.id) != snap.switcher.program.as_ref() && Some(&s.id) != theirs.as_ref()
        })
        .expect("a third wide");
    let digits = [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3];
    frame_with(&mut app, &ctx, &[]);
    frame_with(&mut app, &ctx, &[digits[mine]]);
    settle(&mut app, &ctx);
    // Once the engine has answered and a later snapshot is drawn, the
    // window takes what preview shows, the director's shot, as seen.
    let answered = app.handle.snapshot().tick;
    app.handle
        .wait_until(Duration::from_secs(10), |s| s.tick > answered + 1)
        .expect("the engine ticks");
    frame_with(&mut app, &ctx, &[]);
    frame_with(&mut app, &ctx, &[egui::Key::Space]);
    settle(&mut app, &ctx);
    let snap = app
        .handle
        .wait_until(Duration::from_secs(10), |s| s.switcher.program == theirs)
        .expect("the cut went through");
    assert_eq!(snap.director.takes, takes, "the cut was the operator's");
    app.shutdown();
}

/// A file removed when dropped, so a failing test leaves no recording
/// behind.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn a_close_request_while_recording_asks_and_the_engine_keeps_running() {
    use crate::ui::close_guard::Question;
    use egui_kittest::{Harness, kittest::Queryable};
    if !(have("ffmpeg") && have("ffprobe")) {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let mut config = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 160,
            height: 90,
            fps: 30.0,
            subjects: 1,
        },
    );
    config.output.width = 160;
    config.output.height = 90;
    config.output.encoder = ac_engine::EncoderSetting::Libx264;
    config.monitors = MonitorSettings::disabled();
    let engine = Engine::start_with(config.clone(), EngineOptions::default()).unwrap();
    let handle = engine.handle();
    handle
        .wait_until(Duration::from_secs(10), |s| {
            s.shots.first().is_some_and(|w| w.available)
        })
        .expect("the source delivers frames");
    let path = temp_path("close-guard.mp4");
    // Dropped after the app, which finishes the recording first.
    let _scratch = Scratch(path.clone());
    handle
        .send(EngineCommand::StartOutput {
            rtmp_url: None,
            record_path: Some(path.clone()),
        })
        .unwrap();
    handle
        .wait_until(Duration::from_secs(20), |s| {
            s.output.state == OutputState::Running
        })
        .expect("the recording runs");
    let quit = QuitSignal::new();
    let app = OperatorApp::new(engine, &config).with_quit(quit.clone());
    // The app asks for a repaint every 30 ms, so the harness never
    // settles: it runs a counted number of frames instead.
    let mut h = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .build_ui_state(|ui, app: &mut OperatorApp| app.frame(ui), app);
    // The window's close button.
    let request_close = |h: &mut Harness<'_, OperatorApp>| {
        h.input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
    };
    let commands = |h: &Harness<'_, OperatorApp>| {
        h.output().viewport_output[&egui::ViewportId::ROOT]
            .commands
            .clone()
    };
    request_close(&mut h);
    h.step();
    assert!(commands(&h).contains(&egui::ViewportCommand::CancelClose));
    h.run_steps(2);
    h.get_by_label(Question::Close.title());
    // The show goes on: the engine ticks and records, the dialog waits.
    let asked = handle.snapshot();
    let later = handle
        .wait_until(Duration::from_secs(10), |s| {
            s.tick > asked.tick + 15 && s.output.frames_pushed > asked.output.frames_pushed
        })
        .expect("the engine keeps ticking and recording");
    h.run_steps(2);
    assert!(h.state().engine.is_some());
    assert!(later.running);
    assert_eq!(later.output.state, OutputState::Running);
    h.get_by_label(Question::Close.title());
    // A termination signal still closes, unasked: the app sends Close,
    // and the close request it comes back as goes through.
    quit.signal();
    h.step();
    assert!(commands(&h).contains(&egui::ViewportCommand::Close));
    assert!(h.query_by_label(Question::Close.title()).is_none());
    request_close(&mut h);
    h.step();
    assert!(!commands(&h).contains(&egui::ViewportCommand::CancelClose));
    // Closing shuts the engine down, which finishes the recording.
    h.state_mut().shutdown();
    assert!(h.state().engine.is_none());
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    assert!(size > 0, "the recording was finished");
}
