//! Tests for the readiness check against a fake machine: what is checked
//! for each kind of source, detector and output, that every problem is
//! listed, and what a config file that cannot be read yields.

use std::collections::{HashMap, HashSet};

use super::*;

/// A machine made of answers; anything not given is missing or fails.
#[derive(Debug, Default)]
struct Fake {
    /// What is at each path.
    entries: HashMap<PathBuf, Entry>,
    /// Text of the files read.
    texts: HashMap<PathBuf, String>,
    /// Tools that run, with their version line.
    versions: HashMap<PathBuf, String>,
    /// Programs on `PATH`.
    on_path: HashMap<String, PathBuf>,
    /// Folders that refuse new files.
    read_only: HashSet<PathBuf>,
    /// Every tool run, in order.
    ran: std::cell::RefCell<Vec<PathBuf>>,
}

impl Fake {
    /// The same machine with a readable file at `path`.
    fn file(mut self, path: &str) -> Self {
        let entry = Entry::File {
            readable: true,
            executable: false,
        };
        self.entries.insert(path.into(), entry);
        self
    }

    /// The same machine with `entry` at `path`.
    fn at(mut self, path: &str, entry: Entry) -> Self {
        self.entries.insert(path.into(), entry);
        self
    }

    /// The same machine with a readable folder at `path`.
    fn dir(self, path: &str) -> Self {
        self.at(path, Entry::Dir { readable: true })
    }

    /// The same machine with a working tool at `path`.
    fn tool(mut self, path: &str) -> Self {
        self.versions
            .insert(path.into(), "ffmpeg version 7.1-test".into());
        self
    }
}

impl Inspect for Fake {
    fn entry(&self, path: &Path) -> Entry {
        self.entries.get(path).cloned().unwrap_or(Entry::Missing)
    }

    fn can_create_in(&self, dir: &Path) -> Result<(), String> {
        if self.read_only.contains(dir) {
            Err("Permission denied (os error 13)".into())
        } else {
            Ok(())
        }
    }

    fn version(&self, tool: &Path) -> Result<String, String> {
        self.ran.borrow_mut().push(tool.into());
        self.versions
            .get(tool)
            .cloned()
            .ok_or_else(|| "not found".into())
    }

    fn find_program(&self, name: &str) -> Option<PathBuf> {
        self.on_path.get(name).cloned()
    }

    fn read_text(&self, path: &Path) -> Result<String, String> {
        self.texts
            .get(path)
            .cloned()
            .ok_or_else(|| "No such file or directory (os error 2)".into())
    }

    fn decoder_tools(&self) -> FfmpegTools {
        FfmpegTools {
            ffmpeg: "/bin/ffmpeg".into(),
            ffprobe: "/bin/ffprobe".into(),
        }
    }
}

/// A studio with one synthetic source, `ffmpeg` at `/bin/ffmpeg`, and
/// `more` after it.
fn synthetic(more: &str) -> StudioConfig {
    let text = format!(
        "schema = 1\n[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\nwidth = 640\n\
         height = 360\nfps = 30\n{more}"
    );
    with_ffmpeg(StudioConfig::parse(&text, false).unwrap())
}

/// A studio with one file source at `/studio/media/show.mp4`, `ffmpeg` at
/// `/bin/ffmpeg`, and `more` after it.
fn file_studio(more: &str) -> StudioConfig {
    let text = format!(
        "schema = 1\n[[sources]]\nid = \"cam-1\"\nkind = \"file\"\n\
         path = \"/studio/media/show.mp4\"\n{more}"
    );
    with_ffmpeg(StudioConfig::parse(&text, false).unwrap())
}

/// `c` encoding with `/bin/ffmpeg`, whatever the environment says.
fn with_ffmpeg(mut c: StudioConfig) -> StudioConfig {
    c.output.ffmpeg = "/bin/ffmpeg".into();
    c
}

/// The problems of `r`.
fn problems(r: &Readiness) -> Vec<&str> {
    r.problems().collect()
}

/// The one problem of `r`.
fn only_problem(r: &Readiness) -> &str {
    match problems(r).as_slice() {
        [one] => one,
        many => panic!("expected one problem, got {many:?}"),
    }
}

/// An OWL detector section with the split export under `/studio/models`.
const OWL: &str = r#"
[detector]
kind = "owl"
model = "/studio/models/detector.onnx"
text_encoder = "/studio/models/text_encoder.onnx"
tokenizer = "/studio/models/tokenizer.json"
"#;

/// A machine where [`OWL`]'s files and the file source are present.
fn owl_machine() -> Fake {
    Fake::default()
        .file("/studio/media/show.mp4")
        .file("/studio/models/detector.onnx")
        .file("/studio/models/text_encoder.onnx")
        .file("/studio/models/tokenizer.json")
        .tool("/bin/ffmpeg")
        .tool("/bin/ffprobe")
}

