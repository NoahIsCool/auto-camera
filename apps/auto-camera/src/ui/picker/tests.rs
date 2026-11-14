//! Tests of the source picker's form, driven through its AccessKit tree.

use super::*;
use crate::testing::temp_path;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

/// Two connected cameras.
fn two() -> Result<Vec<Camera>, String> {
    Ok(vec![
        Camera {
            name: "FaceTime HD Camera".into(),
            unique_id: "0xAA".into(),
        },
        Camera {
            name: "Cam Link 4K".into(),
            unique_id: "0xBB".into(),
        },
    ])
}

/// A machine that cannot list cameras.
fn broken() -> Result<Vec<Camera>, String> {
    Err("cannot list cameras: no system_profiler".into())
}

/// The form and the last answer it gave.
struct Form {
    /// The form.
    picker: Picker,
    /// Its last answer.
    action: Option<Action>,
}

/// A harness on a picker for `now`, listing with `lister`, run until the
/// listing has answered.
fn harness(now: Current, cancellable: bool, lister: Lister) -> Harness<'static, Form> {
    let picker = Picker::new(
        Path::new("config/show.toml"),
        Path::new("/studio"),
        now,
        cancellable,
        lister,
    );
    let mut h = Harness::builder()
        .with_size(egui::vec2(700.0, 600.0))
        .build_ui_state(
            |ui, form: &mut Form| {
                if let Some(a) = form.picker.show(ui) {
                    form.action = Some(a);
                }
            },
            Form {
                picker,
                action: None,
            },
        );
    for _ in 0..200 {
        h.step();
        if !h.state().picker.listing() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
    h
}

#[test]
fn a_video_file_is_opened_once_it_exists() {
    let mut h = harness(Current::default(), false, two);
    assert_eq!(h.state().picker.tab, Tab::File);
    assert!(h.query_by_label(CANCEL).is_none(), "nothing to go back to");
    assert!(h.get_by_label(OPEN).accesskit_node().is_disabled());
    let video = temp_path("picker-clip.mp4");
    h.state_mut().picker.path = video.display().to_string();
    h.run_steps(2);
    assert!(
        h.get_by_label(OPEN).accesskit_node().is_disabled(),
        "no file yet"
    );
    std::fs::write(&video, b"not really a video").unwrap();
    h.run_steps(2);
    h.get_by_label(OPEN).click();
    h.run_steps(2);
    assert_eq!(
        h.state().action,
        Some(Action::Apply(Choice::File(video.clone())))
    );
    std::fs::remove_file(&video).unwrap();
}

#[test]
fn a_dropped_file_fills_the_path() {
    let mut h = harness(
        Current {
            cameras: two().unwrap(),
            ..Current::default()
        },
        true,
        two,
    );
    assert_eq!(h.state().picker.tab, Tab::Cameras);
    h.input_mut().dropped_files.push(egui::DroppedFile {
        path: Some("/Movies/drop.mov".into()),
        ..egui::DroppedFile::default()
    });
    h.run_steps(1);
    assert_eq!(h.state().picker.tab, Tab::File);
    assert_eq!(h.state().picker.path, "/Movies/drop.mov");
}

#[test]
fn ticked_cameras_are_opened_in_list_order_with_the_mode() {
    let mut h = harness(Current::default(), true, two);
    h.get_by_label("Cameras").click();
    h.run_steps(2);
    assert!(
        h.get_by_label(OPEN).accesskit_node().is_disabled(),
        "none ticked"
    );
    h.get_by_label("Cam Link 4K").click();
    h.run_steps(1);
    h.get_by_label("FaceTime HD Camera").click();
    h.run_steps(2);
    h.get_by_label(OPEN).click();
    h.run_steps(1);
    let Some(Action::Apply(Choice::Cameras { cameras, mode })) = h.state().action.clone() else {
        panic!("{:?}", h.state().action);
    };
    let ids: Vec<&str> = cameras.iter().map(|c| c.unique_id.as_str()).collect();
    assert_eq!(ids, ["0xAA", "0xBB"]);
    assert_eq!(mode, MODES[0]);
    h.get_by_label(CANCEL).click();
    h.run_steps(1);
    assert_eq!(h.state().action, Some(Action::Cancel));
}

#[test]
fn the_studios_cameras_stay_ticked_even_when_not_connected() {
    let now = Current {
        file: Some("../media/a.mp4".into()),
        cameras: vec![
            Camera {
                name: "Stage".into(),
                unique_id: "0xbb".into(),
            },
            Camera {
                name: "Unplugged".into(),
                unique_id: "0xCC".into(),
            },
        ],
        mode: Some(MODES[1]),
    };
    let h = harness(now, true, two);
    // The relative file is shown resolved against the studio's folder.
    assert_eq!(
        PathBuf::from(&h.state().picker.path),
        PathBuf::from("/studio/../media/a.mp4")
    );
    h.get_by_label("0xCC · not connected");
    let Ok(Choice::Cameras { cameras, mode }) = h.state().picker.choice() else {
        panic!("{:?}", h.state().picker.choice());
    };
    // The connected one under macOS's name, then the unplugged one.
    let names: Vec<&str> = cameras.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Cam Link 4K", "Unplugged"]);
    assert_eq!(mode, MODES[1]);
}

#[test]
fn a_listing_that_fails_says_why_and_errors_show() {
    let mut h = harness(Current::default(), true, broken);
    h.get_by_label("Cameras").click();
    h.run_steps(2);
    h.get_by_label("cannot list cameras: no system_profiler");
    assert_eq!(
        h.state().picker.choice(),
        Err("Tick at least one camera.".into())
    );
    h.state_mut()
        .picker
        .fail("the studio did not start".into(), false);
    h.run_steps(2);
    h.get_by_label("the studio did not start");
    assert!(
        h.query_by_label(CANCEL).is_none(),
        "the studio behind it stopped"
    );
}
