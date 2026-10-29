//! Decoder stderr in the app's log.
//!
//! # Theory
//!
//! Decoders run with `-loglevel error`, so whatever an ffmpeg decoder
//! writes to stderr is a complaint worth keeping: a corrupt packet, a file
//! that went away. [`forward`] pipes each child's stderr to a short-lived
//! thread (`ac-src-log`) that reads it line by line and hands the lines to
//! `tracing` as warnings of target [`TARGET`], with the child's pid and its
//! input, so the app's log says which file a line is about. The source
//! takes no decision from these lines: its health still comes from the
//! decoder's output and exit. This crate installs no subscriber; the app
//! decides where the lines go.
//!
//! A damaged file can make a decoder complain about every frame, mostly in
//! a few lines that differ only in their numbers ("error while decoding MB
//! 12 34"), which would fill the app's log within hours. So each drain
//! holds back repeats ([`Repeats`]): the first line of each shape (digits
//! aside) is logged, a line of that shape again only [`QUIET`] after, saying
//! how many like it were held back meanwhile, and what is still held back
//! when the decoder ends is counted then.
//!
//! A source can also keep a child's first and last lines ([`StderrTail`],
//! given to [`forward`]), because ffmpeg says why it refused an input
//! only there: a capture device's "Selected framerate is not supported" and
//! its list of supported modes ([`crate::capture`]). The source still takes
//! no decision from them; they only explain, in its error, a failure its
//! decoder's output and exit already showed.
//!
//! # Invariants
//!
//! * The thread owns the pipe and ends at its end of file, which the
//!   child's exit or kill brings; [`crate::process::Children`] kills every
//!   child it owns, so no drain outlives its source by more than that.
//! * The pipe is always drained, so a child never blocks on a full pipe,
//!   and memory is bounded: a line is cut at [`MAX_LINE`] bytes (the rest
//!   of it is skipped and the line marked), and at most [`SHAPES`] shapes
//!   are remembered.
//! * Every line is either logged or counted in a later line; none is lost
//!   without a trace.
//! * Empty lines are not logged; line endings are trimmed.
//! * A tail is bounded and ends with its drain (see [`tail`]).

use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod tail;

pub(crate) use tail::{StderrTail, TAIL_FIRST, TAIL_OPENING, strip_context};

/// The target of decoder stderr lines in the app's log.
pub(crate) const TARGET: &str = "ac_sources::ffmpeg";

/// Longest line logged, in bytes; the rest of a longer line is skipped.
pub(crate) const MAX_LINE: usize = 2000;

/// Marker appended to a line that was cut at [`MAX_LINE`].
const CUT: &str = " [...]";

/// Time after a line of a shape was logged before another of that shape
/// is logged.
pub(crate) const QUIET: Duration = Duration::from_secs(60);

/// Shapes a drain remembers; a new one beyond them forgets the shape
/// logged longest ago (counting what it held back).
pub(crate) const SHAPES: usize = 8;

/// Starts the thread that sends `child`'s stderr to the log, if `child`
/// has a stderr pipe; `command` is what started it. Every line is also
/// offered to `tail`, if given, which is ended when the pipe ends (at once
/// when there is no pipe). Fails only when the thread cannot be spawned
/// (the caller then kills the child, whose writes would otherwise hit a
/// closed pipe).
pub(crate) fn forward(
    child: &mut Child,
    command: &Command,
    tail: Option<Arc<StderrTail>>,
) -> io::Result<()> {
    let Some(stderr) = child.stderr.take() else {
        if let Some(tail) = tail {
            tail.end();
        }
        return Ok(());
    };
    let (pid, input) = (child.id(), input_of(command));
    std::thread::Builder::new()
        .name("ac-src-log".into())
        .spawn(move || {
            let started = Instant::now();
            let mut repeats = Repeats::default();
            let mut log = |text: &str| {
                tracing::warn!(target: TARGET, pid, input = %input, "{text}");
            };
            drain(BufReader::new(stderr), |line| {
                if let Some(tail) = &tail {
                    tail.push(line);
                }
                repeats.offer(line, started.elapsed(), &mut log);
            });
            repeats.finish(&mut log);
            if let Some(tail) = &tail {
                tail.end();
            }
        })?;
    Ok(())
}

/// A shape a drain has logged a line of; see [`Repeats`].
#[derive(Debug)]
struct Shape {
    /// The shape: the line with every run of digits replaced by `#`.
    shape: String,
    /// When a line of it was last logged, since the drain started.
    logged: Duration,
    /// Lines of it held back since then.
    held: u64,
    /// The newest line held back.
    example: String,
}

/// Holds back repeats of one decoder's lines; see the module docs.
#[derive(Debug, Default)]
struct Repeats {
    /// The shapes remembered, at most [`SHAPES`].
    shapes: Vec<Shape>,
}

