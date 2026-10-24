//! [`SamHost`] against the fake sidecar in every mode: answers, deadlines,
//! stale replies, protocol errors, crashes, restarts with backoff, stop
//! and shutdown. Every test checks that its children were reaped.

#![cfg(unix)]

mod common;

use std::thread;
use std::time::{Duration, Instant};

use ac_sidecar::{HostConfig, HostError, HostState, SamHost};

use common::{PROMPT, eventually, exists, fake, grandchild_pid, request, running};

/// A host for the fake in `mode`.
fn host(mode: &str, extra: &[&str]) -> SamHost {
    SamHost::new(fake(mode, extra)).unwrap()
}

/// The running child's pid, after waiting for its hello.
fn ready_pid(host: &mut SamHost) -> u32 {
    host.ready().unwrap();
    host.pid().unwrap()
}

/// Asserts `pid` was reaped (soon: procfs may lag a moment).
fn assert_reaped(pid: u32) {
    assert!(eventually(PROMPT, || !exists(pid)), "pid {pid} left behind");
}

#[test]
fn ok_mode_answers_each_phrase_and_exits_at_end_of_input() {
    let mut host = host("ok", &[]);
    let hello = host.ready().unwrap();
    assert_eq!((hello.model.as_str(), hello.input_side), ("fake-sam", 672));
    let pid = host.pid().unwrap();
    let reply = host
        .detect(request(3, &["a person", "a guitar", "a drum"], 0.5), PROMPT)
        .unwrap();
    let phrases: Vec<u32> = reply.detections.iter().map(|f| f.phrase).collect();
    assert_eq!(phrases, [0, 1, 2], "scores 0.9, 0.7, 0.5 all meet 0.5");
    assert_eq!(reply.detections[1].bbox, [0.25, 0.1, 0.15, 0.6]);
    assert!(reply.round_trip >= reply.latency);
    let reply = host
        .detect(request(4, &["a person", "a guitar"], 0.8), PROMPT)
        .unwrap();
    assert_eq!(reply.detections.len(), 1, "0.7 is below 0.8");
    let status = host.status();
    assert_eq!(status.state, HostState::Ready);
    assert_eq!(
        (
            status.stats.requests,
            status.stats.results,
            status.stats.spawns
        ),
        (2, 2, 1)
    );
    assert!(status.last_latency.is_some());
    // The fake logs on stderr, which a separate reader drains, so its line
    // may land just after the answer.
    assert!(
        eventually(PROMPT, || host
            .logs()
            .iter()
            .any(|l| l == "fake: detect id=1 4x2 phrases=3")),
        "{:?}",
        host.logs()
    );
    host.shutdown();
    assert_reaped(pid);
    let logs = host.logs();
    assert!(
        logs.iter().any(|l| l == "fake: input closed, exiting"),
        "{logs:?}"
    );
    assert!(
        logs.iter()
            .any(|l| l.contains("exited with exit status: 0 at shutdown")),
        "{logs:?}"
    );
    assert_eq!(host.status().state, HostState::Idle);
}

#[test]
fn large_frames_are_written_whole() {
    let mut host = host("ok", &[]);
    let pid = ready_pid(&mut host);
    let mut r = request(1, &["a person"], 0.5);
    r.image.width = 1008;
    r.image.height = 567;
    r.image.data = vec![9; 1008 * 567 * 3];
    host.detect(r, PROMPT).unwrap();
    // Logged on stderr, which may lag the answer on stdout.
    assert!(
        eventually(PROMPT, || host
            .logs()
            .iter()
            .any(|l| l.ends_with("1008x567 phrases=1"))),
        "{:?}",
        host.logs()
    );
    drop(host);
    assert_reaped(pid);
}

#[test]
fn a_slow_answer_within_its_deadline_is_kept_and_one_beyond_is_not() {
    let mut host = host("slow", &["--delay-ms", "300"]);
    let first = ready_pid(&mut host);
    let reply = host.detect(request(1, &["a person"], 0.5), PROMPT).unwrap();
    assert!(reply.round_trip >= Duration::from_millis(300));
    let err = host
        .detect(request(2, &["a person"], 0.5), Duration::from_millis(100))
        .unwrap_err();
    assert!(
        matches!(&err, HostError::Timeout { what, after } if what.contains("result") && *after == Duration::from_millis(100)),
        "{err}"
    );
    assert_reaped(first);
    let status = host.status();
    assert_eq!((status.stats.timeouts, status.stats.failures), (1, 1));
    assert!(matches!(status.state, HostState::Unavailable { .. }));
    // The next request after the backoff starts a new process.
    thread::sleep(Duration::from_millis(80));
    host.detect(request(3, &["a person"], 0.5), PROMPT).unwrap();
    let second = host.pid().unwrap();
    assert_ne!(first, second);
    assert_eq!(host.status().stats.spawns, 2);
    drop(host);
    assert_reaped(second);
}

