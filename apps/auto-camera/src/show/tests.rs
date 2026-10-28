//! Tests of saving the show: where it goes, the atomic write (with a disk
//! that fails between the temp file and the rename), and a query added to
//! a running engine that is back after a save and a restart from the show
//! file; [`studio`] has what a show leaves to its studio file. Every file
//! is written in a fresh temp folder.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ac_core::QueryId;
use ac_director::DirectorMode;
use ac_engine::{CommandReply, Engine, EngineCommand, EngineHandle, EngineOptions};
use ac_switcher::{Transition, TransitionKind};

use super::*;
use crate::testing::temp_path;

mod studio;

/// A small synthetic studio, without a `schema` key, with the synthetic
/// detector, one query and the monitors off.
const STUDIO: &str = "\
[[sources]]
id = \"stage\"
kind = \"synthetic\"
width = 160
height = 90
fps = 30
subjects = 2

[[queries]]
name = \"Person\"
prompt = \"person\"

[detector]
kind = \"synthetic\"

[director]
mode = \"off\"

[output]
width = 160
height = 90
encoder = \"libx264\"

[monitors]
rate_hz = 0
thumb_rate_hz = 0
";

/// A fresh temp folder holding `studio.toml` ([`STUDIO`]), and the
/// target `shows/studio.show.toml` over it.
fn folder(name: &str) -> (PathBuf, ShowTarget) {
    let dir = temp_path(name);
    std::fs::create_dir_all(&dir).unwrap();
    let studio = dir.join("studio.toml");
    std::fs::write(&studio, STUDIO).unwrap();
    let target = ShowTarget {
        path: dir.join("shows/studio.show.toml"),
        base: StudioConfig::read_file(&studio).unwrap().base,
        studio,
    };
    (dir, target)
}

/// A shows folder for a target that does not need one.
fn no_dir() -> Result<PathBuf, String> {
    Err("this test has no data dir".into())
}

/// The show a core started from `config` exports, with `changes` applied.
fn exported_from(config: &StudioConfig, changes: &[EngineCommand]) -> Show {
    let infos = config
        .sources
        .iter()
        .map(|s| s.placeholder_info((160, 90)))
        .collect();
    let mut core = ac_engine::EngineCore::new(config, infos, &[true]).unwrap();
    for c in changes {
        core.apply(c).unwrap();
    }
    core.export_show()
}

/// The show a core started from the file `studio` exports, with `changes`
/// applied.
fn exported(studio: &Path, changes: &[EngineCommand]) -> Show {
    exported_from(&StudioConfig::load(studio).unwrap(), changes)
}

/// Exports the show from the engine behind `handle` and saves it to
/// `target`, waiting for the save's thread, as the window does in two
/// steps.
fn save_now(handle: &EngineHandle, target: &ShowTarget) -> Result<PathBuf, String> {
    let show = match handle.send(EngineCommand::ExportShow) {
        Ok(CommandReply::Show(show)) => *show,
        other => return Err(format!("the engine answered {other:?}")),
    };
    SaveJob::start(show, target.clone())?.wait()
}

/// The names of the files in `dir`.
fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// A loaded config of `studio`, read as a show when `show`.
fn loaded(studio: &str, show: bool) -> LoadedConfig {
    let config = StudioConfig::parse(STUDIO, false).unwrap();
    LoadedConfig {
        base: config.clone(),
        config,
        warnings: Vec::new(),
        studio: studio.into(),
        show,
    }
}