impl Repeats {
    /// Logs `line`, seen `now` after the drain started, through `log`, or
    /// holds it back as a repeat.
    fn offer(&mut self, line: &str, now: Duration, log: &mut impl FnMut(&str)) {
        let shape = shape(line);
        if let Some(s) = self.shapes.iter_mut().find(|s| s.shape == shape) {
            if now.saturating_sub(s.logged) < QUIET {
                s.held += 1;
                line.clone_into(&mut s.example);
                return;
            }
            s.logged = now;
            match std::mem::take(&mut s.held) {
                0 => log(line),
                held => log(&format!(
                    "{line} ({held} more like it since the last logged)"
                )),
            }
            return;
        }
        if self.shapes.len() >= SHAPES
            && let Some((oldest, _)) = self.shapes.iter().enumerate().min_by_key(|(_, s)| s.logged)
        {
            count(&self.shapes.swap_remove(oldest), log);
        }
        self.shapes.push(Shape {
            shape,
            logged: now,
            held: 0,
            example: String::new(),
        });
        log(line);
    }

    /// Logs a count of every shape's lines still held back.
    fn finish(&mut self, log: &mut impl FnMut(&str)) {
        for s in self.shapes.drain(..) {
            count(&s, log);
        }
    }
}

/// Logs how many lines of `shape` were held back, if any.
fn count(shape: &Shape, log: &mut impl FnMut(&str)) {
    if shape.held > 0 {
        log(&format!(
            "{} more like this one held back: {}",
            shape.held, shape.example
        ));
    }
}

/// `text` with every run of digits replaced by `#`, so lines that differ
/// only in their numbers read the same.
fn shape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_number = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('#');
            }
            in_number = true;
        } else {
            out.push(c);
            in_number = false;
        }
    }
    out
}

/// What `command` decodes: the argument after its last `-i`, else its
/// program.
pub(crate) fn input_of(command: &Command) -> String {
    let args: Vec<_> = command.get_args().collect();
    let input = args
        .iter()
        .rposition(|a| *a == "-i")
        .and_then(|i| args.get(i + 1))
        .copied()
        .unwrap_or_else(|| command.get_program());
    input.to_string_lossy().into_owned()
}

/// Calls `each` with every non-empty line of `input` (cut at
/// [`MAX_LINE`]) until it ends or fails.
pub(crate) fn drain(mut input: impl BufRead, mut each: impl FnMut(&str)) {
    while let Some(line) = read_line(&mut input, MAX_LINE) {
        if !line.trim().is_empty() {
            each(&line);
        }
    }
}

