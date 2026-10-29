//! One-test binaries that are also the fake detector sidecar.
//!
//! A test that counts the process's threads or children, or kills a
//! sidecar and watches it come back, runs in a binary of its own without
//! libtest's harness (`harness = false` in `Cargo.toml`), for two reasons:
//!
//! * it holds a single test, so no other test's threads or children are
//!   counted and no other test's sidecar is in the way;
//! * it is also the fake sidecar its studio runs. Started with
//!   [`FAKE_FLAG`], [`run`] speaks protocol v1 on stdin and stdout
//!   ([`fake_sidecar`]) instead of testing. `ac-sidecar`'s own
//!   `ac-fake-sidecar` binary cannot be named reliably from another
//!   crate's tests, and libtest would print to stdout, which carries the
//!   protocol.
//!
//! [`run`] reads the command line the way a one-test libtest binary does
//! ([`asked`]), so `cargo test` filters, `--skip`, `--ignored` and
//! `--list` treat the test like any other.

use std::io::{self, BufReader, BufWriter};
use std::process::ExitCode;
use std::time::Duration;

use ac_sidecar::protocol::{
    Capabilities, DetectResult, Found, Hello, Message, PROTOCOL_VERSION, read_message,
    write_message,
};

/// The argument that makes the binary the fake sidecar.
pub const FAKE_FLAG: &str = "--fake-sidecar";

/// The fake sidecar's model name, as its hello reports it.
pub const FAKE_MODEL: &str = "fake-for-engine-tests";

/// What the command line asks of the binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// Be the fake sidecar, with these arguments.
    Fake(Vec<String>),
    /// List the test if it is selected.
    List(bool),
    /// Run the test if it is selected.
    Run(bool),
}

/// libtest options that take a value as the next argument; it is not a
/// filter.
const VALUED: [&str; 7] = [
    "--test-threads",
    "--skip",
    "--format",
    "--color",
    "--logfile",
    "--shuffle-seed",
    "-Z",
];

/// Reads the command line like a binary whose one test is `name`:
/// filters (exact with `--exact`), `--skip`, `--ignored` (the test is not
/// ignored, so it selects nothing), `--list`, and the values of the
/// options in [`VALUED`], which are not filters. Other options are
/// accepted and ignored.
pub fn asked(args: &[String], name: &str) -> Asked {
    if args.first().map(String::as_str) == Some(FAKE_FLAG) {
        return Asked::Fake(args[1..].to_vec());
    }
    let (mut filters, mut skips) = (Vec::new(), Vec::new());
    let (mut exact, mut ignored_only, mut list) = (false, false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--list" => list = true,
            "--exact" => exact = true,
            "--ignored" => ignored_only = true,
            "--skip" => skips.extend(it.next().cloned()),
            a if a.starts_with("--skip=") => skips.push(a["--skip=".len()..].to_owned()),
            a if VALUED.contains(&a) => {
                it.next();
            }
            a if a.starts_with('-') => {}
            filter => filters.push(filter.to_owned()),
        }
    }
    let hits = |f: &String| {
        if exact {
            name == f
        } else {
            name.contains(f.as_str())
        }
    };
    let selected = !ignored_only
        && (filters.is_empty() || filters.iter().any(hits))
        && !skips.iter().any(hits);
    if list {
        Asked::List(selected)
    } else {
        Asked::Run(selected)
    }
}

