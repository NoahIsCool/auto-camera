//! A stand-in sidecar for tests: speaks protocol v1 with no model, and
//! misbehaves on demand.
//!
//! ```text
//! ac-fake-sidecar <mode> [--delay-ms N] [--marker PATH] [--grandchild]
//! ```
//!
//! | Mode | Behaviour |
//! |---|---|
//! | `ok` | hello, then one deterministic box per phrase (see `answer`) |
//! | `slow` | like `ok`, but each answer waits `--delay-ms` (default 1500) |
//! | `hang` | hello, then never answers and ignores the end of its input |
//! | `crash-after-hello` | hello, then exits with status 3 on the first request |
//! | `crash-before-hello` | exits with status 2 before saying hello |
//! | `crash-once` | like `crash-after-hello` if `--marker` does not exist (and creates it), else like `ok` |
//! | `garbage` | hello, then answers a request with bytes that are not a frame |
//! | `wrong-seq` | hello, then four replies that do not match (wrong id, source, seq, PTS), then the right one |
//! | `never-ready` | never says hello and ignores its input |
//! | `error-reply` | hello, then an `error` reply with the request's id for every request |
//! | `fatal-error` | hello, then a fatal `error` (id null) on the first request, and exits with status 4 |
//!
//! `--grandchild` first starts `sleep 3600` sharing the fake's pipes and
//! logs its pid, so tests can check that killing the sidecar's process
//! group reaches helpers too.
//!
//! Like the real sidecar, it writes only protocol frames to stdout, logs
//! to stderr (`fake: ...` lines; each request logs its image size), and
//! exits with status 0 at the end of its input. Bad arguments exit with
//! status 64, a protocol error on its input with 65.

use std::io::{self, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

use ac_sidecar::protocol::{
    Capabilities, Detect, DetectResult, ErrorReply, Found, Hello, Message, PROTOCOL_VERSION,
    read_message, write_message,
};

/// How the fake behaves; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Answers correctly.
    Ok,
    /// Answers correctly after a delay.
    Slow,
    /// Never answers.
    Hang,
    /// Exits on the first request.
    CrashAfterHello,
    /// Exits before hello.
    CrashBeforeHello,
    /// Crashes the first time, answers afterwards.
    CrashOnce,
    /// Answers with bytes that are not a frame.
    Garbage,
    /// Sends mismatched replies before the right one.
    WrongSeq,
    /// Never says hello.
    NeverReady,
    /// Answers every request with an error.
    ErrorReply,
    /// Sends a fatal error and exits.
    FatalError,
}

impl Mode {
    /// Parses a mode name.
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "ok" => Mode::Ok,
            "slow" => Mode::Slow,
            "hang" => Mode::Hang,
            "crash-after-hello" => Mode::CrashAfterHello,
            "crash-before-hello" => Mode::CrashBeforeHello,
            "crash-once" => Mode::CrashOnce,
            "garbage" => Mode::Garbage,
            "wrong-seq" => Mode::WrongSeq,
            "never-ready" => Mode::NeverReady,
            "error-reply" => Mode::ErrorReply,
            "fatal-error" => Mode::FatalError,
            _ => return None,
        })
    }
}

/// Parsed command line.
#[derive(Debug)]
struct Args {
    /// The behaviour.
    mode: Mode,
    /// Delay of `slow` answers.
    delay: Duration,
    /// Marker file of `crash-once`.
    marker: Option<PathBuf>,
    /// Start a `sleep` helper first.
    grandchild: bool,
}

/// Parses the command line (without the program name).
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let name = args.next().ok_or("missing mode")?;
    let mode = Mode::parse(&name).ok_or_else(|| format!("unknown mode {name:?}"))?;
    let mut parsed = Args {
        mode,
        delay: Duration::from_millis(1500),
        marker: None,
        grandchild: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--delay-ms" => {
                let v = args.next().ok_or("--delay-ms needs a value")?;
                let ms: u64 = v.parse().map_err(|_| format!("bad --delay-ms {v:?}"))?;
                parsed.delay = Duration::from_millis(ms);
            }
            "--marker" => parsed.marker = Some(args.next().ok_or("--marker needs a path")?.into()),
            "--grandchild" => parsed.grandchild = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(parsed)
}

