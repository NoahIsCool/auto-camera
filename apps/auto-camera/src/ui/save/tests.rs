//! Tests of Save show: the console's button sends the export, the saver
//! writes the shows in the order they were pressed and says where, and a
//! live window saves a query added to its engine. Files go to fresh temp
//! folders.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ac_director::DirectorMode;
use ac_engine::{Engine, EngineCore, EngineOptions, EngineSnapshot, StudioConfig};
use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::testing::temp_path;
use crate::ui::app::OperatorApp;
use crate::ui::console::Console;
use crate::ui::fixtures;

/// A studio file's text: one small synthetic source, the synthetic
/// detector and the monitors off.
const STUDIO: &str = "schema = 1\n\n[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\n\
                      width = 160\nheight = 90\nfps = 30\nsubjects = 1\n\n[detector]\n\
                      kind = \"synthetic\"\n\n[output]\nwidth = 160\nheight = 90\n\n\
                      [monitors]\nrate_hz = 0\nthumb_rate_hz = 0\n";

/// A fresh temp folder with `studio.toml` and the target beside it.
fn folder(name: &str) -> (PathBuf, ShowTarget) {
    let dir = temp_path(name);
    std::fs::create_dir_all(&dir).unwrap();
    let studio = dir.join("studio.toml");
    std::fs::write(&studio, STUDIO).unwrap();
    let target = ShowTarget {
        path: dir.join("sunday.show.toml"),
        base: StudioConfig::read_file(&studio).unwrap().base,
        studio,
    };
    (dir, target)
}

/// The show of the studio at `studio` with the director in `mode`.
fn show(studio: &Path, mode: DirectorMode) -> Show {
    let mut config = StudioConfig::load(studio).unwrap();
    config.director.mode = mode;
    let infos = vec![config.sources[0].placeholder_info((160, 90))];
    EngineCore::new(&config, infos, &[true])
        .unwrap()
        .export_show()
}

/// True while `saver` has a save running or waiting.
fn busy(saver: &ShowSaver) -> bool {
    saver.running.is_some() || saver.next.is_some()
}

/// Polls `saver` until it is idle, for at most 10 s; the notices raised.
fn settle(saver: &mut ShowSaver) -> Vec<(String, bool)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut out = Vec::new();
    while busy(saver) {
        assert!(Instant::now() < deadline, "the save did not end");
        out.extend(saver.poll());
        std::thread::sleep(Duration::from_millis(2));
    }
    out
}

/// The console of [`fixtures::config`] drawing `snap`, and what it sent.
struct Window {
    /// The console.
    console: Console,
    /// The snapshot drawn.
    snap: EngineSnapshot,
    /// Every command returned.
    sent: Vec<EngineCommand>,
}

#[test]
fn the_console_sends_the_export_when_save_show_is_pressed() {
    let snap = fixtures::snapshot();
    let state = Window {
        console: Console::new(&fixtures::config(), &snap),
        snap,
        sent: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .build_ui_state(
            |ui, w: &mut Window| {
                let sent = w.console.frame(ui, &w.snap, Vec::new(), false);
                w.sent.extend(sent);
            },
            state,
        );
    h.run();
    assert!(h.state().sent.is_empty(), "nothing is saved unasked");
    h.get_by_label(SAVE_SHOW).click();
    h.run();
    assert_eq!(h.state().sent, [EngineCommand::ExportShow]);
}

#[test]
fn saves_follow_the_presses_and_say_where_they_went() {
    let (dir, target) = folder("saver");
    let mut saver = ShowSaver::new(Ok(target.clone()));
    assert_eq!(saver.save(show(&target.studio, DirectorMode::Off)), None);
    // Pressed again while the first runs: queued, and saved after it.
    assert_eq!(
        saver.save(show(&target.studio, DirectorMode::Suggest)),
        None
    );
    assert_eq!(saver.save(show(&target.studio, DirectorMode::Auto)), None);
    let notices = settle(&mut saver);
    let saved = format!("show saved to {}", target.path.display());
    assert!(!notices.is_empty() && notices.len() <= 2, "{notices:?}");
    assert!(
        notices.iter().all(|n| *n == (saved.clone(), false)),
        "{notices:?}"
    );
    let reopened = StudioConfig::load(&target.path).unwrap();
    assert_eq!(reopened.director.mode, DirectorMode::Auto, "the last press");
    // A save that fails says why.
    std::fs::write(&target.path, "not a show\n").unwrap();
    assert_eq!(saver.save(show(&target.studio, DirectorMode::Off)), None);
    let notices = settle(&mut saver);
    let [(text, true)] = notices.as_slice() else {
        panic!("{notices:?}");
    };
    assert!(text.starts_with("cannot save the show: "), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_window_without_a_target_says_why_it_cannot_save() {
    let empty = || Show::parse_with_warnings("show = 1\n", false).unwrap().0;
    let mut saver = ShowSaver::default();
    let (text, error) = saver.save(empty()).unwrap_or_else(|| panic!("a notice"));
    assert!(error && text.contains(NO_TARGET), "{text}");
    assert!(!busy(&saver));
    let mut saver = ShowSaver::new(Err("no home folder".into()));
    let notice = saver.save(empty());
    assert_eq!(
        notice,
        Some(("cannot save the show: no home folder".into(), true))
    );
}

#[test]
fn closing_the_window_finishes_the_saves_it_started() {
    let (dir, target) = folder("close");
    let mut saver = ShowSaver::new(Ok(target.clone()));
    saver.save(show(&target.studio, DirectorMode::Off));
    saver.save(show(&target.studio, DirectorMode::Auto));
    drop(saver);
    let reopened = StudioConfig::load(&target.path).unwrap();
    assert_eq!(reopened.director.mode, DirectorMode::Auto);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_live_window_saves_a_query_added_to_its_engine() {
    let (dir, target) = folder("live");
    let config = StudioConfig::load(&target.studio).unwrap();
    let engine = Engine::start_with(config.clone(), EngineOptions::default()).unwrap();
    let handle = engine.handle();
    handle
        .send(EngineCommand::AddQuery {
            name: "Pastor".into(),
            prompt: "speaker upper body".into(),
            presets: None,
            literal: false,
        })
        .unwrap();
    let app = OperatorApp::new(engine, &config).with_show_target(Ok(target.clone()));
    // The app asks for a repaint every 30 ms, so the harness never
    // settles: it runs a counted number of frames instead.
    let mut h = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .build_ui_state(|ui, app: &mut OperatorApp| app.frame(ui), app);
    h.run_steps(2);
    h.get_by_label(SAVE_SHOW).click();
    let deadline = Instant::now() + Duration::from_secs(10);
    let saved = format!("show saved to {}", target.path.display());
    while h.query_by_label_contains(&saved).is_none() {
        assert!(
            Instant::now() < deadline,
            "no notice that the show was saved"
        );
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    let reopened = StudioConfig::load(&target.path).unwrap();
    let names: Vec<&str> = reopened.queries.iter().map(|q| q.name.as_str()).collect();
    assert_eq!(names, ["Pastor"]);
    drop(h);
    std::fs::remove_dir_all(&dir).unwrap();
}