#[test]
fn a_synthetic_studio_needs_only_the_output_ffmpeg() {
    let fake = Fake::default().tool("/bin/ffmpeg");
    let r = check_build(&synthetic(""), &fake, false);
    assert!(r.is_ready(), "{r:?}");
    assert_eq!(*fake.ran.borrow(), [PathBuf::from("/bin/ffmpeg")]);
    let lines: Vec<&str> = r.findings.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(
        lines,
        [
            "config: schema 1, 1 source, 0 queries, detector \"none\"",
            "output.ffmpeg: /bin/ffmpeg runs (ffmpeg version 7.1-test)",
        ]
    );
    let r = check_build(&synthetic(""), &Fake::default(), false);
    let p = only_problem(&r);
    assert!(
        p.starts_with("output.ffmpeg: /bin/ffmpeg does not run: not found"),
        "{p}"
    );
}

#[test]
fn file_sources_need_their_video_and_the_decoding_tools() {
    let c = file_studio("");
    let fake = owl_machine();
    let r = check_build(&c, &fake, false);
    assert!(r.is_ready(), "{r:?}");
    assert_eq!(
        fake.ran.borrow().len(),
        2,
        "ffmpeg is run once for both uses"
    );
    let ok: Vec<&str> = r.at(Level::Ok).collect();
    assert!(
        ok.contains(&"source \"cam-1\": /studio/media/show.mp4 is readable"),
        "{ok:?}"
    );
    assert!(
        ok.iter()
            .any(|m| m.starts_with("output.ffmpeg and file sources' ffmpeg (AC_FFMPEG):")),
        "{ok:?}"
    );
    let r = check_build(&c, &Fake::default().tool("/bin/ffmpeg"), false);
    let p = problems(&r);
    assert_eq!(p.len(), 2, "{p:?}");
    assert_eq!(
        p[0],
        "source \"cam-1\": /studio/media/show.mp4 does not exist"
    );
    assert!(p[1].starts_with("file sources' ffprobe (AC_FFPROBE): /bin/ffprobe does not run"));
    let folder = owl_machine().dir("/studio/media/show.mp4");
    let r = check_build(&c, &folder, false);
    assert_eq!(
        only_problem(&r),
        "source \"cam-1\": /studio/media/show.mp4 is a folder, not a file"
    );
}

#[test]
fn an_owl_detector_needs_its_files_and_the_onnx_build() {
    let c = file_studio(OWL);
    assert!(check_build(&c, &owl_machine(), true).is_ready());
    let r = check_build(&c, &owl_machine(), false);
    assert!(only_problem(&r).contains("built with `--features onnx`"));
    let fake = owl_machine()
        .at("/studio/models/detector.onnx", Entry::Missing)
        .at(
            "/studio/models/text_encoder.onnx",
            Entry::File {
                readable: false,
                executable: false,
            },
        )
        .at(
            "/studio/models/tokenizer.json",
            Entry::BrokenLink("/Volumes/models/tokenizer.json".into()),
        );
    let r = check_build(&c, &fake, true);
    assert_eq!(
        problems(&r),
        [
            "detector.model: /studio/models/detector.onnx does not exist",
            "detector.text_encoder: /studio/models/text_encoder.onnx is not readable \
             (check its permissions)",
            "detector.tokenizer: /studio/models/tokenizer.json is a link to \
             /Volumes/models/tokenizer.json, which does not exist",
        ]
    );
}

/// A sidecar section running `program` on the server and model under
/// `/studio`.
fn sidecar(program: &str) -> StudioConfig {
    synthetic(&format!(
        "[detector]\nkind = \"sidecar\"\ncommand = [\"{program}\", \
         \"/studio/sidecars/sam3-mlx/server.py\", \"--backend\", \"mlx\", \"--model\", \
         \"/studio/models/sam31-mlx\", \"--side\", \"672\"]\n"
    ))
}

/// The venv's python.
const PYTHON: &str = "/studio/models/sam3/.venv/bin/python";

/// A machine with the sidecar's venv, server and weights.
fn sidecar_machine() -> Fake {
    let mut fake = Fake::default()
        .tool("/bin/ffmpeg")
        .at(
            PYTHON,
            Entry::File {
                readable: true,
                executable: true,
            },
        )
        .file("/studio/models/sam3/.venv/pyvenv.cfg")
        .dir("/opt/homebrew/opt/python@3.12/bin")
        .file("/studio/sidecars/sam3-mlx/server.py")
        .dir("/studio/models/sam31-mlx");
    fake.texts.insert(
        "/studio/models/sam3/.venv/pyvenv.cfg".into(),
        "home = /opt/homebrew/opt/python@3.12/bin\ninclude-system-site-packages = false\n\
         version = 3.12.4\n"
            .into(),
    );
    fake
}