#[test]
fn the_data_dir_follows_each_systems_convention() {
    let env = |vars: &[(&str, &str)]| {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |name: &str| vars.get(name).cloned()
    };
    let home = [("HOME", "/Users/ann")];
    assert_eq!(
        data_dir(Os::Mac, env(&home)),
        Ok("/Users/ann/Library/Application Support/auto-camera".into())
    );
    assert_eq!(
        data_dir(Os::Other, env(&home)),
        Ok("/Users/ann/.local/share/auto-camera".into())
    );
    let xdg = [("HOME", "/home/ann"), ("XDG_DATA_HOME", "/data")];
    assert_eq!(
        data_dir(Os::Other, env(&xdg)),
        Ok("/data/auto-camera".into())
    );
    assert_eq!(
        data_dir(Os::Mac, env(&xdg)),
        Ok("/home/ann/Library/Application Support/auto-camera".into()),
        "XDG is not the Mac's convention"
    );
    // A relative XDG_DATA_HOME is ignored, as the convention asks.
    let relative = [("HOME", "/home/ann"), ("XDG_DATA_HOME", "data")];
    assert_eq!(
        data_dir(Os::Other, env(&relative)),
        Ok("/home/ann/.local/share/auto-camera".into())
    );
    // The override wins everywhere.
    let ours = [("HOME", "/home/ann"), (DATA_DIR_ENV, "/srv/ac")];
    for os in [Os::Mac, Os::Other] {
        assert_eq!(data_dir(os, env(&ours)), Ok("/srv/ac".into()));
    }
    // A relative override is an error, not a quiet fall back to HOME.
    let relative = [("HOME", "/home/ann"), (DATA_DIR_ENV, "showdata")];
    for os in [Os::Mac, Os::Other] {
        assert_eq!(
            data_dir(os, env(&relative)),
            Err("AC_DATA_DIR is `showdata`, which is not an absolute path".into())
        );
    }
    // One set empty is not set.
    let empty = [("HOME", "/home/ann"), (DATA_DIR_ENV, "")];
    assert_eq!(
        data_dir(Os::Other, env(&empty)),
        Ok("/home/ann/.local/share/auto-camera".into())
    );
    let none = Err("there is no home folder".to_owned());
    assert_eq!(data_dir(Os::Other, env(&[])), none);
    assert_eq!(data_dir(Os::Mac, env(&[("HOME", "")])), none);
}

#[test]
fn the_target_is_the_flag_then_the_show_opened_then_the_data_dir() {
    let shows = Ok(PathBuf::from("/data/shows"));
    let studio = loaded("/studios/sunday.toml", false);
    let t = ShowTarget::for_run(
        &studio,
        Path::new("/studios/sunday.toml"),
        None,
        shows.clone(),
    )
    .unwrap();
    // The FNV-1a hash of the path, fixed, so later builds save there too.
    assert_eq!(
        t.path,
        PathBuf::from("/data/shows/sunday-4b3716bc.show.toml")
    );
    assert_eq!(t.studio, PathBuf::from("/studios/sunday.toml"));
    assert_eq!(t.base, studio.base);
    // A show opened is saved back to, and still lies over its studio.
    let show = loaded("/studios/sunday.toml", true);
    let t =
        ShowTarget::for_run(&show, Path::new("/mine/easter.toml"), None, shows.clone()).unwrap();
    assert_eq!(t.path, PathBuf::from("/mine/easter.toml"));
    assert_eq!(t.studio, PathBuf::from("/studios/sunday.toml"));
    // --save-show wins, and relative paths are made absolute.
    let t =
        ShowTarget::for_run(&show, Path::new("x"), Some(Path::new("s.toml")), no_dir()).unwrap();
    assert!(t.path.is_absolute() && t.path.ends_with("s.toml"), "{t:?}");
    let t = ShowTarget::for_run(
        &loaded("rel/studio.toml", false),
        Path::new("x"),
        None,
        shows,
    )
    .unwrap();
    assert!(t.studio.is_absolute() && t.studio.ends_with("rel/studio.toml"));
    // Without a data dir there is no default target, and it says why.
    let err = ShowTarget::for_run(&studio, Path::new("x"), None, no_dir()).unwrap_err();
    assert!(err.starts_with("no folder for shows: this test has no data dir"));
    assert!(
        err.contains(DATA_DIR_ENV) && err.contains("--save-show"),
        "{err}"
    );
}