/// Reads one line of at most `cap` bytes, lossily decoded and without its
/// line ending; the rest of a longer line is skipped and the line marked.
/// `None` at the end of input or on a read error.
fn read_line(input: &mut impl BufRead, cap: usize) -> Option<String> {
    let mut line = Vec::new();
    let read = input
        .by_ref()
        .take(cap as u64)
        .read_until(b'\n', &mut line)
        .ok()?;
    if read == 0 {
        return None;
    }
    // A full buffer without its newline: skip the rest of the line. A
    // skip of the newline alone means the line just fitted.
    let cut = line.last() != Some(&b'\n')
        && line.len() == cap
        && input.skip_until(b'\n').is_ok_and(|skipped| skipped > 1);
    let mut text = String::from_utf8_lossy(&line)
        .trim_end_matches(['\n', '\r'])
        .to_owned();
    if cut {
        text.push_str(CUT);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines `drain` reports for `bytes`.
    fn lines(bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        drain(bytes, |l| out.push(l.to_owned()));
        out
    }

    #[test]
    fn lines_are_split_trimmed_and_blank_ones_skipped() {
        assert_eq!(
            lines(b"first error\r\n\n  \nsecond\nlast without newline"),
            ["first error", "second", "last without newline"]
        );
        assert!(lines(b"").is_empty());
    }

    #[test]
    fn long_lines_are_cut_and_marked() {
        let mut input = &b"abcdefgh\nabc\nij"[..];
        assert_eq!(read_line(&mut input, 3).as_deref(), Some("abc [...]"));
        // A line that just fits is not marked.
        assert_eq!(read_line(&mut input, 3).as_deref(), Some("abc"));
        assert_eq!(read_line(&mut input, 3).as_deref(), Some("ij"));
        assert_eq!(read_line(&mut input, 3), None);
        let long = vec![b'x'; 3 * MAX_LINE];
        let got = lines(&long);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), MAX_LINE + CUT.len());
    }

    #[test]
    fn invalid_utf8_is_replaced_and_read_errors_end_the_drain() {
        assert_eq!(lines(b"caf\xe9\n"), ["caf\u{fffd}"]);
        /// A reader that always fails.
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
        }
        let mut n = 0;
        drain(BufReader::new(Broken), |_| n += 1);
        assert_eq!(n, 0);
    }

    /// What `Repeats` logs for `lines`, each seen at its time in seconds,
    /// and at the end.
    fn throttled(lines: &[(u64, &str)]) -> Vec<String> {
        let mut out = Vec::new();
        let mut log = |t: &str| out.push(t.to_owned());
        let mut repeats = Repeats::default();
        for &(secs, line) in lines {
            repeats.offer(line, Duration::from_secs(secs), &mut log);
        }
        repeats.finish(&mut log);
        out
    }

    #[test]
    fn repeats_are_held_back_and_counted() {
        let q = QUIET.as_secs();
        let got = throttled(&[
            (0, "[h264 @ 0x5a] error while decoding MB 12 34"),
            (0, "[h264 @ 0x5a] concealing 1200 DC errors"),
            (1, "[h264 @ 0x5a] error while decoding MB 7 3"),
            (2, "[h264 @ 0x5a] concealing 9 DC errors"),
            (3, "[h264 @ 0x5a] error while decoding MB 8 1"),
            (q, "[h264 @ 0x5a] error while decoding MB 1 1"),
            (q + 1, "[h264 @ 0x5a] error while decoding MB 2 2"),
            (q + 2, "Invalid data found when processing input"),
        ]);
        assert_eq!(
            got,
            [
                "[h264 @ 0x5a] error while decoding MB 12 34",
                "[h264 @ 0x5a] concealing 1200 DC errors",
                "[h264 @ 0x5a] error while decoding MB 1 1 (2 more like it since the last logged)",
                "Invalid data found when processing input",
                "1 more like this one held back: [h264 @ 0x5a] error while decoding MB 2 2",
                "1 more like this one held back: [h264 @ 0x5a] concealing 9 DC errors",
            ]
        );
        // A line that comes back after a quiet spell is logged plainly.
        let again = throttled(&[(0, "lost sync 1"), (q, "lost sync 2")]);
        assert_eq!(again, ["lost sync 1", "lost sync 2"]);
        assert!(throttled(&[]).is_empty());
    }

    #[test]
    fn a_new_shape_beyond_the_limit_counts_the_oldest_one_out() {
        let mut lines: Vec<(u64, String)> = (0..SHAPES as u64)
            .map(|i| (i, format!("kind {} error", "x".repeat(i as usize + 1))))
            .collect();
        // The first shape repeats, then one shape too many arrives.
        lines.push((SHAPES as u64, "kind x error".into()));
        lines.push((SHAPES as u64, "a new kind".into()));
        lines.push((SHAPES as u64 + 1, "kind x error".into()));
        let borrowed: Vec<_> = lines.iter().map(|(t, l)| (*t, l.as_str())).collect();
        let got = throttled(&borrowed);
        assert_eq!(got.len(), SHAPES + 3, "{got:?}");
        assert_eq!(got[SHAPES], "1 more like this one held back: kind x error");
        assert_eq!(got[SHAPES + 1], "a new kind");
        // Forgotten, the first shape is logged again at once.
        assert_eq!(got[SHAPES + 2], "kind x error");
    }

    #[test]
    fn shapes_ignore_numbers_only() {
        assert_eq!(shape("MB 12 34 at 0x5a"), "MB # # at #x#a");
        assert_eq!(shape(""), "");
        assert_ne!(shape("error A"), shape("error B"));
    }

    #[test]
    fn the_input_is_the_last_dash_i_argument() {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-i", "/media/a.mp4", "-f", "rawvideo", "pipe:1"]);
        assert_eq!(input_of(&cmd), "/media/a.mp4");
        cmd.args(["-i", "/media/b.mp4"]);
        assert_eq!(input_of(&cmd), "/media/b.mp4");
        assert_eq!(input_of(&Command::new("sleep")), "sleep");
        let mut dangling = Command::new("ffmpeg");
        dangling.arg("-i");
        assert_eq!(input_of(&dangling), "ffmpeg");
    }

    #[cfg(unix)]
    #[test]
    fn a_kept_tail_ends_with_its_child() {
        use std::process::Stdio;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo '[x @ 0x1] refused' >&2"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let tail = Arc::new(StderrTail::new());
        forward(&mut child, &cmd, Some(tail.clone())).unwrap();
        assert!(child.wait().unwrap().success());
        let got = tail.summary(Duration::from_secs(10), TAIL_FIRST);
        assert_eq!(got.as_deref(), Some("refused"));
        // Without a stderr pipe the tail ends at once, empty.
        let mut quiet = Command::new("true");
        let mut child = quiet.stderr(Stdio::null()).spawn().unwrap();
        let tail = Arc::new(StderrTail::new());
        forward(&mut child, &quiet, Some(tail.clone())).unwrap();
        child.wait().unwrap();
        assert_eq!(tail.summary(Duration::from_secs(10), TAIL_FIRST), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_childs_stderr_is_drained_until_it_exits() {
        use std::process::Stdio;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo oops >&2; echo again >&2"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        forward(&mut child, &cmd, None).unwrap();
        assert!(child.stderr.is_none(), "the drain owns the pipe");
        assert!(child.wait().unwrap().success());
        // Without a stderr pipe there is nothing to forward.
        let mut quiet = Command::new("true");
        let mut child = quiet.stderr(Stdio::null()).spawn().unwrap();
        forward(&mut child, &quiet, None).unwrap();
        child.wait().unwrap();
    }
}
