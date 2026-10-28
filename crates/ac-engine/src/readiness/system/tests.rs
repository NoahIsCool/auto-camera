//! Tests for [`SystemInspector`] on this machine: entries in a scratch
//! folder, the probe file, `PATH` lookups and running tools, with `sh`
//! standing in for ffmpeg (so no executable is written while other tests
//! may be spawning processes) and the real ffmpeg when it is installed.

use super::*;

/// A fresh, empty scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("ac-engine-inspector-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// `sh -c script`, run as a tool would be (with `-version` after it).
fn sh(script: &str, timeout: Duration) -> Result<String, String> {
    let mut command = Command::new("sh");
    command.args(["-c", script, "-version"]);
    run_version(command, timeout)
}

#[test]
fn entries_say_what_is_there() {
    let dir = scratch("entries");
    let file = dir.join("model.onnx");
    fs::write(&file, b"weights").unwrap();
    let inspector = SystemInspector::default();
    assert!(matches!(
        inspector.entry(&file),
        Entry::File { readable: true, .. }
    ));
    assert_eq!(inspector.entry(&dir), Entry::Dir { readable: true });
    assert_eq!(inspector.entry(&dir.join("nothing")), Entry::Missing);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let link = dir.join("python");
        std::os::unix::fs::symlink(dir.join("gone/python3.12"), &link).unwrap();
        assert_eq!(
            inspector.entry(&link),
            Entry::BrokenLink(dir.join("gone/python3.12"))
        );
        let script = dir.join("server.py");
        fs::write(&script, "print('hi')\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            inspector.entry(&script),
            Entry::File {
                executable: false,
                ..
            }
        ));
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            inspector.entry(&script),
            Entry::File {
                executable: true,
                ..
            }
        ));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        // A privileged user (root in a container) opens it anyway.
        if File::open(&file).is_err() {
            assert!(matches!(
                inspector.entry(&file),
                Entry::File {
                    readable: false,
                    ..
                }
            ));
        }
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_eq!(
        inspector.read_text(&file).as_deref(),
        Ok("weights"),
        "read_text"
    );
    assert!(inspector.read_text(&dir.join("nothing")).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_probe_file_is_made_and_removed() {
    let dir = scratch("probe");
    let inspector = SystemInspector::default();
    inspector.can_create_in(&dir).unwrap();
    inspector.can_create_in(&dir).unwrap();
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0, "no probe is left");
    assert!(inspector.can_create_in(&dir.join("missing")).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn programs_are_found_on_path() {
    let inspector = SystemInspector::default();
    let sh = inspector.find_program("sh").expect("sh is on PATH");
    assert!(sh.is_absolute() && sh.ends_with("sh"), "{}", sh.display());
    assert_eq!(
        inspector.find_program("no-such-program-for-ac-engine"),
        None
    );
}

#[test]
fn a_tool_runs_when_it_prints_a_version_and_exits_zero() {
    let second = Duration::from_secs(5);
    assert_eq!(
        sh(
            "echo 'ffmpeg version 7.1 Copyright (c) 2000-2024 the FFmpeg developers'; echo built",
            second
        ),
        Ok("ffmpeg version 7.1".into())
    );
    let msg = sh("echo 'ffmpeg version 7.1'; exit 3", second).unwrap_err();
    assert!(msg.contains("failed"), "{msg}");
    let msg = sh(":", second).unwrap_err();
    assert!(msg.contains("printed nothing"), "{msg}");
    let msg = sh("yes ffmpeg | head -c 200000", second).unwrap();
    assert_eq!(msg, "ffmpeg", "a chatty tool does not block on its pipe");
    let msg = SystemInspector::default()
        .version(Path::new("/no/such/dir/ffmpeg"))
        .unwrap_err();
    assert_eq!(msg, "not found");
}

#[test]
fn a_hung_tool_is_killed_at_the_deadline() {
    let started = Instant::now();
    let msg = sh("exec sleep 30", Duration::from_millis(200)).unwrap_err();
    assert!(msg.contains("did not finish within 0.2 s"), "{msg}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "it was stopped"
    );
}

/// True once the process `pid` is gone or a zombie (killed, awaiting a
/// reaper that may never come in a container), within five seconds.
#[cfg(unix)]
fn ends(pid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .expect("ps runs");
        let stat = String::from_utf8_lossy(&out.stdout);
        if stat.trim().is_empty() || stat.trim_start().starts_with('Z') {
            return true;
        }
        thread::sleep(POLL);
    }
    false
}

#[test]
#[cfg(unix)]
fn a_wrapper_is_stopped_at_the_deadline_with_what_it_started() {
    let dir = scratch("wrapper");
    let timeout = Duration::from_millis(300);
    // The shell forks `sleep`, which holds the output after the shell dies.
    let started = Instant::now();
    let msg = sh("sleep 30; :", timeout).unwrap_err();
    assert!(msg.contains("did not finish within 0.3 s"), "{msg}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "returned at the deadline, after {:?}",
        started.elapsed()
    );
    let pid_file = dir.join("pid");
    let msg = sh(
        &format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
        timeout,
    )
    .unwrap_err();
    assert!(msg.contains("did not finish"), "{msg}");
    let pid = fs::read_to_string(&pid_file).unwrap();
    assert!(ends(pid.trim()), "the wrapper's child {pid} was killed");
    // A tool that exits at once but leaves a child holding its output is
    // not waited for either.
    let started = Instant::now();
    let msg = sh(
        &format!(
            "echo 'ffmpeg version 7.1'; sleep 30 & echo $! > '{}'",
            pid_file.display()
        ),
        timeout,
    )
    .unwrap_err();
    assert!(msg.contains("did not finish"), "{msg}");
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid = fs::read_to_string(&pid_file).unwrap();
    assert!(ends(pid.trim()), "the tool's child {pid} was killed");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_real_ffmpeg_reports_its_version() {
    let tools = FfmpegTools::from_env();
    if !tools.is_available() {
        let required =
            std::env::var_os("AC_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty() && v != "0");
        assert!(!required, "ffmpeg is required (AC_REQUIRE_FFMPEG)");
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let inspector = SystemInspector::default();
    for tool in [&tools.ffmpeg, &tools.ffprobe] {
        let version = inspector.version(tool).unwrap();
        assert!(version.contains(" version "), "{version}");
        assert!(!version.contains("Copyright"), "{version}");
    }
    assert_eq!(inspector.decoder_tools(), tools);
}