#[test]
fn a_show_is_never_saved_over_its_studio_file() {
    let (dir, target) = folder("over-studio");
    let studio = loaded(target.studio.to_str().unwrap(), false);
    let err =
        ShowTarget::for_run(&studio, &target.studio, Some(&target.studio), no_dir()).unwrap_err();
    assert!(err.contains("is the studio file"), "{err}");
    // The same file under another spelling is the studio file too.
    let other = dir.join("shows/../studio.toml");
    std::fs::create_dir_all(dir.join("shows")).unwrap();
    assert!(ShowTarget::for_run(&studio, &target.studio, Some(&other), no_dir()).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_save_writes_a_show_that_opens_again_for_its_owner_only() {
    let (dir, target) = folder("save");
    let show = exported(
        &target.studio,
        &[EngineCommand::SetDirectorMode {
            mode: DirectorMode::Auto,
        }],
    );
    let path = SaveJob::start(show, target.clone())
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(path, target.path);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# An Auto Camera show"), "{text}");
    assert!(
        text.contains(&format!(
            "studio = {:?}",
            target.studio.display().to_string()
        )),
        "{text}"
    );
    assert_eq!(
        names(&dir.join("shows")),
        ["studio.show.toml"],
        "no temp file left"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600, "the operator's own");
        assert_eq!(mode(&dir.join("shows")), 0o700);
    }
    let reopened = StudioConfig::load_with_warnings(&path).unwrap();
    assert!(reopened.show);
    assert_eq!(reopened.config.director.mode, DirectorMode::Auto);
    // A JSON target is written as JSON, and opens too.
    let json = ShowTarget {
        path: dir.join("shows/studio.json"),
        ..target.clone()
    };
    let show = exported(&target.studio, &[]);
    SaveJob::start(show, json.clone()).unwrap().wait().unwrap();
    assert!(
        std::fs::read_to_string(&json.path)
            .unwrap()
            .starts_with('{')
    );
    assert_eq!(
        StudioConfig::load(&json.path).unwrap().director.mode,
        DirectorMode::Off
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The system's disk, with one step made to fail as a crash or a full
/// disk would, and the names of the threads each step ran on.
struct Faulty {
    /// The step that fails.
    fail: Step,
    /// Threads the steps ran on.
    threads: Arc<Mutex<Vec<String>>>,
}

/// A step of [`Disk`] that [`Faulty`] fails.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    /// None: every step works.
    Nothing,
    /// The temp file is written in part, then the write fails.
    Write,
    /// The temp file is written and synced, then the rename fails.
    Rename,
}

impl Faulty {
    /// A disk failing `fail`.
    fn new(fail: Step) -> Self {
        Self {
            fail,
            threads: Arc::default(),
        }
    }

    /// Notes the thread a step runs on.
    fn note(&self) {
        let name = std::thread::current().name().unwrap_or("").to_owned();
        self.threads.lock().unwrap().push(name);
    }
}

impl Disk for Faulty {
    fn read(&mut self, path: &Path) -> io::Result<Option<String>> {
        self.note();
        SystemDisk.read(path)
    }

    fn create_dir_all(&mut self, dir: &Path) -> io::Result<()> {
        SystemDisk.create_dir_all(dir)
    }

    fn write_new(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.note();
        if self.fail == Step::Write {
            SystemDisk.write_new(path, &bytes[..bytes.len() / 2])?;
            return Err(io::Error::other("simulated: no space left on the disk"));
        }
        SystemDisk.write_new(path, bytes)
    }

    fn replace(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        if self.fail == Step::Rename {
            return Err(io::Error::other(
                "simulated: the power went before the rename",
            ));
        }
        SystemDisk.replace(from, to)
    }

    fn sync_dir(&mut self, dir: &Path) -> io::Result<()> {
        SystemDisk.sync_dir(dir)
    }

    fn remove(&mut self, path: &Path) -> io::Result<()> {
        SystemDisk.remove(path)
    }
}

#[test]
fn a_simulated_failed_write_leaves_the_previous_file_intact() {
    let (dir, target) = folder("failed-write");
    let first = exported(&target.studio, &[]);
    save(first, &target, &mut Faulty::new(Step::Nothing)).unwrap();
    let previous = std::fs::read(&target.path).unwrap();
    let changed = exported(
        &target.studio,
        &[EngineCommand::SetDirectorMode {
            mode: DirectorMode::Auto,
        }],
    );
    for (step, says) in [
        (Step::Rename, "cannot rename"),
        (Step::Write, "cannot write"),
    ] {
        let err = save(changed.clone(), &target, &mut Faulty::new(step)).unwrap_err();
        assert!(
            err.contains(says) && err.contains("simulated"),
            "{step:?}: {err}"
        );
        assert_eq!(
            std::fs::read(&target.path).unwrap(),
            previous,
            "{step:?}: the previous show, byte for byte"
        );
        assert_eq!(
            names(&dir.join("shows")),
            ["studio.show.toml"],
            "{step:?}: the temp file is removed"
        );
    }
    // The same save on a disk that works replaces it.
    save(changed, &target, &mut Faulty::new(Step::Nothing)).unwrap();
    assert_ne!(std::fs::read(&target.path).unwrap(), previous);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_show_that_would_not_open_or_a_file_that_is_not_a_show_is_left_alone() {
    let (dir, target) = folder("refused");
    let show = exported(&target.studio, &[]);
    // A target that is some other file.
    std::fs::create_dir_all(dir.join("shows")).unwrap();
    std::fs::write(&target.path, STUDIO).unwrap();
    let err = save(show.clone(), &target, &mut SystemDisk).unwrap_err();
    assert!(
        err.contains("is not a show file; it is left as it is"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&target.path).unwrap(), STUDIO);
    // A studio file that is gone: the show would not open.
    let gone = ShowTarget {
        path: dir.join("shows/new.show.toml"),
        studio: dir.join("moved.toml"),
        ..target.clone()
    };
    let err = save(show.clone(), &gone, &mut SystemDisk).unwrap_err();
    assert!(
        err.starts_with("the show would not open: cannot read"),
        "{err}"
    );
    // A value the config refuses (a transition no window sets).
    let mut long = show;
    long.switcher.duration = Some(60.0);
    let ok_target = ShowTarget {
        path: dir.join("shows/long.show.toml"),
        ..target
    };
    let err = save(long, &ok_target, &mut SystemDisk).unwrap_err();
    assert!(err.contains("switcher.duration 60"), "{err}");
    assert_eq!(names(&dir.join("shows")), ["studio.show.toml"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_save_runs_on_its_own_thread() {
    let (dir, target) = folder("thread");
    let disk = Faulty::new(Step::Nothing);
    let threads = disk.threads.clone();
    let show = exported(&target.studio, &[]);
    let mut job = SaveJob::start_with(show, target, Box::new(disk)).unwrap();
    let result = loop {
        if let Some(r) = job.try_finish() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    result.unwrap();
    let threads = threads.lock().unwrap().clone();
    assert!(!threads.is_empty());
    assert!(threads.iter().all(|t| t == "ac-save-show"), "{threads:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_query_added_at_runtime_is_back_after_a_save_and_restart() {
    let (dir, target) = folder("restart");
    let first = StudioConfig::load_with_warnings(&target.studio).unwrap();
    let target = ShowTarget::for_run(&first, &target.studio, Some(&target.path), no_dir()).unwrap();
    let engine = Engine::start_with(first.config, EngineOptions::default()).unwrap();
    let h = engine.handle();
    h.send(EngineCommand::AddQuery {
        name: "Guitarist".into(),
        prompt: "guitarist close-up".into(),
        presets: None,
        literal: false,
    })
    .unwrap();
    let dip = Transition {
        kind: TransitionKind::Dip,
        duration: 1.5,
    };
    h.send(EngineCommand::SetTransition { transition: dip })
        .unwrap();
    let before = h
        .wait_until(Duration::from_secs(10), |s| s.queries.len() == 2)
        .unwrap();
    assert_eq!(save_now(&h, &target).unwrap(), target.path);
    engine.shutdown().unwrap();
    // Restart from the show file, as `--config` does.
    let reopened = StudioConfig::load_with_warnings(&target.path).unwrap();
    assert!(reopened.show);
    assert_eq!(reopened.studio, target.studio);
    let engine = Engine::start_with(reopened.config, EngineOptions::default()).unwrap();
    let after = engine.snapshot();
    let ids: Vec<&QueryId> = after.queries.iter().map(|q| &q.id).collect();
    assert_eq!(
        ids,
        [&QueryId("person".into()), &QueryId("guitarist".into())]
    );
    for (a, b) in after.queries.iter().zip(&before.queries) {
        assert_eq!(
            (&a.name, &a.prompt, &a.parse, &a.presets),
            (&b.name, &b.prompt, &b.parse, &b.presets)
        );
    }
    assert_eq!(after.switcher.transition, dip);
    engine.shutdown().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
