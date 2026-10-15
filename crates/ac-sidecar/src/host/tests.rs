//! Unit tests of the host that need no sidecar process.

use super::*;
use crate::prep::RgbImage;
use crate::protocol::Phrase;

/// Lines as owned strings.
fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_last_child_line_belongs_to_the_current_process() {
    let log = lines(&[
        "old process's last words",
        "[host] started pid 2: python server.py",
        "loading weights",
        "[host] hello: model m, input side 672, weights unchecked",
    ]);
    assert_eq!(
        last_child_line(&log).map(String::as_str),
        Some("loading weights")
    );
    let silent = lines(&["old words", "[host] started pid 3: python server.py"]);
    assert_eq!(last_child_line(&silent), None);
    assert_eq!(
        last_child_line(&lines(&["no host notes"])).unwrap(),
        "no host notes"
    );
    assert_eq!(last_child_line(&[]), None);
}

#[test]
fn configs_are_checked() {
    assert!(HostConfig::default().validate().is_err());
    assert!(HostConfig::new([" "]).validate().is_err());
    let ok = HostConfig::new(["python3", "server.py"]);
    assert_eq!(ok.command, ["python3", "server.py"]);
    assert!(ok.validate().is_ok());
    let zero = HostConfig {
        startup_timeout: Duration::ZERO,
        ..ok.clone()
    };
    assert!(zero.validate().is_err());
    let inverted = HostConfig {
        restart_backoff: Duration::from_secs(5),
        max_restart_backoff: Duration::from_secs(1),
        ..ok.clone()
    };
    assert!(inverted.validate().is_err());
    let no_logs = HostConfig { log_lines: 0, ..ok };
    assert!(matches!(no_logs.validate(), Err(HostError::Config(_))));
}

#[test]
fn a_new_host_is_idle_and_starts_nothing() {
    let host = SamHost::new(HostConfig::new(["/nonexistent/sidecar"])).unwrap();
    let status = host.status();
    assert_eq!(status.state, HostState::Idle);
    assert_eq!(status.pid, None);
    assert_eq!(status.stats, HostStats::default());
}

#[test]
fn a_missing_program_fails_to_spawn_and_then_backs_off() {
    let mut host = SamHost::new(HostConfig::new(["/nonexistent/sidecar"])).unwrap();
    assert!(matches!(host.start(), Err(HostError::Spawn(m)) if m.contains("nonexistent")));
    assert!(matches!(
        host.ready(),
        Err(HostError::Unavailable {
            retry_in: Some(_),
            ..
        })
    ));
    let status = host.status();
    assert_eq!(status.stats.failures, 1);
    assert_eq!(status.stats.refused, 1);
    assert!(matches!(status.state, HostState::Unavailable { .. }));
}

#[test]
fn invalid_requests_are_refused_without_starting_anything() {
    let mut host = SamHost::new(HostConfig::new(["/nonexistent/sidecar"])).unwrap();
    let request = DetectRequest {
        source: "cam".into(),
        seq: 0,
        pts: Duration::ZERO,
        image: RgbImage {
            width: 1,
            height: 1,
            data: vec![0; 3],
        },
        phrases: Vec::new(),
    };
    let err = host.detect(request.clone(), Duration::from_secs(1));
    assert!(matches!(err, Err(HostError::Invalid(m)) if m.contains("phrases")));
    let mut short = request.clone();
    short.phrases = vec![Phrase {
        text: "a person".into(),
        threshold: 0.5,
    }];
    short.image.data.pop();
    assert!(matches!(
        host.detect(short.clone(), Duration::from_secs(1)),
        Err(HostError::Invalid(_))
    ));
    short.image.data.push(0);
    assert!(matches!(
        host.detect(short, Duration::ZERO),
        Err(HostError::Invalid(_))
    ));
    assert_eq!(host.status().stats.spawns, 0);
}

#[test]
fn a_stopped_host_refuses_work() {
    let mut host = SamHost::new(HostConfig::new(["/nonexistent/sidecar"])).unwrap();
    let stop = host.stop_handle();
    assert!(!stop.is_stopped());
    stop.stop();
    stop.stop();
    assert!(stop.is_stopped());
    assert_eq!(host.start(), Err(HostError::Stopped));
    assert_eq!(host.ready(), Err(HostError::Stopped));
    assert_eq!(host.status().state, HostState::Stopped);
}

#[test]
fn errors_read_well() {
    let e = HostError::Unavailable {
        reason: "the sidecar exited".into(),
        retry_in: Some(Duration::from_millis(1500)),
    };
    assert_eq!(
        e.to_string(),
        "the sidecar is unavailable: the sidecar exited; retrying in 1.5 s"
    );
    let soon = HostError::Unavailable {
        reason: "x".into(),
        retry_in: Some(Duration::from_micros(39_200)),
    };
    assert_eq!(
        soon.to_string(),
        "the sidecar is unavailable: x; retrying in 40 ms"
    );
    let e = HostError::Timeout {
        what: "the sidecar's result".into(),
        after: Duration::from_secs(5),
    };
    assert_eq!(e.to_string(), "the sidecar's result timed out after 5s");
}