#[test]
fn a_hung_sidecar_is_killed_at_the_deadline() {
    let mut host = host("hang", &[]);
    let pid = ready_pid(&mut host);
    let started = Instant::now();
    let err = host
        .detect(request(1, &["a person"], 0.5), Duration::from_millis(200))
        .unwrap_err();
    assert!(matches!(err, HostError::Timeout { .. }), "{err}");
    assert!(started.elapsed() < PROMPT);
    assert!(err.to_string().contains("killed by the host"), "{err}");
    assert_reaped(pid);
}

#[test]
fn stop_interrupts_a_request_and_drop_kills_a_child_that_ignores_its_input() {
    let mut host = host("hang", &[]);
    let pid = ready_pid(&mut host);
    let stop = host.stop_handle();
    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        stop.stop();
    });
    let started = Instant::now();
    let err = host.detect(request(1, &["a person"], 0.5), Duration::from_secs(60));
    assert_eq!(err, Err(HostError::Stopped));
    assert!(started.elapsed() < PROMPT);
    stopper.join().unwrap();
    assert_eq!(host.status().state, HostState::Stopped);
    assert!(exists(pid), "stop leaves the process to its owner");
    let started = Instant::now();
    drop(host);
    // The hung fake never reads its end of input: killed after the grace.
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(started.elapsed() < PROMPT);
    assert_reaped(pid);
}

#[test]
fn stop_interrupts_a_startup() {
    let config = HostConfig {
        startup_timeout: Duration::from_secs(60),
        ..fake("never-ready", &[])
    };
    let mut host = SamHost::new(config).unwrap();
    host.start().unwrap();
    let pid = host.pid().unwrap();
    let stop = host.stop_handle();
    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        stop.stop();
    });
    let started = Instant::now();
    assert_eq!(host.ready(), Err(HostError::Stopped));
    assert!(started.elapsed() < PROMPT);
    stopper.join().unwrap();
    drop(host);
    assert_reaped(pid);
}

#[test]
fn a_crash_after_hello_is_reported_then_restarted_with_backoff() {
    let config = HostConfig {
        restart_backoff: Duration::from_millis(100),
        max_restart_backoff: Duration::from_millis(400),
        ..fake("crash-after-hello", &[])
    };
    let mut host = SamHost::new(config).unwrap();
    let first = ready_pid(&mut host);
    let err = host
        .detect(request(1, &["a person"], 0.5), PROMPT)
        .unwrap_err();
    let text = err.to_string();
    assert!(matches!(err, HostError::Unavailable { .. }), "{text}");
    assert!(text.contains("exited during a request"), "{text}");
    assert!(text.contains("exit status: 3"), "{text}");
    assert!(text.contains("fake: crashing after hello"), "{text}");
    assert_reaped(first);
    // Refused at once while backing off.
    let err = host
        .detect(request(2, &["a person"], 0.5), PROMPT)
        .unwrap_err();
    assert!(
        matches!(err, HostError::Unavailable { retry_in: Some(d), .. } if d <= Duration::from_millis(100)),
        "{err}"
    );
    assert_eq!(host.status().stats.refused, 1);
    assert_eq!(host.status().stats.spawns, 1);
    // After the backoff, a new process, which crashes again; the wait doubles.
    thread::sleep(Duration::from_millis(120));
    assert!(host.detect(request(3, &["a person"], 0.5), PROMPT).is_err());
    let status = host.status();
    assert_eq!((status.stats.spawns, status.stats.failures), (2, 2));
    assert_eq!(status.consecutive_failures, 2);
    let err = host
        .detect(request(4, &["a person"], 0.5), PROMPT)
        .unwrap_err();
    assert!(
        matches!(err, HostError::Unavailable { retry_in: Some(d), .. } if d > Duration::from_millis(100)),
        "{err}"
    );
    assert_eq!(host.pid(), None);
}