/// Sleeps for good.
fn hang() -> ! {
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

/// Left edges of the boxes of phrases 0 to 3 (then repeating).
const LEFT: [f64; 4] = [0.05, 0.25, 0.45, 0.65];

/// Scores of phrases 0 to 3 (then repeating), as exact literals so tests
/// can compare them with thresholds.
const SCORE: [f32; 4] = [0.9, 0.7, 0.5, 0.3];

/// The deterministic answer to `d`: phrase `i` gets the box
/// `[LEFT[i mod 4], 0.1, 0.15, 0.6]` with score `SCORE[i mod 4]`, kept when
/// it meets the phrase's threshold.
fn answer(d: &Detect, started: Instant) -> DetectResult {
    let detections = d
        .phrases
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let found = Found {
                phrase: i as u32,
                bbox: [LEFT[i % 4], 0.1, 0.15, 0.6],
                score: SCORE[i % 4],
            };
            (found.score >= p.threshold).then_some(found)
        })
        .collect();
    DetectResult {
        id: d.id,
        source: d.source.clone(),
        seq: d.seq,
        pts_ns: d.pts_ns,
        latency_ms: started.elapsed().as_secs_f64() * 1000.0,
        detections,
    }
}

/// Writes `message` with an empty payload.
fn send(out: &mut impl Write, message: Message) -> io::Result<()> {
    write_message(out, &message, &[]).map_err(io::Error::other)
}

/// Runs the fake; see the module docs.
fn run(args: Args) -> io::Result<ExitCode> {
    eprintln!("fake: mode {:?}", args.mode);
    if args.grandchild {
        let helper = Command::new("sleep").arg("3600").spawn()?;
        eprintln!("fake: grandchild {}", helper.id());
    }
    let mode = match (args.mode, &args.marker) {
        (Mode::CrashOnce, Some(marker)) if marker.exists() => Mode::Ok,
        (Mode::CrashOnce, Some(marker)) => {
            std::fs::write(marker, b"crashed once\n")?;
            Mode::CrashAfterHello
        }
        (mode, _) => mode,
    };
    match mode {
        Mode::CrashBeforeHello => {
            eprintln!("fake: crashing before hello");
            return Ok(ExitCode::from(2));
        }
        Mode::NeverReady => {
            eprintln!("fake: loading forever");
            hang()
        }
        _ => {}
    }
    let mut input = BufReader::new(io::stdin().lock());
    let mut out = BufWriter::new(io::stdout().lock());
    send(
        &mut out,
        Message::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            model: "fake-sam".into(),
            weights_sha256: None,
            input_side: 672,
            capabilities: Capabilities::default(),
        }),
    )?;
    eprintln!("fake: ready");
    loop {
        let (message, _payload) = match read_message(&mut input) {
            Ok(Some(m)) => m,
            Ok(None) => {
                eprintln!("fake: input closed, exiting");
                return Ok(ExitCode::SUCCESS);
            }
            Err(e) => {
                eprintln!("fake: bad input: {e}");
                return Ok(ExitCode::from(65));
            }
        };
        let Message::Detect(d) = message else {
            eprintln!("fake: unexpected {} message", message.kind());
            return Ok(ExitCode::from(65));
        };
        let started = Instant::now();
        eprintln!(
            "fake: detect id={} {}x{} phrases={}",
            d.id,
            d.width,
            d.height,
            d.phrases.len()
        );
        match mode {
            Mode::Ok | Mode::CrashOnce => send(&mut out, Message::Result(answer(&d, started)))?,
            Mode::Slow => {
                thread::sleep(args.delay);
                send(&mut out, Message::Result(answer(&d, started)))?;
            }
            Mode::Hang => hang(),
            Mode::CrashAfterHello => {
                eprintln!("fake: crashing after hello");
                return Ok(ExitCode::from(3));
            }
            Mode::Garbage => {
                out.write_all(b"this is not a frame\n")?;
                out.flush()?;
            }
            Mode::WrongSeq => {
                let right = answer(&d, started);
                let wrong = [
                    DetectResult {
                        id: d.id + 1,
                        ..right.clone()
                    },
                    DetectResult {
                        source: format!("{}-other", d.source),
                        ..right.clone()
                    },
                    DetectResult {
                        seq: d.seq.wrapping_add(1),
                        ..right.clone()
                    },
                    DetectResult {
                        pts_ns: d.pts_ns.wrapping_add(1),
                        ..right.clone()
                    },
                ];
                for w in wrong {
                    send(&mut out, Message::Result(w))?;
                }
                send(&mut out, Message::Result(right))?;
            }
            Mode::ErrorReply => send(
                &mut out,
                Message::Error(ErrorReply {
                    id: Some(d.id),
                    message: "fake failure".into(),
                }),
            )?,
            Mode::FatalError => {
                send(
                    &mut out,
                    Message::Error(ErrorReply {
                        id: None,
                        message: "fake fatal failure".into(),
                    }),
                )?;
                eprintln!("fake: exiting after a fatal error");
                return Ok(ExitCode::from(4));
            }
            Mode::CrashBeforeHello | Mode::NeverReady => unreachable!("handled before hello"),
        }
    }
}

/// Parses the arguments and runs the fake.
fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("fake: {e}");
            return ExitCode::from(64);
        }
    };
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fake: I/O error: {e}");
            ExitCode::from(74)
        }
    }
}
