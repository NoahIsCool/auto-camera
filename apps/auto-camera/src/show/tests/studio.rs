//! Tests of what a saved show leaves to its studio file (the stream key
//! always; the sources and recording path unless the show gives its own)
//! and of the default show names, which keep studio files named alike in
//! different folders apart.

use super::*;

/// A second small synthetic source, `desk`, to put before [`STUDIO`]'s.
const DESK: &str = "\
[[sources]]
id = \"desk\"
kind = \"synthetic\"
width = 160
height = 90
fps = 30

";

/// [`STUDIO`] with `more` at the top of its `[output]` table.
fn with_output(more: &str) -> String {
    STUDIO.replace("[output]\n", &format!("[output]\n{more}"))
}

#[test]
fn studio_files_named_alike_get_shows_of_their_own() {
    let dir = temp_path("same-name");
    let shows = dir.join("shows");
    let mut targets = Vec::new();
    for (folder, mode) in [("church", "suggest"), ("school", "auto")] {
        let studio = dir.join(folder).join("studio.toml");
        std::fs::create_dir_all(studio.parent().unwrap()).unwrap();
        let text = STUDIO.replace("mode = \"off\"", &format!("mode = \"{mode}\""));
        std::fs::write(&studio, text).unwrap();
        let first = StudioConfig::load_with_warnings(&studio).unwrap();
        let t = ShowTarget::for_run(&first, &studio, None, Ok(shows.clone())).unwrap();
        save(exported(&studio, &[]), &t, &mut SystemDisk).unwrap();
        targets.push(t);
    }
    let [church, school] = targets.as_slice() else {
        unreachable!()
    };
    assert_ne!(church.path, school.path);
    let name = |t: &ShowTarget| t.path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name(church).starts_with("studio-") && name(church).ends_with(".show.toml"));
    for (t, mode) in [
        (church, DirectorMode::Suggest),
        (school, DirectorMode::Auto),
    ] {
        let reopened = StudioConfig::load_with_warnings(&t.path).unwrap();
        assert_eq!(reopened.studio, t.studio, "each show over its own studio");
        assert_eq!(reopened.config.director.mode, mode);
    }
    // The same studio file under another spelling has the same show.
    let other = dir.join("church/../church/studio.toml");
    let again = ShowTarget::for_run(
        &loaded(other.to_str().unwrap(), false),
        &other,
        None,
        Ok(shows.clone()),
    )
    .unwrap();
    assert_eq!(again.path, church.path);
    // A show of another studio file is never replaced, whatever names it.
    let before = std::fs::read(&church.path).unwrap();
    let over = ShowTarget {
        path: church.path.clone(),
        ..school.clone()
    };
    let err = save(exported(&school.studio, &[]), &over, &mut SystemDisk).unwrap_err();
    assert_eq!(
        err,
        format!(
            "{} is the show of another studio file, {}; it is left as it is",
            church.path.display(),
            church.studio.display()
        )
    );
    assert_eq!(std::fs::read(&church.path).unwrap(), before);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_saved_show_leaves_the_stream_key_sources_and_recording_path_to_the_studio() {
    let (dir, target) = folder("studio-parts");
    let old = "rtmp_url = \"rtmp://live.example.com/app/OLDKEY\"\nrecord_path = \"old.mp4\"\n";
    std::fs::write(&target.studio, with_output(old)).unwrap();
    // The run starts, and its target keeps the studio file as read now.
    let first = StudioConfig::load_with_warnings(&target.studio).unwrap();
    let target = ShowTarget::for_run(&first, &target.studio, Some(&target.path), no_dir()).unwrap();
    let show = exported_from(
        &first.config,
        &[EngineCommand::SetDirectorMode {
            mode: DirectorMode::Auto,
        }],
    );
    // While it runs, a camera is added to the studio file: the save does
    // not pin the sources the run started with.
    std::fs::write(&target.studio, format!("{DESK}{}", with_output(old))).unwrap();
    save(show, &target, &mut SystemDisk).unwrap();
    let text = std::fs::read_to_string(&target.path).unwrap();
    for absent in ["OLDKEY", "rtmp", "[[sources]]", "[output]", "record_path"] {
        assert!(!text.contains(absent), "{absent} in {text}");
    }
    assert!(text.contains("mode = \"auto\""), "{text}");
    // Later the key is rotated and the recording path changed.
    let new = "rtmp_url = \"rtmp://live.example.com/app/NEWKEY\"\nrecord_path = \"new.mp4\"\n";
    std::fs::write(&target.studio, format!("{DESK}{}", with_output(new))).unwrap();
    let reopened = StudioConfig::load(&target.path).unwrap();
    assert_eq!(
        reopened.output.rtmp_url.as_deref(),
        Some("rtmp://live.example.com/app/NEWKEY")
    );
    assert_eq!(reopened.output.record_path, Some(PathBuf::from("new.mp4")));
    let ids: Vec<&str> = reopened.sources.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["desk", "stage"], "the camera added is there");
    assert_eq!(reopened.director.mode, DirectorMode::Auto, "the show's");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_show_with_its_own_sources_and_recording_path_keeps_them_through_a_save() {
    let (dir, target) = folder("own-parts");
    std::fs::create_dir_all(dir.join("shows")).unwrap();
    let hand = format!(
        "show = 1\nstudio = \"../studio.toml\"\n\n{DESK}[output]\nrecord_path = \"mine.mp4\"\n"
    );
    std::fs::write(&target.path, hand).unwrap();
    let opened = StudioConfig::load_with_warnings(&target.path).unwrap();
    let target = ShowTarget::for_run(&opened, &target.path, None, no_dir()).unwrap();
    assert_eq!(target.base.sources.len(), 1, "the studio file's own");
    save(exported_from(&opened.config, &[]), &target, &mut SystemDisk).unwrap();
    let text = std::fs::read_to_string(&target.path).unwrap();
    assert!(text.contains("id = \"desk\""), "{text}");
    assert!(text.contains("record_path = \"mine.mp4\""), "{text}");
    let reopened = StudioConfig::load(&target.path).unwrap();
    let ids: Vec<&str> = reopened.sources.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["desk"]);
    assert_eq!(reopened.output.record_path, Some(PathBuf::from("mine.mp4")));
    std::fs::remove_dir_all(&dir).unwrap();
}