#[test]
fn a_sidecar_that_crashes_once_recovers() {
    let dir = std::env::temp_dir().join(format!("ac-sidecar-once-{}", std::process::id()));
    let _ = std::fs::remove_file(&dir);
    let marker = dir.to_string_lossy().into_owned();
    let mut host = host("crash-once", &["--marker", &marker]);
    let first = ready_pid(&mut host);
    assert!(host.detect(request(1, &["a person"], 0.5), PROMPT).is_err());
    assert_reaped(first);
    thread::sleep(Duration::from_millis(80));
    let reply = host.detect(request(2, &["a person"], 0.5), PROMPT).unwrap();
    assert_eq!(reply.detections.len(), 1);
    let status = host.status();
    assert_eq!(status.state, HostState::Ready);
    assert_eq!((status.stats.spawns, status.stats.failures), (2, 1));
    assert_eq!(
        status.consecutive_failures, 0,
        "a result resets the backoff"
    );
    let second = host.pid().unwrap();
    drop(host);
    assert_reaped(second);
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn a_crash_before_hello_fails_the_startup() {
    let mut host = host("crash-before-hello", &[]);
    host.start().unwrap();
    let pid = host.pid().unwrap();
    let err = host.ready().unwrap_err();
    let text = err.to_string();
    assert!(text.contains("exited before hello"), "{text}");
    assert!(text.contains("exit status: 2"), "{text}");
    assert!(text.contains("fake: crashing before hello"), "{text}");
    assert_reaped(pid);
    assert!(matches!(host.status().state, HostState::Unavailable { .. }));
}

#[test]
fn bad_arguments_fail_like_a_crash_before_hello() {
    let mut host = host("no-such-mode", &[]);
    let err = host.ready().unwrap_err();
    assert!(err.to_string().contains("exit status: 64"), "{err}");
}

#[test]
fn a_sidecar_that_never_says_hello_misses_the_startup_deadline() {
    let config = HostConfig {
        startup_timeout: Duration::from_millis(300),
        ..fake("never-ready", &[])
    };
    let mut host = SamHost::new(config).unwrap();
    host.start().unwrap();
    let pid = host.pid().unwrap();
    let started = Instant::now();
    let err = host.ready().unwrap_err();
    assert!(
        matches!(&err, HostError::Timeout { what, after } if what.contains("hello") && *after == Duration::from_millis(300)),
        "{err}"
    );
    assert!(started.elapsed() < PROMPT);
    assert!(err.to_string().contains("fake: loading forever"), "{err}");
    assert_reaped(pid);
    assert_eq!(host.status().stats.timeouts, 1);
}

#[test]
fn garbage_on_stdout_is_a_protocol_error() {
    let mut host = host("garbage", &[]);
    let pid = ready_pid(&mut host);
    let err = host
        .detect(request(1, &["a person"], 0.5), PROMPT)
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("protocol error"), "{text}");
    assert!(text.contains("exceeds"), "{text}");
    assert_reaped(pid);
    let stats = host.status().stats;
    assert_eq!((stats.protocol_errors, stats.failures), (1, 1));
}

#[test]
fn replies_that_do_not_match_the_request_are_dropped_and_counted() {
    let mut host = host("wrong-seq", &[]);
    let pid = ready_pid(&mut host);
    let reply = host.detect(request(9, &["a person"], 0.5), PROMPT).unwrap();
    assert_eq!(reply.detections.len(), 1);
    let stats = host.status().stats;
    assert_eq!((stats.stale, stats.results, stats.failures), (4, 1, 0));
    drop(host);
    assert_reaped(pid);
}

#[test]
fn an_error_reply_fails_only_its_request() {
    let mut host = host("error-reply", &[]);
    let pid = ready_pid(&mut host);
    for seq in 0..2 {
        let err = host.detect(request(seq, &["a person"], 0.5), PROMPT);
        assert_eq!(err, Err(HostError::Rejected("fake failure".into())));
    }
    let status = host.status();
    assert_eq!(status.pid, Some(pid));
    assert_eq!(status.state, HostState::Ready);
    assert_eq!((status.stats.rejected, status.stats.failures), (2, 0));
    drop(host);
    assert_reaped(pid);
}

#[test]
fn a_fatal_error_ends_the_process() {
    let mut host = host("fatal-error", &[]);
    let pid = ready_pid(&mut host);
    let err = host
        .detect(request(1, &["a person"], 0.5), PROMPT)
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("fake fatal failure"), "{text}");
    assert!(text.contains("exit status: 4"), "{text}");
    assert_reaped(pid);
}

#[test]
fn an_early_hello_is_picked_up_after_start() {
    let mut host = host("ok", &[]);
    host.start().unwrap();
    assert_eq!(host.status().state, HostState::Starting);
    let pid = host.pid().unwrap();
    thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    assert_eq!(host.ready().unwrap().model, "fake-sam");
    assert!(started.elapsed() < Duration::from_millis(100));
    drop(host);
    assert_reaped(pid);
}

/// The pid of the helper the fake logged, waiting for the line.
fn helper_of(host: &SamHost) -> u32 {
    let mut helper = None;
    assert!(eventually(PROMPT, || {
        helper = grandchild_pid(&host.logs());
        helper.is_some()
    }));
    helper.unwrap()
}

#[test]
fn a_clean_exit_takes_the_sidecars_helpers_along() {
    let mut host = host("hang", &["--grandchild"]);
    let pid = ready_pid(&mut host);
    let helper = helper_of(&host);
    assert!(running(helper));
    // No request was sent, so the fake exits by itself at end of input.
    let started = Instant::now();
    host.shutdown();
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "no grace wait"
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("exit status: 0 at shutdown")),
        "{:?}",
        host.logs()
    );
    assert_reaped(pid);
    assert!(
        eventually(PROMPT, || !running(helper)),
        "helper {helper} survived"
    );
    assert_eq!(host.status().stats.detached_threads, 0, "pipes closed");
}

#[test]
fn killing_the_sidecar_kills_its_helpers_too() {
    let mut host = host("hang", &["--grandchild"]);
    let pid = ready_pid(&mut host);
    let helper = helper_of(&host);
    assert!(running(helper));
    let err = host.detect(request(1, &["a person"], 0.5), Duration::from_millis(200));
    assert!(matches!(err, Err(HostError::Timeout { .. })));
    assert_reaped(pid);
    assert!(
        eventually(PROMPT, || !running(helper)),
        "helper {helper} survived"
    );
    assert_eq!(host.status().stats.detached_threads, 0, "pipes closed");
}