#[test]
fn a_sidecar_needs_its_program_venv_and_paths_but_is_not_started() {
    let c = sidecar(PYTHON);
    let fake = sidecar_machine();
    let r = check_build(&c, &fake, false);
    assert!(r.is_ready(), "{r:?}");
    assert_eq!(fake.ran.borrow().len(), 1, "only ffmpeg runs");
    let ok: Vec<&str> = r.at(Level::Ok).collect();
    assert!(
        ok.contains(&"detector.command[0]: /studio/models/sam3/.venv/bin/python is executable")
    );
    assert!(ok.contains(
        &"detector venv: /studio/models/sam3/.venv (Python 3.12.4, from \
          /opt/homebrew/opt/python@3.12/bin)"
    ));
    assert!(ok.contains(&"detector.command[5]: folder /studio/models/sam31-mlx is readable"));

    let gone = sidecar_machine()
        .at("/opt/homebrew/opt/python@3.12/bin", Entry::Missing)
        .at("/studio/models/sam31-mlx", Entry::Missing);
    let r = check_build(&c, &gone, false);
    let p = problems(&r);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(p[0].contains("its base Python folder /opt/homebrew/opt/python@3.12/bin"));
    assert_eq!(
        p[1],
        "detector.command[5]: /studio/models/sam31-mlx does not exist"
    );

    let no_venv = sidecar_machine().at(PYTHON, Entry::Missing);
    let p = only_problem(&check_build(&c, &no_venv, false)).to_owned();
    assert!(
        p.contains("make the sidecar's venv as its README says"),
        "{p}"
    );
    let dangling = sidecar_machine().at(
        PYTHON,
        Entry::BrokenLink("/opt/homebrew/opt/python@3.11/bin/python3.11".into()),
    );
    let p = only_problem(&check_build(&c, &dangling, false)).to_owned();
    assert!(p.contains("python3.11, which does not exist"), "{p}");
    let plain = sidecar_machine().at(
        PYTHON,
        Entry::File {
            readable: true,
            executable: false,
        },
    );
    let p = only_problem(&check_build(&c, &plain, false)).to_owned();
    assert!(p.ends_with("is not executable"), "{p}");
}

#[test]
fn a_bare_sidecar_program_is_looked_up_on_path() {
    let c = sidecar("python3");
    let r = check_build(&c, &sidecar_machine(), false);
    assert!(only_problem(&r).contains("\"python3\" is not on PATH"));
    let mut fake = sidecar_machine();
    fake.on_path
        .insert("python3".into(), "/usr/bin/python3".into());
    let r = check_build(&c, &fake, false);
    assert!(r.is_ready(), "{r:?}");
    assert!(
        r.at(Level::Ok)
            .any(|m| m == "detector.command[0]: python3 is /usr/bin/python3 on PATH")
    );
    let empty = check_build(&sidecar(" "), &fake, false);
    assert_eq!(
        only_problem(&empty),
        "config: detector.command[0], the program, is empty",
        "said once, by the config's check"
    );
}

#[test]
fn a_tool_that_does_not_run_says_how_to_name_another() {
    let mut c = file_studio("");
    c.output.ffmpeg = "/opt/ffmpeg/bin/ffmpeg".into();
    let fake = Fake::default().file("/studio/media/show.mp4");
    let r = check_build(&c, &fake, false);
    let p = problems(&r);
    assert_eq!(p.len(), 3, "the output's own ffmpeg is run apart: {p:?}");
    assert!(
        p[0].ends_with("not found; install ffmpeg, or name it with AC_FFMPEG"),
        "{}",
        p[0]
    );
    assert!(p[1].ends_with("name it with AC_FFPROBE"), "{}", p[1]);
    assert!(
        p[2].starts_with("output.ffmpeg: /opt/ffmpeg/bin/ffmpeg does not run")
            && p[2].ends_with("name it with output.ffmpeg or AC_FFMPEG"),
        "{}",
        p[2]
    );
}

