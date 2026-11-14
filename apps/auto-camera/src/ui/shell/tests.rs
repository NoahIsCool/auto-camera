//! Tests of the window's shell: the picker before a studio, and a change
//! of sources while one runs.

use super::*;
use crate::setup::Camera;
use crate::testing::temp_path;
use crate::ui::picker::{CANCEL, OPEN};
use ac_engine::MonitorSettings;
use ac_sources::SourceConfig;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

/// No cameras.
fn none() -> Result<Vec<Camera>, String> {
    Ok(Vec::new())
}

/// A shell harness on `shell` at the window's size.
fn harness(shell: Shell) -> Harness<'static, Shell> {
    Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .build_ui_state(|ui, shell: &mut Shell| shell.frame(ui), shell)
}

/// A folder of its own for one test's studio file, removed when dropped.
struct Folder(PathBuf);

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn without_a_studio_the_picker_writes_one_and_opens_it() {
    let dir = Folder(temp_path("shell-first-run"));
    std::fs::create_dir_all(&dir.0).unwrap();
    let studio = dir.0.join("show.toml");
    let setup = Setup {
        studio: studio.clone(),
        save_show: None,
        options: None,
        lister: none,
    };
    let shell = Shell::new(Start::Picker, setup, QuitSignal::new()).unwrap();
    assert!(shell.picking() && !shell.running());
    assert_eq!(shell.title(), "Auto Camera · Sources");
    let mut h = harness(shell);
    h.run_steps(2);
    assert!(h.query_by_label(CANCEL).is_none());
    // A file that is not a video: the source starts offline and is
    // retried, the engine runs.
    let video = dir.0.join("clip.mp4");
    std::fs::write(&video, b"x").unwrap();
    h.state_mut().picker.as_mut().unwrap().path = video.display().to_string();
    h.run_steps(2);
    h.get_by_label(OPEN).click();
    h.run_steps(2);
    assert!(h.state().running(), "{:?}", h.state());
    assert!(!h.state().picking());
    assert_eq!(h.state().title(), "Auto Camera · clip");
    assert!(
        std::fs::read_to_string(&studio)
            .unwrap()
            .contains("clip.mp4")
    );
    h.state_mut().shutdown();
}

#[test]
fn sources_opens_the_picker_over_a_running_studio_and_cancel_keeps_it() {
    let dir = Folder(temp_path("shell-sources"));
    std::fs::create_dir_all(&dir.0).unwrap();
    let mut config = StudioConfig::with_source(
        "stage",
        SourceConfig::Synthetic {
            width: 160,
            height: 90,
            fps: 30.0,
            subjects: 0,
        },
    );
    config.output.width = 160;
    config.output.height = 90;
    config.monitors = MonitorSettings::disabled();
    let setup = Setup {
        studio: dir.0.join("show.toml"),
        save_show: None,
        options: None,
        lister: none,
    };
    let start = Start::Studio(Box::new(Opened {
        config,
        warnings: Vec::new(),
        show: Err("no target".into()),
    }));
    let shell = Shell::new(start, setup, QuitSignal::new()).unwrap();
    assert!(shell.running() && !shell.picking());
    let mut h = harness(shell);
    h.run_steps(2);
    h.get_by_label(header::SOURCES).click();
    h.run_steps(2);
    assert!(h.state().picking());
    h.get_by_label(CANCEL).click();
    h.run_steps(2);
    assert!(!h.state().picking());
    assert!(h.state().running());
    // Nothing was written.
    assert!(!dir.0.join("show.toml").exists());
    h.state_mut().shutdown();
}