/// Checks [`asked`] on the command lines `cargo test` passes to the binary
/// of test `name`.
fn check_asked(name: &str) {
    let ask = |line: &str| {
        asked(
            &line
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>(),
            name,
        )
    };
    let part = &name[..name.len() / 2];
    let cases = [
        (String::new(), Asked::Run(true)),
        ("--test-threads 1".into(), Asked::Run(true)),
        ("--test-threads=1 --nocapture".into(), Asked::Run(true)),
        (part.to_owned(), Asked::Run(true)),
        ("no-such-test".into(), Asked::Run(false)),
        (format!("--skip {part}"), Asked::Run(false)),
        (format!("--skip={part}"), Asked::Run(false)),
        ("--skip no-such-test".into(), Asked::Run(true)),
        ("--ignored".into(), Asked::Run(false)),
        ("--include-ignored".into(), Asked::Run(true)),
        (format!("--exact {part}"), Asked::Run(false)),
        (format!("--exact {name}"), Asked::Run(true)),
        ("--format terse --color never -q".into(), Asked::Run(true)),
        ("--list".into(), Asked::List(true)),
        ("--list --ignored".into(), Asked::List(false)),
        (
            format!("{FAKE_FLAG} --hang-after 4"),
            Asked::Fake(vec!["--hang-after".into(), "4".into()]),
        ),
    ];
    for (line, want) in cases {
        assert_eq!(ask(&line), want, "{line:?}");
    }
}

/// The binary's `main`: runs the fake sidecar or `test` (named `name`),
/// like a one-test libtest binary; see [`asked`]. A failed assertion in
/// `test` panics, and the process exits with status 101.
pub fn run(name: &str, test: fn()) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selected = match asked(&args, name) {
        Asked::Fake(args) => return fake_sidecar(&args),
        Asked::List(selected) => {
            if selected {
                println!("{name}: test");
            }
            return ExitCode::SUCCESS;
        }
        Asked::Run(selected) => selected,
    };
    check_asked(name);
    if !selected {
        println!("\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 1 filtered out\n");
        return ExitCode::SUCCESS;
    }
    println!("\nrunning 1 test");
    test();
    println!("test {name} ... ok\n\ntest result: ok. 1 passed; 0 failed\n");
    ExitCode::SUCCESS
}

/// The fake sidecar: hello, then one box per phrase for every request
/// after a short pause, until stdin ends. With `--hang-after N` it answers
/// N requests, then hangs in the next one and ignores stdin. It exits once
/// its parent is gone, so a failed test leaves none behind.
pub fn fake_sidecar(args: &[String]) -> ExitCode {
    let hang_after = match args {
        [] => None,
        [flag, n] if flag == "--hang-after" => match n.parse::<u32>() {
            Ok(n) => Some(n),
            Err(_) => return ExitCode::from(64),
        },
        _ => return ExitCode::from(64),
    };
    let parent = std::os::unix::process::parent_id();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if std::os::unix::process::parent_id() != parent {
                std::process::exit(5);
            }
        }
    });
    let mut input = BufReader::new(io::stdin().lock());
    let mut out = BufWriter::new(io::stdout().lock());
    let hello = Message::Hello(Hello {
        protocol: PROTOCOL_VERSION,
        model: FAKE_MODEL.into(),
        weights_sha256: None,
        input_side: 672,
        capabilities: Capabilities::default(),
    });
    if write_message(&mut out, &hello, &[]).is_err() {
        return ExitCode::from(5);
    }
    let mut answered = 0;
    loop {
        let detect = match read_message(&mut input) {
            Ok(Some((Message::Detect(d), _pixels))) => d,
            Ok(None) => return ExitCode::SUCCESS,
            Ok(Some(_)) | Err(_) => return ExitCode::from(4),
        };
        if hang_after.is_some_and(|n| answered >= n) {
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
        let detections = detect
            .phrases
            .iter()
            .zip(0..)
            .filter(|(p, _)| p.threshold <= 0.9)
            .map(|(_, phrase)| Found {
                phrase,
                bbox: [0.3, 0.2, 0.2, 0.6],
                score: 0.9,
            })
            .collect();
        let result = Message::Result(DetectResult {
            id: detect.id,
            source: detect.source,
            seq: detect.seq,
            pts_ns: detect.pts_ns,
            latency_ms: 20.0,
            detections,
        });
        if write_message(&mut out, &result, &[]).is_err() {
            return ExitCode::from(5);
        }
        answered += 1;
    }
}