#[test]
fn a_recording_needs_a_new_name_in_a_folder_that_takes_files() {
    let c = synthetic(
        "[output]\nrecord_path = \"/studio/recordings/show.mp4\"\n\
         rtmp_url = \"rtmp://live.example.com/app/SECRET-KEY\"\n",
    );
    let fake = Fake::default()
        .tool("/bin/ffmpeg")
        .dir("/studio/recordings");
    let r = check_build(&c, &fake, false);
    assert!(r.is_ready(), "{r:?}");
    let all = format!("{r:?}");
    assert!(
        !all.contains("SECRET"),
        "the stream key is never shown: {all}"
    );
    assert!(
        r.at(Level::Ok)
            .any(|m| m.starts_with("output.rtmp_url: rtmp://live.example.com/app/"))
    );
    assert!(
        r.at(Level::Ok)
            .any(|m| m == "output.record_path: folder /studio/recordings takes new files")
    );

    let taken = Fake::default()
        .tool("/bin/ffmpeg")
        .dir("/studio/recordings")
        .file("/studio/recordings/show.mp4");
    let p = only_problem(&check_build(&c, &taken, false)).to_owned();
    assert!(
        p.contains("already exists; recordings are never overwritten"),
        "{p}"
    );
    let missing = Fake::default().tool("/bin/ffmpeg");
    let p = only_problem(&check_build(&c, &missing, false)).to_owned();
    assert!(
        p.contains("folder /studio/recordings does not exist"),
        "{p}"
    );
    let mut locked = Fake::default()
        .tool("/bin/ffmpeg")
        .dir("/studio/recordings");
    locked.read_only.insert("/studio/recordings".into());
    let p = only_problem(&check_build(&c, &locked, false)).to_owned();
    assert!(
        p.contains("cannot create a file in /studio/recordings: Permission denied"),
        "{p}"
    );
    let bare = synthetic("[output]\nrecord_path = \"show.mp4\"\n");
    let r = check_build(&bare, &Fake::default().tool("/bin/ffmpeg").dir("."), false);
    assert!(
        r.is_ready(),
        "a bare name records in the working folder: {r:?}"
    );
}

#[test]
fn every_problem_is_listed_bad_values_included() {
    let c = file_studio(
        "[output]\nkeyframe_seconds = 30\nrecord_path = \"/studio/recordings/show.mp4\"\n",
    );
    let fake = Fake::default().tool("/bin/ffmpeg").tool("/bin/ffprobe");
    let r = check_build(&c, &fake, false);
    assert!(!r.is_ready());
    assert_eq!(
        problems(&r),
        [
            "config: output: keyframe_seconds = 30 is outside 1..=10",
            "source \"cam-1\": /studio/media/show.mp4 does not exist",
            "output.record_path: folder /studio/recordings does not exist (starting the \
             output does not create it)",
        ]
    );
    let mut two = synthetic("[output]\nfps = 0\n[discovery]\nrate_hz = 0.0\n");
    two.sources[0].id = "a b".into();
    let p = problems(&check_build(
        &two,
        &Fake::default().tool("/bin/ffmpeg"),
        false,
    ))
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    assert_eq!(p.len(), 3, "one per table: {p:?}");
    assert!(p[0].contains("source id \"a b\""));
    assert!(p[1].contains("discovery.rate_hz"));
    assert!(p[2].contains("output: fps = 0"));
}

#[test]
fn a_file_that_cannot_be_read_is_the_only_problem() {
    let dir = std::env::temp_dir().join(format!("ac-engine-readiness-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = Fake::default().tool("/bin/ffmpeg");
    let r = check_file(&dir.join("missing.toml"), &fake);
    assert!(only_problem(&r).starts_with("cannot read"), "{r:?}");
    let path = dir.join("studio.toml");
    std::fs::write(&path, "schema = 7\n[[sources]]\nid = \"s\"\n").unwrap();
    let r = check_file(&path, &fake);
    assert!(only_problem(&r).contains("this file is schema 7"), "{r:?}");
    assert!(
        fake.ran.borrow().is_empty(),
        "nothing is run for a file that does not parse"
    );
    let old = "[[sources]]\nid = \"s\"\nkind = \"synthetic\"\nwidth = 64\nheight = 36\nfps = 30\n\
               [output]\nffmpeg = \"/bin/ffmpeg\"\n";
    std::fs::write(&path, old).unwrap();
    let r = check_file(&path, &fake);
    assert!(
        r.is_ready(),
        "a missing schema key is only a warning: {r:?}"
    );
    let warnings: Vec<&str> = r.warnings().collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("Add `schema = 1`"), "{warnings:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn venv_files_are_read_key_by_key() {
    let text = "home = /usr/bin\nversion_info = 3.13.1\nexecutable=/usr/bin/python3.13\nempty =\n";
    assert_eq!(venv_key(text, "home"), Some("/usr/bin"));
    assert_eq!(venv_key(text, "version_info"), Some("3.13.1"));
    assert_eq!(venv_key(text, "executable"), Some("/usr/bin/python3.13"));
    assert_eq!(venv_key(text, "empty"), None);
    assert_eq!(venv_key(text, "version"), None);
    assert_eq!(
        venv_root(Path::new("/a/.venv/bin/python")),
        Some(Path::new("/a/.venv"))
    );
    assert_eq!(venv_root(Path::new("/a/python")), None);
    assert_eq!(venv_root(Path::new("python")), None);
}
